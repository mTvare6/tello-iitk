use crate::{
    auth::{self, ORIGIN},
    model::*,
};
use anyhow::{Context, Result, bail};
#[cfg(not(target_arch = "wasm32"))]
use reqwest::Client;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use url::Url;
#[cfg(not(target_arch = "wasm32"))]
use {std::time::Duration, tokio::io::AsyncWriteExt};

#[cfg(not(target_arch = "wasm32"))]
const RESPONSE_LIMIT: usize = 32 * 1024 * 1024;
const DOWNLOAD_LIMIT: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct DownloadTarget {
    pub course: Course,
    pub file: Attachment,
}

#[derive(Default, Debug)]
pub struct DownloadPlan {
    pub files: Vec<DownloadTarget>,
    pub skipped: Vec<String>,
}

#[derive(Clone)]
pub struct Api {
    #[cfg(not(target_arch = "wasm32"))]
    client: Client,
    cache: Arc<Mutex<ApiCache>>,
}

#[derive(Default)]
struct ApiCache {
    generation: u64,
    entries: HashMap<String, Value>,
    bytes: usize,
}

impl ApiCache {
    fn insert(&mut self, generation: u64, path: String, value: Value, bytes: usize) {
        if self.generation != generation {
            return;
        }

        if self.bytes + bytes > 64 * 1024 * 1024 || self.entries.len() >= 128 {
            self.entries.clear();
            self.bytes = 0;
        }

        // Another request may have cached this while we were fetching it.
        if let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(path) {
            entry.insert(value);
            self.bytes += bytes;
        }
    }
}

