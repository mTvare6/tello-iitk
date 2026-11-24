use crate::{
    auth,
    model::{Action, Location, Page},
    navigation::Navigation,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub use crate::tracking_model::{SavedView, action_key, location_key};

#[derive(Default, Serialize, Deserialize)]
pub struct Preferences {
    pub username: String,
    pub save_password: bool,
}

pub fn root() -> Result<PathBuf> {
    let path = auth::session_path()?
        .parent()
        .context("No data directory")?
        .join("state");

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }

    builder.create(&path)?;

    let metadata = std::fs::symlink_metadata(&path)?;

    ensure!(
        metadata.is_dir(),
        "State directory must be a real directory"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "State directory must have permissions 700"
        );
    }

    Ok(path)
}

fn private(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        ensure!(metadata.is_file(), "State must be a regular file");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "State file must have permissions 600"
            );
        }

        ensure!(
            metadata.len() <= 64 * 1024 * 1024,
            "State file is too large"
        );
    }

    Ok(())
}

impl Preferences {
    pub fn load() -> Result<Self> {
        let path = root()?.join("desktop.json");
        private(&path)?;

        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid desktop preferences"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        let directory = root()?;

        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.as_file().sync_all()?;
        file.persist(directory.join("desktop.json"))
            .map_err(|e| e.error)?;

        Ok(())
    }
}

enum Command {
    Observe {
        key: String,
        entries: Vec<String>,
        opened: bool,
    },
    Read(String),
    Resume(i64, String),
    Stop,
}

pub struct Tracker {
    tx: mpsc::Sender<Command>,
    unread: Arc<Mutex<HashSet<String>>>,
    pub resume: Vec<SavedView>,
    pending: Option<(i64, String)>,
    last_json: String,
    last_sent: Instant,
    worker: Option<thread::JoinHandle<()>>,
}

fn unread(db: &Connection) -> Result<HashSet<String>> {
    let mut statement = db.prepare("WITH RECURSIVE added(entry) AS (SELECT entry FROM unread UNION SELECT known.page FROM known JOIN added ON known.entry=added.entry) SELECT entry FROM added")?;

    Ok(statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?)
}

impl Tracker {
    pub fn open(uid: &str, wake: impl Fn(Result<()>) + Send + 'static) -> Result<Self> {
        ensure!(
            !uid.is_empty() && uid.len() <= 32 && uid.bytes().all(|b| b.is_ascii_digit()),
            "Invalid account identity"
        );

        let path = root()?.join(format!("account-{uid}.sqlite"));
        private(&path)?;

        if !path.exists() {
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);

            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }

            match options.open(&path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }

        private(&path)?;

        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA journal_size_limit=4194304;
            CREATE TABLE IF NOT EXISTS pages(page TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS known(page TEXT,entry TEXT,PRIMARY KEY(page,entry));
            CREATE INDEX IF NOT EXISTS known_entry ON known(entry);
            CREATE TABLE IF NOT EXISTS unread(entry TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS resume(id INTEGER PRIMARY KEY,revision INTEGER,data TEXT);",
        )?;

        let last_json = db
            .query_row("SELECT data FROM resume WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;

        let resume: Vec<SavedView> = match last_json.as_deref() {
            Some(json) => serde_json::from_str(json).context("Invalid saved navigation")?,
            None => Vec::new(),
        };

        ensure!(resume.len() <= 32, "Saved navigation is too deep");

        let last_json = last_json.unwrap_or_else(|| "[]".into());

        let snapshot = Arc::new(Mutex::new(unread(&db)?));
        let shared = snapshot.clone();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(command) = rx.recv() {
                if matches!(command, Command::Stop) {
                    break;
                }

                let refresh = matches!(command, Command::Observe { .. } | Command::Read(_));
                let result = (|| -> Result<()> {
                    match command {
                        Command::Observe {
                            key,
                            entries,
                            opened,
                        } => {
                            let tx = db.transaction()?;
                            let baseline = tx
                                .query_row("SELECT 1 FROM pages WHERE page=?1", [&key], |_| Ok(()))
                                .optional()?
                                .is_some();
                            let inherited =
                                tx.query_row("SELECT 1 FROM unread WHERE entry=?1", [&key], |_| {
                                    Ok(())
                                })
                                .optional()?
                                .is_some();

                            let known = {
                                let mut statement =
                                    tx.prepare("SELECT entry FROM known WHERE page=?1")?;

                                statement
                                    .query_map([&key], |r| r.get::<_, String>(0))?
                                    .collect::<rusqlite::Result<HashSet<_>>>()?
                            };

                            tx.execute("INSERT OR REPLACE INTO pages VALUES(?1)", [&key])?;
                            tx.execute("DELETE FROM known WHERE page=?1", [&key])?;

                            for entry in entries.into_iter().take(20000) {
                                if (baseline || inherited) && !known.contains(&entry) {
                                    tx.execute(
                                        "INSERT OR IGNORE INTO unread VALUES(?1)",
                                        [&entry],
                                    )?;
                                }

                                tx.execute(
                                    "INSERT OR IGNORE INTO known VALUES(?1,?2)",
                                    params![key, entry],
                                )?;
                            }

                            if opened {
                                tx.execute("DELETE FROM unread WHERE entry=?1", [&key])?;
                            }

                            while tx.query_row("SELECT count(*) FROM known", [], |r| {
                                r.get::<_, i64>(0)
                            })? > 100000
                                || tx.query_row("SELECT count(*) FROM pages", [], |r| {
                                    r.get::<_, i64>(0)
                                })? > 512
                            {
                                let oldest: String = tx.query_row(
                                    "SELECT page FROM pages ORDER BY rowid LIMIT 1",
                                    [],
                                    |r| r.get(0),
                                )?;

                                tx.execute("DELETE FROM known WHERE page=?1", [&oldest])?;
                                tx.execute("DELETE FROM pages WHERE page=?1", [&oldest])?;
                            }

                            tx.execute("DELETE FROM unread WHERE NOT EXISTS(SELECT 1 FROM known WHERE known.entry=unread.entry)",[])?;
                            tx.commit()?;
                        }
                        Command::Read(key) => {
                            db.execute("DELETE FROM unread WHERE entry=?1", [key])?;
                        }
                        Command::Resume(revision, data) => {
                            db.execute("INSERT INTO resume VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,data=excluded.data WHERE excluded.revision>resume.revision",params![revision,data])?;
                        }
                        Command::Stop => {}
                    }

                    if refresh {
                        *shared.lock().unwrap_or_else(|e| e.into_inner()) = unread(&db)?;
                    }

                    Ok(())
                })();

                if refresh || result.is_err() {
                    wake(result);
                }
            }
        });

        Ok(Self {
            tx,
            unread: snapshot,
            resume,
            pending: None,
            last_json,
            last_sent: Instant::now() - Duration::from_secs(1),
            worker: Some(worker),
        })
    }

    pub fn observe(&self, location: &Location, page: &Page, opened: bool) {
        let _ = self.tx.send(Command::Observe {
            key: location_key(location),
            entries: page
                .entries
                .iter()
                .filter_map(|e| action_key(&e.action))
                .collect(),
            opened,
        });
    }

    pub fn is_new(&self, action: &Action) -> bool {
        action_key(action).is_some_and(|key| {
            self.unread
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&key)
        })
    }

    pub fn mark_read(&self, key: String) {
        self.unread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);

        let _ = self.tx.send(Command::Read(key));
    }

    pub fn home(&mut self) {
        let views = vec![SavedView {
            location: Location::Catalog("/courses".into()),
            selected: 0,
            filter: String::new(),
            scroll: 0,
            text_focus: false,
        }];

        if let Ok(json) = serde_json::to_string(&views) {
            self.last_json = json.clone();
            self.pending = Some((
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .min(i64::MAX as u128) as i64,
                json,
            ));

            self.flush();
        }
    }

    pub fn remember(&mut self, nav: &Navigation) {
        let views: Vec<SavedView> = nav
            .history
            .iter()
            .chain(nav.current.iter())
            .rev()
            .take(32)
            .map(|v| SavedView {
                location: v.location.clone(),
                selected: v.selected,
                filter: v.filter.clone(),
                scroll: v.scroll,
                text_focus: v.text_focus,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        if views.is_empty() {
            return;
        }

        if let Ok(json) = serde_json::to_string(&views)
            && json != self.last_json
        {
            let revision = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .min(i64::MAX as u128) as i64;

            self.last_json = json.clone();
            self.pending = Some((revision, json));
        }

        if self.last_sent.elapsed() >= Duration::from_millis(150) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if let Some((revision, json)) = self.pending.take() {
            let _ = self.tx.send(Command::Resume(revision, json));
            self.last_sent = Instant::now();
        }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.flush();

        let _ = self.tx.send(Command::Stop);

        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