impl Api {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(session: &Path) -> Result<Self> {
        let client = Client::builder()
            .cookie_provider(auth::load(session)?)
            .user_agent("hello-iitk-tui/0.1 (read-only course client)")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(40))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 8 {
                    attempt.error("Too many redirects")
                } else if !same_origin(attempt.url()) {
                    attempt.error("Cross-origin service redirect refused")
                } else {
                    attempt.follow()
                }
            }))
            .build()?;

        Ok(Self {
            client,
            cache: Arc::default(),
        })
    }

    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        cache.generation = cache.generation.wrapping_add(1);
        cache.entries.clear();
        cache.bytes = 0;
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn get(&self, path: &str) -> Result<Vec<u8>> {
        let url = Url::parse(ORIGIN)?.join(path)?;

        if !same_origin(&url) {
            bail!("Service URL must stay on HelloIITK");
        }

        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Request failed: {}", e.without_url()))?;

        check_status(response.status().as_u16())?;

        let mut body = Vec::new();

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| anyhow::anyhow!("Response read failed: {}", e.without_url()))?
        {
            if body.len() + chunk.len() > RESPONSE_LIMIT {
                bail!("API response exceeds 32 MiB");
            }

            body.extend_from_slice(&chunk);
        }

        reject_login(&body)?;

        Ok(body)
    }

    async fn json(&self, course: &Course, endpoint: &str) -> Result<Value> {
        validate_course_id(&course.id)?;

        let path = format!("/studentapi/{}/{endpoint}", course.id);
        let generation = {
            let cache = self.cache.lock().await;

            if let Some(v) = cache.entries.get(&path) {
                return Ok(v.clone());
            }

            cache.generation
        };

        let bytes = self.get(&path).await?;
        let value: Value = serde_json::from_slice(&bytes)
            .context("Expected JSON from course API; session may have expired")?;

        if value.get("message").and_then(Value::as_str) != Some("Success") {
            bail!("API did not report Success; session or access may have changed");
        }

        self.cache
            .lock()
            .await
            .insert(generation, path, value.clone(), bytes.len());

        Ok(value)
    }

    pub async fn items(&self, c: &Course, s: Section) -> Result<Vec<Item>> {
        parse_items(self.json(c, s.endpoint()).await?, s == Section::Handout)
    }

    pub async fn downloadables(&self, location: &Location) -> Result<DownloadPlan> {
        collect_downloads(
            location.clone(),
            |node| async move { self.load(&node).await },
        )
        .await
    }

    pub async fn load(&self, location: &Location) -> Result<Page> {
        match location {
            Location::Catalog(path) => {
                if !(path == "/courses"
                    || path.starts_with("/courses/")
                    || path.starts_with("/courses?"))
                {
                    bail!("Invalid catalog path");
                }

                let body = self.get(path).await?;

                parse_catalog(std::str::from_utf8(&body)?, path)
            }
            Location::Course(c) => {
                let permissions = self.json(c, "user_permissions/allowed").await?;
                let permissions = permissions
                    .pointer("/data/permissions")
                    .context("Missing course permissions")?;
                let entries = Section::ALL
                    .into_iter()
                    .filter(|s| {
                        permissions
                            .get(s.permission())
                            .and_then(|p| p.get("viewEntity"))
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                            > 0
                    })
                    .map(|s| Entry {
                        title: s.label().into(),
                        subtitle: "Open section".into(),
                        action: Action::Navigate(Location::Collection(c.clone(), s)),
                    })
                    .collect();

                Ok(Page {
                    title: c.title.clone(),
                    text: String::new(),
                    entries,
                })
            }
            Location::Collection(c, s) => {
                let items = self.items(c, *s).await?;

                if *s == Section::Handout {
                    return items.first().context("No course handout").map(Item::page);
                }

                let entries = if *s == Section::Lectures {
                    let mut weeks: Vec<(String, usize)> = Vec::new();

                    for item in items {
                        let label = item.week_label();

                        if let Some(w) = weeks.iter_mut().find(|w| w.0 == label) {
                            w.1 += 1;
                        } else {
                            weeks.push((label, 1));
                        }
                    }

                    weeks
                        .into_iter()
                        .map(|(w, n)| Entry {
                            title: clean(&w),
                            subtitle: format!("{n} lectures"),
                            action: Action::Navigate(Location::Week(c.clone(), w)),
                        })
                        .collect()
                } else {
                    items
                        .into_iter()
                        .map(|item| item_entry(c, *s, item))
                        .collect()
                };

                Ok(Page {
                    title: s.label().into(),
                    text: String::new(),
                    entries,
                })
            }
            Location::Week(c, week) => {
                let entries = self
                    .items(c, Section::Lectures)
                    .await?
                    .into_iter()
                    .filter(|i| &i.week_label() == week)
                    .map(|i| item_entry(c, Section::Lectures, i))
                    .collect();

                Ok(Page {
                    title: clean(week),
                    text: String::new(),
                    entries,
                })
            }
            Location::Item(c, s, id) => self
                .items(c, *s)
                .await?
                .into_iter()
                .find(|i| &i.key() == id)
                .context("Item is no longer available; refresh its section")
                .map(|i| i.page()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn download(
        &self,
        attachment: &Attachment,
        course: &Course,
        directory: &Path,
    ) -> Result<PathBuf> {
        validate_course_id(&course.id)?;

        let url = Url::parse(ORIGIN)?.join(&attachment.url)?;

        if !same_origin(&url)
            || !(url.path().starts_with(&format!("/manage/{}/", course.id))
                || url.path().starts_with("/sites/default/files/"))
        {
            bail!("Unrecognized attachment origin/path; download through the website");
        }

        let mut response = self
            .client
            .get(url)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Download request failed: {}", e.without_url()))?;

        check_status(response.status().as_u16())?;

        if response.url().path().starts_with("/user/")
            || response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h.contains("text/html"))
        {
            bail!("Download returned an HTML page; authentication or file access may be required");
        }

        if response.content_length().unwrap_or(0) > DOWNLOAD_LIMIT {
            bail!("File exceeds 512 MiB download limit");
        }

        prepare_download_dir(directory)?;

        let name = format!(
            "{}-{}-{}",
            &course.id[..course.id.len().min(48)],
            attachment.id,
            safe_filename(&attachment.filename)
        );

        let mut index = 0;

        let (path, mut file) = loop {
            let path = directory.join(if index == 0 {
                name.clone()
            } else {
                format!("{index}-{name}")
            });

            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);

            match options.open(&path).await {
                Ok(f) => break (path, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    index += 1;
                }
                Err(e) => return Err(e.into()),
            }
        };

        let mut cleanup = PartialDownload {
            path: path.clone(),
            committed: false,
        };
        let outcome: Result<()> = async {
            let mut total = 0u64;

            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| anyhow::anyhow!("Download interrupted: {}", e.without_url()))?
            {
                if total == 0 {
                    reject_login(&chunk)?;
                }

                total += chunk.len() as u64;

                if total > DOWNLOAD_LIMIT {
                    bail!("File exceeds 512 MiB limit");
                }

                file.write_all(&chunk).await?;
            }

            if total == 0 {
                bail!("Download returned an empty file");
            }

            file.flush().await?;

            Ok(())
        }
        .await;

        if let Err(e) = outcome {
            drop(file);

            return Err(e);
        }

        cleanup.committed = true;

        Ok(path)
    }
}

/// Collect files within the selected semester.
async fn collect_downloads<F, Fut>(root: Location, mut load: F) -> Result<DownloadPlan>
where
    F: FnMut(Location) -> Fut,
    Fut: std::future::Future<Output = Result<Page>>,
{
    let mut pending = vec![root.clone()];
    let mut visited = HashSet::new();
    let mut seen_files = HashSet::new();
    let mut plan = DownloadPlan::default();

    while let Some(location) = pending.pop() {
        if !visited.insert(location.clone()) {
            continue;
        }

        let page = match load(location.clone()).await {
            Ok(page) => page,
            Err(e) if location == root || e.is::<auth::AccessDenied>() => return Err(e),
            Err(e) => {
                plan.skipped.push(format!("{}: {e}", location.label()));
                continue;
            }
        };

        for entry in page.entries.into_iter().rev() {
            match entry.action {
                Action::File(file) => {
                    if let Some(course) = location.course() {
                        let mut url = Url::parse(ORIGIN)?.join(&file.url)?;
                        url.set_fragment(None);

                        if seen_files.insert(url.to_string()) {
                            plan.files.push(DownloadTarget {
                                course: course.clone(),
                                file,
                            });
                        }
                    }
                }
                Action::Navigate(next) => {
                    let follow = match (&location, &next) {
                        (Location::Catalog(current), Location::Catalog(next)) => {
                            let base = Url::parse(ORIGIN)?;

                            base.join(current)?.path() == base.join(next)?.path()
                        }
                        (_, Location::Catalog(_)) => false,
                        _ => true,
                    };

                    if follow {
                        pending.push(next);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(plan)
}

#[cfg(not(target_arch = "wasm32"))]
struct PartialDownload {
    path: PathBuf,
    committed: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for PartialDownload {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn item_entry(c: &Course, s: Section, item: Item) -> Entry {
    Entry {
        title: clean(&item.title),
        subtitle: item.summary(),
        action: Action::Navigate(Location::Item(c.clone(), s, item.key())),
    }
}

pub fn same_origin(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("hello.iitk.ac.in")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn check_status(status: u16) -> Result<()> {
    if matches!(status, 401 | 403) {
        return Err(auth::AccessDenied.into());
    }

    if !(200..300).contains(&status) {
        bail!("HelloIITK returned HTTP {}", status);
    }

    Ok(())
}

pub fn reject_login(body: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(body);

    if text.contains("user-login-form") || text.contains("Access to this page is restricted") {
        return Err(auth::AccessDenied.into());
    }

    Ok(())
}

pub fn safe_filename(name: &str) -> String {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 16
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
        });

    let mut safe = String::new();

    for c in name.chars() {
        let c = if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
            c
        } else {
            '_'
        };

        if safe.len() + c.len_utf8() > 150 {
            if let Some(extension) = extension {
                while safe.len() + 1 + extension.len() > 150 {
                    safe.pop();
                }

                safe.push('.');
                safe.push_str(extension);
            }

            break;
        }

        safe.push(c);
    }

    let name = safe;

    if name.is_empty() || name == "." || name == ".." {
        "attachment".into()
    } else {
        name
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn prepare_download_dir(directory: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }

    builder.create(directory)?;

    Ok(())
}

#[cfg(target_arch = "wasm32")]
impl Api {
    pub fn new(session: &Path) -> Result<Self> {
        auth::identity(session)?;

        Ok(Self {
            cache: Arc::default(),
        })
    }

    async fn get(&self, path: &str) -> Result<Vec<u8>> {
        let response = crate::browser::http::request(path, None).await?;
        check_status(response.status)?;
        reject_login(&response.body)?;

        Ok(response.body)
    }

    pub async fn download(
        &self,
        attachment: &Attachment,
        course: &Course,
        _directory: &Path,
    ) -> Result<PathBuf> {
        validate_course_id(&course.id)?;
        let url = Url::parse(ORIGIN)?.join(&attachment.url)?;

        if !same_origin(&url)
            || !(url.path().starts_with(&format!("/manage/{}/", course.id))
                || url.path().starts_with("/sites/default/files/"))
        {
            bail!("Unrecognized attachment origin/path");
        }

        let name = format!(
            "{}-{}-{}",
            &course.id[..course.id.len().min(48)],
            attachment.id,
            safe_filename(&attachment.filename)
        );
        crate::browser::http::download(&url, name, DOWNLOAD_LIMIT).await
    }
}
