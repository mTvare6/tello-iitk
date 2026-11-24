use crate::input::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crate::runtime::{self, Instant, sync::mpsc};
use crate::{
    api::{Api, DownloadTarget},
    auth, login,
    model::*,
    navigation::Navigation,
    text::{Cursor, Edit},
    tracking, vault,
};
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use crossterm::event::{self, Event};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{path::PathBuf, time::Duration};

const BG: Color = Color::Rgb(15, 20, 30);
const PANEL: Color = Color::Rgb(22, 29, 42);
const INK: Color = Color::Rgb(222, 231, 242);
const MUTED: Color = Color::Rgb(144, 161, 183);
const EDGE: Color = Color::Rgb(57, 74, 96);
const CYAN: Color = Color::Rgb(99, 218, 233);
const GREEN: Color = Color::Rgb(139, 219, 164);
const GOLD: Color = Color::Rgb(250, 207, 112);
const PURPLE: Color = Color::Rgb(199, 160, 255);
const RED: Color = Color::Rgb(255, 142, 151);

enum Completed {
    Page(Location, Page, bool),
    DownloadProgress {
        done: usize,
        total: usize,
        failed: usize,
        saved: Option<(String, PathBuf)>,
    },
    DownloadsFinished {
        done: usize,
        total: usize,
        failed: usize,
        error: Option<String>,
        open: Option<PathBuf>,
        expired: bool,
    },
    Preview {
        id: u64,
        location: Location,
        result: Result<Page>,
    },
    Authenticated(Api, Option<(String, zeroize::Zeroizing<String>)>),
    Restored(Vec<crate::navigation::View>, Option<String>),
    Tracking(Result<()>),
    Credential {
        id: u64,
        username: String,
        result: Result<CredentialReply>,
    },
    LoginForm(Box<login::Challenge>, Option<&'static str>),
    Suggestion(Option<String>),
}

enum CredentialReply {
    Loaded(Option<zeroize::Zeroizing<String>>),
    Saved,
    Forgotten,
}

#[derive(Clone, Debug)]
struct Jump {
    targets: Vec<(String, usize)>,
    prefix: String,
}

impl Jump {
    fn new(rows: &[usize]) -> Self {
        const LETTERS: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";

        let mut width = 1;
        let mut capacity = LETTERS.len();

        while capacity < rows.len() {
            width += 1;
            capacity *= LETTERS.len();
        }

        let targets = rows
            .iter()
            .enumerate()
            .map(|(mut n, index)| {
                let mut label = vec![b'a'; width];

                for c in label.iter_mut().rev() {
                    *c = LETTERS[n % LETTERS.len()];
                    n /= LETTERS.len();
                }

                (String::from_utf8(label).unwrap(), *index)
            })
            .collect();

        Self {
            targets,
            prefix: String::new(),
        }
    }

    fn label(&self, index: usize) -> &str {
        self.targets
            .iter()
            .find(|(_, i)| *i == index)
            .map(|(s, _)| s.as_str())
            .unwrap_or("")
    }
}

struct Hint {
    key: String,
    action: String,
}

fn hint(key: &str, action: &str) -> Hint {
    Hint {
        key: key.into(),
        action: action.into(),
    }
}

pub(crate) enum BrowserEffect {
    OpenUrl(String),
    OpenFile(PathBuf),
    DownloadFiles(String, Vec<PathBuf>),
    Image(Vec<u8>),
}

struct App {
    browser: bool,
    tracker: Option<tracking::Tracker>,
    home_requested: bool,
    remember_password: bool,
    credential_job: Option<runtime::task::JoinHandle<()>>,
    credential_id: u64,
    notice: String,
    notice_error: bool,
    browser_files: Vec<PathBuf>,
    browser_effects: Vec<BrowserEffect>,
    login_hitboxes: Vec<(Rect, usize)>,
    api: Option<Api>,
    session: PathBuf,
    login_screen: bool,
    authenticating: bool,
    login_form: Option<Box<login::Challenge>>,
    login_fields: login::Fields,
    show_captcha: bool,
    needs_login_form: bool,
    nav: Navigation,
    busy: bool,
    status: String,
    status_error: bool,
    help: bool,
    help_scroll: u16,
    help_max: u16,
    searching: bool,
    search_cursor: Cursor,
    text_focus: bool,
    jump: Option<Jump>,
    pending_g: bool,
    download_dir: PathBuf,
    pending: Option<runtime::task::JoinHandle<()>>,
    solver: Option<runtime::task::JoinHandle<()>>,
    downloader: Option<runtime::task::JoinHandle<()>>,
    downloading: bool,
    opening_read: Option<String>,
    download_id: u64,
    download_status: String,
    download_name: String,
    page_cache: std::collections::HashMap<Location, Page>,
    preview_task: Option<runtime::task::JoinHandle<()>>,
    preview_id: u64,
    preview_target: Option<Location>,
    preview_page: Option<Page>,
    preview_error: Option<String>,
    preview_enabled: bool,
    preview_rendered: String,
    preview_width: u16,
    tx: mpsc::UnboundedSender<(u64, Result<Completed>)>,
    rendered: String,
    render_width: u16,
    generation: u64,
    opened: std::collections::HashMap<String, PathBuf>,
    started: Instant,
    list_state: ListState,
    visible_rows: Vec<usize>,
    list_hitboxes: Vec<(Rect, usize)>,
}

impl App {
    fn new(
        session: PathBuf,
        download_dir: PathBuf,
        tx: mpsc::UnboundedSender<(u64, Result<Completed>)>,
    ) -> Result<Self> {
        let (api, status) = match Api::new(&session) {
            Ok(api) => (Some(api), String::new()),
            Err(e) => (
                None,
                if runtime::file_exists(&session) {
                    clean(&format!("{e:#}"))
                } else {
                    String::new()
                },
            ),
        };

        let preferences = tracking::Preferences::load()?;

        let mut login_fields = login::Fields {
            username: auth::identity(&session)
                .ok()
                .map(|(_, name)| name)
                .filter(|s| !s.is_empty())
                .unwrap_or(preferences.username),
            ..Default::default()
        };

        login_fields.cursors[0].position = login_fields.username.chars().count();

        let mut app = Self {
            browser: false,
            tracker: None,
            home_requested: false,
            remember_password: preferences.save_password,
            credential_job: None,
            credential_id: 0,
            notice: String::new(),
            notice_error: false,
            browser_files: Vec::new(),
            browser_effects: Vec::new(),
            login_hitboxes: Vec::new(),
            login_screen: api.is_none(),
            needs_login_form: api.is_none(),
            api,
            session,
            authenticating: false,
            login_form: None,
            login_fields,
            show_captcha: false,
            nav: Navigation::default(),
            busy: false,
            status,
            status_error: false,
            help: false,
            help_scroll: 0,
            help_max: 0,
            searching: false,
            search_cursor: Default::default(),
            text_focus: false,
            jump: None,
            pending_g: false,
            download_dir,
            pending: None,
            solver: None,
            downloader: None,
            downloading: false,
            opening_read: None,
            download_id: 0,
            download_status: String::new(),
            download_name: String::new(),
            page_cache: Default::default(),
            preview_task: None,
            preview_id: 0,
            preview_target: None,
            preview_page: None,
            preview_error: None,
            preview_enabled: false,
            preview_rendered: String::new(),
            preview_width: 0,
            tx,
            rendered: String::new(),
            render_width: 0,
            generation: 0,
            opened: Default::default(),
            started: Instant::now(),
            list_state: ListState::default(),
            visible_rows: Vec::new(),
            list_hitboxes: Vec::new(),
        };

        app.attach_tracker();

        Ok(app)
    }

    fn attach_tracker(&mut self) {
        self.tracker = None;

        if let Ok((uid, _)) = auth::identity(&self.session) {
            let tx = self.tx.clone();

            match tracking::Tracker::open(&uid, move |result| {
                let _ = tx.send((0, Ok(Completed::Tracking(result))));
            }) {
                Ok(tracker) => self.tracker = Some(tracker),
                Err(error) => {
                    self.notice = format!("Browsing state unavailable: {error}");
                    self.notice_error = true;
                }
            }
        }
    }

    fn remember_view(&mut self) {
        if self.login_screen {
            return;
        }

        if let Some(view) = &mut self.nav.current {
            view.text_focus = self.text_focus;
        }

        if let Some(tracker) = &mut self.tracker {
            tracker.remember(&self.nav);
        }
    }

    fn start_location(&mut self) {
        let saved = self
            .tracker
            .as_ref()
            .map(|t| t.resume.clone())
            .unwrap_or_default();

        if std::mem::take(&mut self.home_requested) || saved.is_empty() {
            self.load(Location::Catalog("/courses".into()), false);

            return;
        }

        let Some(api) = self.api.clone() else {
            return;
        };
        self.cancel_pending();
        self.generation += 1;

        let generation = self.generation;
        self.busy = true;
        self.login_screen = false;
        self.message("Restoring location…", false);

        let tx = self.tx.clone();
        self.pending = Some(runtime::spawn(async move {
            let result = async {
                let mut views = Vec::new();
                let mut warning = None;

                for saved in saved {
                    match api.load(&saved.location).await {
                        Ok(page) => views.push(saved.apply(page)),
                        Err(error) if error.is::<auth::AccessDenied>() => return Err(error),
                        Err(_) => {
                            warning =
                                Some("Last location is unavailable; restored its parent.".into());
                            break;
                        }
                    }
                }

                if views.is_empty() {
                    views.push(crate::navigation::View::new(
                        Location::Catalog("/courses".into()),
                        api.load(&Location::Catalog("/courses".into())).await?,
                    ));
                }

                Ok(Completed::Restored(views, warning))
            }
            .await;

            let _ = tx.send((generation, result));
        }));
    }

    fn home(&mut self) {
        self.remember_view();
        self.cancel_pending();
        self.clear_preview();
        self.help = false;
        self.searching = false;
        self.jump = None;
        self.pending_g = false;
        self.home_requested = self.api.is_none();

        if let Some(tracker) = &mut self.tracker {
            tracker.home();
        }

        self.nav = Navigation::default();

        if self.api.is_some() {
            self.load(Location::Catalog("/courses".into()), false);
        } else {
            self.login_screen = true;
            self.login_fields.clear_secrets();
            self.login_form = None;
            self.needs_login_form = true;
            self.busy = false;
            self.authenticating = false;
            self.message("Sign in to open Courses.", false);
        }
    }

    fn start_credential(&mut self, kind: &str, password: Option<zeroize::Zeroizing<String>>) {
        if self.browser {
            return;
        }

        if let Some(task) = self.credential_job.take() {
            task.abort();
        }

        self.credential_id += 1;

        let id = self.credential_id;
        let username = self.login_fields.username.clone();
        let tx = self.tx.clone();
        let kind = kind.to_owned();
        self.credential_job = Some(runtime::spawn(async move {
            let result = match kind.as_str() {
                "load" => vault::load(&username).await.map(CredentialReply::Loaded),
                "save" => vault::save(
                    &username,
                    password.as_deref().map(|s| s.as_str()).unwrap_or(""),
                )
                .await
                .map(|_| CredentialReply::Saved),
                _ => vault::forget(&username)
                    .await
                    .map(|_| CredentialReply::Forgotten),
            };

            let _ = tx.send((
                0,
                Ok(Completed::Credential {
                    id,
                    username,
                    result,
                }),
            ));
        }));
    }

    fn maybe_load_password(&mut self) {
        if !self.browser
            && self.remember_password
            && self.login_fields.password.is_empty()
            && !self.login_fields.username.is_empty()
            && self.credential_job.is_none()
        {
            self.start_credential("load", None);
        }
    }

    fn mark_read(&self, action: &Action) {
        if let Some(tracker) = &self.tracker
            && let Some(key) = tracking::action_key(action)
        {
            tracker.mark_read(key);
        }
    }

    fn message(&mut self, s: impl Into<String>, error: bool) {
        self.status = s.into();
        self.status_error = error;
    }

    fn reset_view(&mut self) {
        self.clear_preview();
        self.render_width = 0;
        self.list_state = ListState::default();
        self.jump = None;
        self.pending_g = false;
        self.text_focus = self.nav.current.as_ref().is_some_and(|v| v.text_focus);
    }

    fn load(&mut self, location: Location, refresh: bool) {
        self.remember_view();

        let Some(api) = self.api.clone() else {
            self.login_screen = true;

            return;
        };

        self.cancel_pending();
        self.clear_preview();

        if refresh {
            self.page_cache.clear();
            self.opened.clear();
        }

        self.generation += 1;

        let generation = self.generation;

        if !refresh && let Some(page) = self.page_cache.get(&location).cloned() {
            self.complete(generation, Ok(Completed::Page(location, page, false)));

            return;
        }

        self.busy = true;
        self.jump = None;
        self.pending_g = false;
        self.login_screen = false;
        self.message("Loading…", false);

        let tx = self.tx.clone();
        self.pending = Some(runtime::spawn(async move {
            if refresh {
                api.clear_cache().await;
            }

            let result = api
                .load(&location)
                .await
                .map(|page| Completed::Page(location, page, refresh));
            let _ = tx.send((generation, result));
        }));
    }

    fn cancel_pending(&mut self) -> bool {
        if let Some(solver) = self.solver.take() {
            solver.abort();
        }

        if let Some(task) = self.pending.take() {
            task.abort();
            self.generation += 1;
            self.busy = false;
            self.authenticating = false;

            true
        } else {
            false
        }
    }

    fn prepare_login(&mut self) {
        if self.busy {
            return;
        }

        if let Some(solver) = self.solver.take() {
            solver.abort();
        }

        self.cancel_downloads();
        self.clear_preview();
        self.needs_login_form = false;
        self.login_screen = true;
        self.login_form = None;
        self.login_fields.set_captcha(String::new());
        self.busy = true;
        self.authenticating = false;
        self.generation += 1;

        let generation = self.generation;
        self.message("Loading…", false);
        self.maybe_load_password();

        let tx = self.tx.clone();
        self.pending = Some(runtime::spawn(async move {
            let result = login::Challenge::fetch()
                .await
                .map(|f| Completed::LoginForm(Box::new(f), None));
            let _ = tx.send((generation, result));
        }));
    }

    fn move_login_focus(&mut self, backwards: bool) {
        let fields: &[usize] = if self.show_captcha {
            &[0, 1, 2, 3]
        } else {
            &[0, 1, 3]
        };

        let current = fields
            .iter()
            .position(|i| *i == self.login_fields.focus)
            .unwrap_or(0);
        self.login_fields.focus = fields[if backwards {
            (current + fields.len() - 1) % fields.len()
        } else {
            (current + 1) % fields.len()
        }];
    }

    fn submit_login(&mut self) {
        if self.busy {
            return;
        }

        if self.login_fields.username.is_empty() {
            self.login_fields.focus = 0;
            self.message("Enter your username.", true);

            return;
        }

        if self.login_fields.password.is_empty() {
            self.login_fields.focus = 1;
            self.message("Enter your password.", true);

            return;
        }

        if self.login_form.as_ref().is_some_and(|f| f.has_captcha())
            && self.login_fields.captcha.is_empty()
        {
            if self.solver.is_some() {
                self.message("Reading login challenge…", false);

                return;
            }

            self.show_captcha = true;
            self.login_fields.focus = 2;
            self.message(
                "Could not read CAPTCHA. Enter the characters or F2 for a new image.",
                true,
            );

            return;
        }

        let Some(form) = self.login_form.take() else {
            self.prepare_login();

            return;
        };

        if let Some(solver) = self.solver.take() {
            solver.abort();
        }

        let saved_password = if !self.browser && self.remember_password {
            Some((
                self.login_fields.username.clone(),
                self.login_fields.password.clone(),
            ))
        } else {
            None
        };

        let credentials = self.login_fields.take();
        let session = self.session.clone();
        self.busy = true;
        self.authenticating = true;
        self.generation += 1;

        let generation = self.generation;
        self.message("Signing in…", false);

        let tx = self.tx.clone();
        self.pending = Some(runtime::spawn(async move {
            let result = async {
                match form.submit(credentials, &session).await? {
                    login::Outcome::Authenticated => Ok(Completed::Authenticated(
                        Api::new(&session)?,
                        saved_password,
                    )),
                    login::Outcome::Rejected(form, reason) => {
                        Ok(Completed::LoginForm(form, Some(reason)))
                    }
                }
            }
            .await;

            let _ = tx.send((generation, result));
        }));
    }

    fn complete(&mut self, generation: u64, result: Result<Completed>) {
        match result {
            Ok(Completed::Tracking(result)) => {
                if let Err(error) = result {
                    self.notice = format!("Could not save browsing state: {error}");
                    self.notice_error = true;
                }
            }
            Ok(Completed::Credential {
                id,
                username,
                result,
            }) => {
                if id != self.credential_id {
                    return;
                }

                self.credential_job = None;

                match result {
                    Ok(CredentialReply::Loaded(Some(password)))
                        if !self.browser
                            && self.login_screen
                            && !self.authenticating
                            && self.remember_password
                            && self.login_fields.username == username
                            && self.login_fields.password.is_empty() =>
                    {
                        self.login_fields.cursors[1].position = password.chars().count();
                        self.login_fields.password = password;
                    }
                    Ok(CredentialReply::Loaded(_)) => {}
                    Ok(CredentialReply::Saved) => {
                        self.notice = "Password saved in desktop keyring.".into();
                        self.notice_error = false;
                    }
                    Ok(CredentialReply::Forgotten) => {
                        self.notice = "Saved password removed from desktop keyring.".into();
                        self.notice_error = false;
                    }
                    Err(error) => {
                        self.notice = error.to_string();
                        self.notice_error = true;
                    }
                }
            }
            Ok(Completed::Preview {
                id,
                location,
                result,
            }) => {
                if id == self.preview_id && self.preview_target.as_ref() == Some(&location) {
                    self.preview_task = None;
                    self.preview_width = 0;

                    match result {
                        Ok(page) => {
                            if let Some(tracker) = &self.tracker {
                                tracker.observe(&location, &page, false);
                            }

                            self.page_cache.insert(location, page.clone());
                            self.preview_page = Some(page);
                            self.preview_error = None;
                        }
                        Err(e) if e.is::<auth::AccessDenied>() => {
                            self.complete_request(self.generation, Err(e));
                        }
                        Err(e) => self.preview_error = Some(clean(&format!("{e:#}"))),
                    }
                }
            }
            Ok(Completed::DownloadProgress {
                done,
                total,
                failed,
                saved,
            }) => {
                if generation != self.download_id || !self.downloading {
                    return;
                }

                if let Some((url, path)) = saved {
                    if self.browser {
                        self.browser_files.push(path.clone());
                    }

                    self.opened.insert(url, path);
                }

                self.download_status = format!(
                    "{done}/{total} downloaded{}",
                    if failed > 0 {
                        format!(" · {failed} failed")
                    } else {
                        String::new()
                    }
                );
            }
            Ok(Completed::DownloadsFinished {
                done,
                total,
                failed,
                error,
                open,
                expired,
            }) => {
                if generation != self.download_id || !self.downloading {
                    return;
                }

                self.downloading = false;
                self.downloader = None;
                self.download_status.clear();
                self.message(
                    if total == 0 && failed == 0 {
                        "No downloadable files.".into()
                    } else {
                        format!(
                            "{done}/{total} downloaded{}{}",
                            if failed > 0 {
                                format!(" · {failed} failed")
                            } else {
                                String::new()
                            },
                            error.map(|e| format!(" · {e}")).unwrap_or_default()
                        )
                    },
                    failed > 0,
                );

                if let Some(key) = self.opening_read.take()
                    && open.is_some()
                    && let Some(tracker) = &self.tracker
                {
                    tracker.mark_read(key);
                }

                if self.browser {
                    if let Some(path) = open {
                        self.browser_effects.push(BrowserEffect::OpenFile(path));
                    } else if !self.browser_files.is_empty() {
                        self.browser_effects.push(BrowserEffect::DownloadFiles(
                            self.download_name.clone(),
                            std::mem::take(&mut self.browser_files),
                        ));
                    }

                    self.browser_files.clear();
                } else if let Some(path) = open
                    && runtime::open(&path).is_err()
                {
                    self.message(
                        format!("Saved {} · could not open viewer", path.display()),
                        true,
                    );
                }

                if expired {
                    self.cancel_pending();
                    self.clear_preview();
                    self.complete(self.generation, Err(auth::AccessDenied.into()));
                }
            }
            other => self.complete_request(generation, other),
        }
    }

    fn complete_request(&mut self, generation: u64, result: Result<Completed>) {
        if generation != 0 && generation != self.generation {
            return;
        }

        if let Ok(Completed::Suggestion(code)) = &result {
            let active = self.solver.take().is_some();

            if active
                && self.login_screen
                && !self.authenticating
                && self.login_form.is_some()
                && self.login_fields.captcha.is_empty()
            {
                if let Some(code) = code {
                    self.login_fields.set_captcha(code.clone());

                    if !self.status_error {
                        self.message(
                            if self.show_captcha {
                                "Check the suggested code."
                            } else {
                                ""
                            },
                            false,
                        );
                    }
                } else {
                    self.show_captcha = true;

                    if self.login_fields.focus == 3 {
                        self.login_fields.focus = 2;
                    }

                    self.message(
                        "Could not read CAPTCHA. Enter the characters or F2 for a new image.",
                        true,
                    );
                }
            }

            return;
        }

        self.busy = false;
        self.authenticating = false;
        self.pending = None;

        match result {
            Ok(Completed::Authenticated(api, saved_password)) => {
                self.show_captcha = false;
                self.page_cache.clear();
                self.login_fields.clear_secrets();
                self.login_form = None;
                self.api = Some(api);
                self.nav = Navigation::default();
                self.searching = false;
                self.reset_view();
                self.attach_tracker();

                if !self.browser {
                    let preferences = tracking::Preferences {
                        username: self.login_fields.username.clone(),
                        save_password: self.remember_password,
                    };

                    if let Err(error) = preferences.save() {
                        self.notice = error.to_string();
                        self.notice_error = true;
                    }
                }

                self.start_location();

                if let Some((_, password)) = saved_password {
                    self.start_credential("save", Some(password));
                }
            }
            Ok(Completed::Restored(mut views, warning)) => {
                self.nav.current = views.pop();
                self.nav.history = views;

                for view in self.nav.history.iter().chain(self.nav.current.iter()) {
                    self.page_cache
                        .insert(view.location.clone(), view.page.clone());

                    if let Some(tracker) = &self.tracker {
                        tracker.observe(&view.location, &view.page, true);
                    }
                }

                self.reset_view();
                self.message(warning.unwrap_or_default(), false);
                self.remember_view();
            }
            Ok(Completed::LoginForm(form, rejected)) => {
                self.login_form = Some(form);

                if let Some(reason) = rejected {
                    self.show_captcha |= reason == login::CAPTCHA_REJECTED;
                    self.login_fields.clear_secrets();
                    self.login_fields.focus = 1;
                    self.message(reason, true);
                } else {
                    self.message("", false);
                }

                self.suggest_code();
            }
            Ok(Completed::Suggestion(_)) => {}
            Ok(Completed::Page(location, page, replace)) => {
                if let Some(tracker) = &self.tracker {
                    tracker.observe(&location, &page, true);
                }

                self.page_cache.insert(location.clone(), page.clone());

                if replace && let Some(v) = &mut self.nav.current {
                    let filter = v.filter.clone();
                    v.page = page;
                    v.set_filter(filter);
                    v.scroll = 0;
                } else {
                    self.nav.push(location, page);
                }

                self.reset_view();
                self.message("", false);
            }
            Ok(
                Completed::Preview { .. }
                | Completed::DownloadProgress { .. }
                | Completed::DownloadsFinished { .. }
                | Completed::Tracking(_)
                | Completed::Credential { .. },
            ) => {}
            Err(e) => {
                if e.is::<auth::AccessDenied>() {
                    self.cancel_downloads();
                    self.page_cache.clear();
                    self.api = None;
                    self.show_captcha = false;
                    self.nav = Navigation::default();
                    self.login_screen = true;
                    self.needs_login_form = true;
                    self.searching = false;
                    self.reset_view();
                    self.message("Session expired or access denied. Sign in again.", true);
                } else {
                    self.message(clean(&format!("{e:#}")), true);
                }
            }
        }
    }

    fn suggest_code(&mut self) {
        if self.authenticating {
            return;
        }

        let Some(image) = self.login_form.as_ref().and_then(|f| f.ocr_image()) else {
            return;
        };

        if let Some(solver) = self.solver.take() {
            solver.abort();
        }

        let generation = self.generation;
        let tx = self.tx.clone();
        self.solver = Some(runtime::spawn(async move {
            let code = crate::ocr::suggest_code(image).await.map(|p| p.text);
            let _ = tx.send((generation, Ok(Completed::Suggestion(code))));
        }));
    }

    fn back(&mut self) {
        self.remember_view();

        if self.cancel_pending() {
            self.message("Request cancelled.", false);

            return;
        }

        if self.nav.back() {
            self.searching = false;
            self.reset_view();
            self.message("", false);
        } else {
            self.message("", false);
        }
    }

    fn activate(&mut self, download_only: bool) {
        if self.busy {
            return;
        }

        let Some(view) = &self.nav.current else {
            return;
        };

        if self.text_focus && download_only {
            self.start_download(Action::Navigate(view.location.clone()), None, false);

            return;
        }

        if self.text_focus && !download_only {
            if !view.page.entries.is_empty() {
                self.text_focus = false;
            }

            return;
        }

        let Some(entry) = view.selected_entry() else {
            return;
        };
        let action = entry.action.clone();
        let course = view.location.course().cloned();

        if download_only {
            self.start_download(action, course, false);

            return;
        }

        match action {
            Action::Navigate(location) => self.load(location, false),
            Action::External(url) if self.browser => {
                self.browser_effects
                    .push(BrowserEffect::OpenUrl(url.into()));
                self.message("Opened in browser.", false);
            }
            Action::External(url) => match runtime::open(url.as_str()) {
                Ok(_) => self.message("Opened in browser.", false),
                Err(_) => self.message("Could not open browser.", true),
            },
            Action::File(file) => {
                if let Some(path) = self
                    .opened
                    .get(&file.url)
                    .filter(|p| runtime::file_exists(p))
                {
                    if self.browser {
                        self.browser_effects
                            .push(BrowserEffect::OpenFile(path.clone()));
                        self.message("Opened file.", false);
                        self.mark_read(&Action::File(file.clone()));

                        return;
                    }

                    match runtime::open(path) {
                        Ok(_) => {
                            self.mark_read(&Action::File(file.clone()));
                            self.message("Opened file.", false);
                        }
                        Err(_) => self.message("Could not open viewer.", true),
                    }
                } else {
                    self.start_download(Action::File(file), course, true);
                }
            }
            Action::None => {}
        }
    }

    fn start_download(&mut self, action: Action, course: Option<Course>, open_file: bool) {
        if self.downloading {
            self.message("Download running. x cancels.", false);

            return;
        }

        if !matches!(action, Action::Navigate(_) | Action::File(_)) {
            return;
        }

        let Some(api) = self.api.clone() else {
            return;
        };
        self.opening_read = if open_file {
            tracking::action_key(&action)
        } else {
            None
        };

        let label = match &action {
            Action::Navigate(Location::Course(course)) => course.id.clone(),
            Action::Navigate(location @ Location::Item(_, _, _)) => self
                .nav
                .current
                .as_ref()
                .and_then(|view| {
                    if &view.location == location {
                        Some(view.page.title.clone())
                    } else {
                        view.selected_entry().map(|entry| entry.title.clone())
                    }
                })
                .unwrap_or_else(|| "Item".into()),
            Action::Navigate(location) => location.label(),
            _ => String::new(),
        };
        self.download_name = format!("{}.zip", crate::api::safe_filename(&label));
        self.downloading = true;
        self.message("", false);
        self.browser_files.clear();
        self.download_id += 1;

        let id = self.download_id;
        self.download_status = "Finding files…".into();

        let tx = self.tx.clone();
        let directory = self.download_dir.clone();
        let existing = self.opened.clone();
        self.downloader = Some(runtime::spawn(async move {
            let plan = match action {
                Action::Navigate(location) => api.downloadables(&location).await,
                Action::File(file) => course
                    .map(|course| crate::api::DownloadPlan {
                        files: vec![DownloadTarget { course, file }],
                        skipped: Vec::new(),
                    })
                    .ok_or_else(|| anyhow::anyhow!("No course selected")),
                _ => return,
            };

            let plan = match plan {
                Ok(plan) => plan,
                Err(e) => {
                    let _ = tx.send((
                        id,
                        Ok(Completed::DownloadsFinished {
                            done: 0,
                            total: 0,
                            failed: 1,
                            error: Some(clean(&format!("{e:#}"))),
                            open: None,
                            expired: e.is::<auth::AccessDenied>(),
                        }),
                    ));

                    return;
                }
            };

            let total = plan.files.len();

            let mut done = 0;
            let mut failed = plan.skipped.len();
            let mut error = plan.skipped.first().cloned();
            let mut open = None;
            let mut expired = false;

            for target in plan.files {
                let result = if let Some(path) = existing
                    .get(&target.file.url)
                    .filter(|p| runtime::file_exists(p))
                {
                    Ok(path.clone())
                } else {
                    api.download(&target.file, &target.course, &directory).await
                };

                let saved = match result {
                    Ok(path) => {
                        done += 1;

                        if open_file && total == 1 {
                            open = Some(path.clone());
                        }

                        Some((target.file.url, path))
                    }
                    Err(e) => {
                        failed += 1;

                        if error.is_none() {
                            error = Some(clean(&format!("{}: {e:#}", target.file.filename)));
                        }

                        if e.is::<auth::AccessDenied>() {
                            expired = true;
                        }

                        None
                    }
                };

                let _ = tx.send((
                    id,
                    Ok(Completed::DownloadProgress {
                        done,
                        total,
                        failed,
                        saved,
                    }),
                ));

                if expired {
                    break;
                }
            }

            let _ = tx.send((
                id,
                Ok(Completed::DownloadsFinished {
                    done,
                    total,
                    failed,
                    error,
                    open,
                    expired,
                }),
            ));
        }));
    }

    fn cancel_downloads(&mut self) {
        if let Some(task) = self.downloader.take() {
            task.abort();
        }

        self.download_id += 1;
        self.downloading = false;
        self.download_status.clear();
    }

    fn clear_preview(&mut self) {
        if let Some(task) = self.preview_task.take() {
            task.abort();
        }

        self.preview_id += 1;
        self.preview_target = None;
        self.preview_page = None;
        self.preview_error = None;
        self.preview_width = 0;
    }

    fn sync_preview(&mut self) {
        let target = if self.preview_enabled && !self.login_screen && !self.busy {
            self.nav
                .current
                .as_ref()
                .and_then(|v| v.selected_entry())
                .and_then(|e| {
                    if let Action::Navigate(location) = &e.action {
                        Some(location.clone())
                    } else {
                        None
                    }
                })
        } else {
            None
        };

        if target == self.preview_target {
            return;
        }

        self.clear_preview();

        let Some(location) = target else {
            return;
        };
        self.preview_target = Some(location.clone());

        if let Some(page) = self.page_cache.get(&location).cloned() {
            self.preview_page = Some(page);

            return;
        }

        let Some(api) = self.api.clone() else {
            return;
        };
        let id = self.preview_id;
        let tx = self.tx.clone();
        self.preview_task = Some(runtime::spawn(async move {
            runtime::time::sleep(Duration::from_millis(120)).await;

            let result = api.load(&location).await;
            let _ = tx.send((
                0,
                Ok(Completed::Preview {
                    id,
                    location,
                    result,
                }),
            ));
        }));
    }

    fn edit_filter(&mut self, edit: Edit) {
        if let Some(view) = &mut self.nav.current {
            let mut filter = view.filter.clone();
            self.search_cursor.apply(&mut filter, edit, 512);

            if filter != view.filter {
                view.set_filter(filter);
            }
        }
    }

    fn scroll_or_select(&mut self, code: KeyCode) {
        let Some(v) = &mut self.nav.current else {
            return;
        };
        let scroll_text = self.text_focus
            || matches!(code, KeyCode::PageDown | KeyCode::PageUp) && !v.page.text.is_empty();
        let delta = match code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::PageDown => 8,
            KeyCode::PageUp => -8,
            _ => 0,
        };

        if scroll_text {
            let max = self
                .rendered
                .lines()
                .count()
                .saturating_sub(1)
                .min(u16::MAX as usize) as u16;

            v.scroll = match code {
                KeyCode::Home => 0,
                KeyCode::End | KeyCode::Char('G') => max,
                _ => v.scroll.saturating_add_signed(delta as i16).min(max),
            };
        } else {
            match code {
                KeyCode::Home => v.selected = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    v.selected = v.visible().len().saturating_sub(1)
                }
                _ => v.move_by(delta),
            };
        }
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }

        if key.code == KeyCode::Home && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.home();

            return false;
        }

        self.notice.clear();

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }

        if self.help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) => self.help = false,
                KeyCode::Char('q') => return true,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = self.help_scroll.saturating_add(1).min(self.help_max)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => {
                    self.help_scroll = self.help_scroll.saturating_add(8).min(self.help_max)
                }
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(8),
                _ => {}
            }

            return false;
        }

        if self.login_screen {
            match key.code {
                KeyCode::F(1) => {
                    self.help = true;
                    self.help_scroll = 0;
                }
                KeyCode::F(2) => self.prepare_login(),
                KeyCode::F(4) if !self.show_captcha => {}
                KeyCode::F(4) if self.browser => {
                    if let Some(bytes) = self.login_form.as_ref().and_then(|f| f.ocr_image()) {
                        self.browser_effects.push(BrowserEffect::Image(bytes));
                    }
                }
                KeyCode::F(4) => {
                    if let Some(form) = &self.login_form
                        && let Err(e) = form.open_image()
                    {
                        self.message(clean(&format!("Cannot open CAPTCHA image: {e}")), true);
                    }
                }
                KeyCode::F(6) if !self.browser && !self.authenticating => {
                    self.remember_password = !self.remember_password;

                    let result = tracking::Preferences {
                        username: self.login_fields.username.clone(),
                        save_password: self.remember_password,
                    }
                    .save();

                    if let Err(error) = result {
                        self.notice = error.to_string();
                        self.notice_error = true;
                    }

                    if !self.remember_password {
                        self.start_credential("forget", None);
                    } else {
                        self.notice = "Password will be saved after successful sign-in.".into();
                        self.notice_error = false;
                    }
                }
                KeyCode::F(5) if !self.authenticating => {
                    self.login_fields.set_captcha(String::new());
                    self.suggest_code();
                }
                KeyCode::Tab | KeyCode::Down if !self.authenticating => {
                    self.move_login_focus(false);
                    self.maybe_load_password();
                }
                KeyCode::BackTab | KeyCode::Up if !self.authenticating => {
                    self.move_login_focus(true);
                    self.maybe_load_password();
                }
                KeyCode::Enter if !self.authenticating => {
                    if self.login_fields.focus == 0 {
                        self.login_fields.focus = 1;
                        self.maybe_load_password();
                    } else if self.login_fields.focus == 1 && self.show_captcha {
                        self.login_fields.focus = 2;
                    } else {
                        self.submit_login();
                    }
                }
                KeyCode::Backspace if !self.authenticating => self.login_fields.backspace(),
                KeyCode::Delete if !self.authenticating => self.login_fields.edit(Edit::Delete),
                KeyCode::Left if !self.authenticating => self.login_fields.edit(Edit::Left),
                KeyCode::Right if !self.authenticating => self.login_fields.edit(Edit::Right),
                KeyCode::Home if !self.authenticating => self.login_fields.edit(Edit::Home),
                KeyCode::End if !self.authenticating => self.login_fields.edit(Edit::End),
                KeyCode::Char(c)
                    if !self.authenticating
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.login_fields.input(c)
                }
                KeyCode::Esc => {
                    self.login_fields.clear_secrets();

                    if self.cancel_pending() {
                        self.message("Login cancelled. F2 loads a new form.", false);
                    } else if self.api.is_some() {
                        self.login_screen = false;
                        self.login_form = None;
                    }
                }
                _ => {}
            }

            return false;
        }

        if self.searching {
            if let Some(v) = &mut self.nav.current {
                match key.code {
                    KeyCode::Esc => {
                        v.set_filter(String::new());
                        self.searching = false;
                    }
                    KeyCode::Enter => self.searching = false,
                    KeyCode::Backspace => self.edit_filter(Edit::Backspace),
                    KeyCode::Delete => self.edit_filter(Edit::Delete),
                    KeyCode::Left => self.edit_filter(Edit::Left),
                    KeyCode::Right => self.edit_filter(Edit::Right),
                    KeyCode::Home => self.edit_filter(Edit::Home),
                    KeyCode::End => self.edit_filter(Edit::End),
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.edit_filter(Edit::Insert(c))
                    }
                    _ => {}
                }
            }

            return false;
        }

        if let Some(jump) = &mut self.jump {
            match key.code {
                KeyCode::Esc => self.jump = None,
                KeyCode::Backspace => {
                    jump.prefix.pop();
                }
                KeyCode::Char(c) => {
                    let next = format!("{}{c}", jump.prefix);

                    if jump.targets.iter().any(|(s, _)| s.starts_with(&next)) {
                        jump.prefix = next;

                        if let Some((_, index)) =
                            jump.targets.iter().find(|(s, _)| *s == jump.prefix)
                        {
                            if let Some(v) = &mut self.nav.current {
                                v.selected = *index;
                            }

                            self.jump = None;
                            self.message("", false);
                        }
                    }
                }
                _ => {}
            }

            return false;
        }

        if self.pending_g {
            self.pending_g = false;

            if key.code == KeyCode::Char('g') {
                self.scroll_or_select(KeyCode::Home);

                return false;
            }

            if key.code == KeyCode::Esc {
                return false;
            }
        }

        if self.busy {
            match key.code {
                KeyCode::Char('q') => return true,
                KeyCode::Char('x') if self.downloading => {
                    self.cancel_downloads();
                    self.message("Download cancelled.", false);
                }
                KeyCode::Char('?') | KeyCode::F(1) => {
                    self.help = true;
                    self.help_scroll = 0;
                }
                KeyCode::Char('b') | KeyCode::Char('h') | KeyCode::Backspace | KeyCode::Left => {
                    self.back()
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    self.scroll_or_select(key.code)
                }
                _ => {}
            }

            return false;
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('L') if !self.busy => {
                self.show_captcha = false;
                self.login_fields.clear_secrets();
                self.login_fields.focus = 0;
                self.prepare_login();
            }
            KeyCode::Char('/') if !self.busy && self.nav.current.is_some() => {
                self.searching = true;

                if let Some(v) = &self.nav.current {
                    self.search_cursor.position = v.filter.chars().count();
                }

                self.text_focus = false;
            }
            KeyCode::Char('f')
                if !self.busy && !self.text_focus && !self.visible_rows.is_empty() =>
            {
                self.jump = Some(Jump::new(&self.visible_rows))
            }
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('b') | KeyCode::Char('h') | KeyCode::Backspace | KeyCode::Left => {
                self.back()
            }
            KeyCode::Enter | KeyCode::Char('o') | KeyCode::Char('l') | KeyCode::Right => {
                self.activate(false)
            }
            KeyCode::Char('d' | 'D') => self.activate(true),
            KeyCode::Char('x') if self.downloading => {
                self.cancel_downloads();
                self.message("Download cancelled.", false);
            }
            KeyCode::Char('r') if !self.busy => {
                let location = self
                    .nav
                    .current
                    .as_ref()
                    .map(|v| v.location.clone())
                    .unwrap_or(Location::Catalog("/courses".into()));

                self.load(location, true);
            }
            KeyCode::Tab => {
                if let Some(v) = &self.nav.current
                    && !v.page.text.is_empty()
                    && !v.page.entries.is_empty()
                {
                    self.text_focus = !self.text_focus;
                }
            }
            KeyCode::Esc => {
                if let Some(v) = &mut self.nav.current {
                    v.set_filter(String::new());
                }
            }
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::PageDown
            | KeyCode::PageUp
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('G') => self.scroll_or_select(key.code),
            _ => {}
        }

        false
    }

    fn hints(&self) -> Vec<Hint> {
        if self.help {
            return vec![
                hint("↑↓", "scroll"),
                hint("Esc", "close"),
                hint("Ctrl-Home", "home"),
                hint("q", "quit"),
            ];
        }

        if self.login_screen {
            if self.authenticating {
                return vec![
                    hint("Esc", "cancel"),
                    hint("Ctrl-Home", "home"),
                    hint("Ctrl-C", "quit"),
                ];
            }

            let mut hints = vec![
                hint("Tab/↑↓", "fields"),
                hint("←→", "edit"),
                hint(
                    "Enter",
                    if self.login_fields.focus == 0
                        || self.login_fields.focus == 1 && self.show_captcha
                    {
                        "next"
                    } else {
                        "sign in"
                    },
                ),
                hint(
                    "F2",
                    if self.show_captcha {
                        "new code"
                    } else {
                        "reload"
                    },
                ),
            ];

            if self.show_captcha {
                hints.push(hint("F4", "enlarge"));
            }

            if !self.browser {
                hints.push(hint(
                    "F6",
                    if self.remember_password {
                        "save password: on"
                    } else {
                        "save password: off"
                    },
                ));
            }

            if self.api.is_some() {
                hints.push(hint("Esc", "back"));
            }

            hints.extend([
                hint("Ctrl-Home", "home"),
                hint("F1", "help"),
                hint("Ctrl-C", "quit"),
            ]);

            return hints;
        }

        if self.searching {
            return vec![
                hint("←→", "edit"),
                hint("Enter", "done"),
                hint("Esc", "clear"),
                hint("Ctrl-Home", "home"),
                hint("Ctrl-C", "quit"),
            ];
        }

        if self.jump.is_some() {
            return vec![
                hint("Type label", "select"),
                hint("Esc", "cancel"),
                hint("Ctrl-Home", "home"),
                hint("Ctrl-C", "quit"),
            ];
        }

        if self.pending_g {
            return vec![
                hint("g", "first"),
                hint("Esc", "cancel"),
                hint("Ctrl-Home", "home"),
            ];
        }

        if self.busy {
            let mut hints = vec![
                hint("↑↓", "move"),
                hint(
                    "←",
                    if self.pending.is_some() {
                        "cancel"
                    } else {
                        "back"
                    },
                ),
                hint("?", "help"),
                hint("q", "quit"),
            ];

            if self.downloading {
                hints.push(hint("x", "cancel download"));
            }

            return hints;
        }

        let mut hints = vec![hint(
            "↑↓/jk",
            if self.text_focus { "scroll" } else { "move" },
        )];

        if let Some(v) = &self.nav.current {
            if self.text_focus && !v.page.entries.is_empty() {
                hints.push(hint("Tab/→", "files"));
            } else if !self.text_focus
                && let Some(e) = v.selected_entry()
            {
                hints.push(hint("Enter/→", open_label(&e.action)));

                if matches!(e.action, Action::File(_) | Action::Navigate(_)) {
                    hints.push(hint("d", "download"));
                }
            }

            if self.text_focus && !v.page.entries.is_empty() {
                hints.push(hint("d", "download"));
            }

            if !self.text_focus && !v.visible().is_empty() {
                hints.extend([hint("/", "filter"), hint("f", "jump")]);
            }

            if !self.text_focus && !v.page.text.is_empty() && !v.page.entries.is_empty() {
                hints.push(hint("Tab", "text"));
            }

            if !v.filter.is_empty() {
                hints.push(hint("Esc", "clear"));
            }
        }

        if !self.nav.history.is_empty() {
            hints.push(hint("←/h", "back"));
        }

        if self.downloading {
            hints.push(hint("x", "cancel download"));
        }

        hints.extend([
            hint("Ctrl-Home", "home"),
            hint("r", "refresh"),
            hint("?", "help"),
            hint("q", "quit"),
        ]);

        hints
    }

    fn draw(&mut self, f: &mut Frame) {
        self.remember_view();

        let area = f.area();
        self.preview_enabled = area.width >= 80 && !self.login_screen;
        // Cache hits need updating before drawing the frame.
        self.sync_preview();
        f.render_widget(
            Block::default().style(Style::default().bg(BG).fg(INK)),
            area,
        );

        if area.width < 24 || area.height < 10 {
            f.render_widget(Paragraph::new("Enlarge terminal. Ctrl-C quits."), area);

            return;
        }

        let hints = self.hints();
        let lines = hint_lines(&hints, area.width as usize);

        let mut status = if self.busy {
            format!(
                "{} {}",
                ["◐", "◓", "◑", "◒"][(self.started.elapsed().as_millis() / 150 % 4) as usize],
                self.status
            )
        } else if self.searching {
            format!(
                "/{}",
                self.search_cursor.display(
                    self.nav
                        .current
                        .as_ref()
                        .map(|v| v.filter.as_str())
                        .unwrap_or(""),
                    area.width.saturating_sub(2) as usize,
                    false,
                    true
                )
            )
        } else if let Some(jump) = &self.jump {
            format!("{}▏", jump.prefix)
        } else if self.pending_g {
            "g…".into()
        } else {
            self.status.clone()
        };

        if !self.notice.is_empty() {
            status = if status.is_empty() {
                self.notice.clone()
            } else {
                format!("{status} · {}", self.notice)
            };
        }

        if self.downloading {
            status = if status.is_empty() {
                self.download_status.clone()
            } else {
                format!("{status} · {}", self.download_status)
            };
        }

        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(if status.is_empty() { 0 } else { 1 }),
            Constraint::Length(if self.browser {
                0
            } else {
                (lines.len() as u16).clamp(1, 3)
            }),
        ])
        .split(area);

        self.draw_header(f, chunks[0]);

        if self.login_screen {
            self.draw_login(f, chunks[1]);
        } else {
            self.draw_content(f, chunks[1]);
        }

        if !status.is_empty() {
            f.render_widget(
                Paragraph::new(status).style(Style::default().fg(
                    if self.status_error || self.notice_error && !self.notice.is_empty() {
                        RED
                    } else {
                        MUTED
                    },
                )),
                chunks[2],
            );
        }

        f.render_widget(Paragraph::new(lines), chunks[3]);

        if self.help {
            let popup = Rect::new(
                area.x + 1,
                area.y + 1,
                area.width.saturating_sub(2),
                area.height.saturating_sub(2),
            );

            f.render_widget(Clear, popup);

            let width = popup.width.saturating_sub(2).max(1) as usize;
            let count: usize = HELP
                .lines()
                .map(|line| textwrap::wrap(line, width).len().max(1))
                .sum();
            self.help_max = count
                .saturating_sub(popup.height.saturating_sub(2) as usize)
                .min(u16::MAX as usize) as u16;
            self.help_scroll = self.help_scroll.min(self.help_max);
            f.render_widget(
                Paragraph::new(HELP)
                    .style(Style::default().bg(BG).fg(INK))
                    .block(panel(" Keys · Esc closes ", EDGE))
                    .wrap(Wrap { trim: false })
                    .scroll((self.help_scroll, 0)),
                popup,
            );
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let title = if self.login_screen {
            "Sign in".to_string()
        } else {
            let mut parts = Vec::new();

            for v in self.nav.history.iter().chain(self.nav.current.iter()) {
                if matches!(v.location, Location::Catalog(_)) {
                    continue;
                }

                if parts.last() != Some(&v.page.title) {
                    parts.push(v.page.title.clone());
                }
            }

            if parts.is_empty() {
                "Courses".into()
            } else {
                parts.join(" / ")
            }
        };

        f.render_widget(
            Paragraph::new(title).style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
            area,
        );
    }

    fn draw_login(&mut self, f: &mut Frame, area: Rect) {
        let width = area.width.min(116);
        let area = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + 1,
            width,
            area.height.saturating_sub(1),
        );

        let image = self
            .login_form
            .as_ref()
            .and_then(|form| form.image.as_ref());
        let wide = area.width >= 96;
        let (user, password, picture, code, button) = if !self.show_captcha {
            let width = area.width.min(32);
            let column = Rect::new(
                area.x + (area.width - width) / 2,
                area.y,
                width,
                area.height,
            );

            let fields = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(column);

            (
                fields[0],
                fields[1],
                Rect::default(),
                Rect::default(),
                fields[2],
            )
        } else if wide {
            let columns =
                Layout::horizontal([Constraint::Length(32), Constraint::Min(1)]).split(area);
            let fields = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(columns[0]);

            (fields[0], fields[1], columns[1], fields[2], fields[3])
        } else {
            let fields = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(2),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

            (fields[0], fields[1], fields[2], fields[3], fields[4])
        };

        self.login_hitboxes = vec![(user, 0), (password, 1), (button, 3)];

        if self.show_captcha {
            self.login_hitboxes.push((code, 2));
        }

        let values = [
            self.login_fields.username.as_str(),
            self.login_fields.password.as_str(),
            self.login_fields.captcha.as_str(),
        ];

        for (i, (label, rect)) in [("Username", user), ("Password", password), ("Code", code)]
            .into_iter()
            .enumerate()
        {
            if i == 2 && !self.show_captcha {
                continue;
            }

            let selected = self.login_fields.focus == i;
            let value = self.login_fields.cursors[i].display(
                values[i],
                rect.width.saturating_sub(3) as usize,
                i == 1,
                selected,
            );

            f.render_widget(
                Paragraph::new(value).block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_style(Style::default().fg(if selected { CYAN } else { EDGE }))
                        .title(Span::styled(
                            label,
                            Style::default().fg(if selected { CYAN } else { MUTED }),
                        )),
                ),
                rect,
            );
        }

        if let Some(image) = image {
            draw_captcha(f, image, picture);
        } else {
            f.render_widget(
                Paragraph::new(if self.busy {
                    "Loading image…"
                } else {
                    "F2: reload image"
                })
                .style(Style::default().fg(MUTED)),
                picture,
            );
        }

        f.render_widget(
            Paragraph::new(if self.authenticating {
                "Signing in…"
            } else {
                "Sign in"
            })
            .style(
                Style::default()
                    .fg(if self.login_fields.focus == 3 {
                        BG
                    } else {
                        CYAN
                    })
                    .bg(if self.login_fields.focus == 3 {
                        CYAN
                    } else {
                        BG
                    }),
            ),
            button,
        );
    }

    fn draw_content(&mut self, f: &mut Frame, area: Rect) {
        let target = self
            .nav
            .current
            .as_ref()
            .and_then(|v| v.selected_entry())
            .and_then(|e| match &e.action {
                Action::Navigate(location) => Some(location.clone()),
                _ => None,
            });

        if self.preview_enabled
            && let Some(target) = target.as_ref()
        {
            let columns =
                Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
                    .split(area);
            self.draw_current(f, columns[0]);
            self.draw_preview(f, columns[1], target);
        } else {
            self.draw_current(f, area);
        }
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect, target: &Location) {
        let page = if self.preview_target.as_ref() == Some(target) {
            self.preview_page.as_ref()
        } else {
            None
        };

        let Some(page) = page else {
            f.render_widget(
                Paragraph::new(
                    self.preview_error
                        .as_deref()
                        .filter(|_| self.preview_target.as_ref() == Some(target))
                        .unwrap_or("Loading…"),
                )
                .block(panel("", EDGE))
                .style(Style::default().fg(if self.preview_error.is_some() {
                    RED
                } else {
                    MUTED
                }))
                .wrap(Wrap { trim: false }),
                area,
            );

            return;
        };

        let block = panel(&page.title, EDGE);
        let inner = block.inner(area);
        f.render_widget(block, area);

        let has_text = !page.text.is_empty();
        let count = page.entries.len();
        let children_height = if count == 0 {
            0
        } else {
            (count as u16 * 2 + 1)
                .min(inner.height / 2)
                .max(3)
                .min(inner.height)
        };

        let sections = Layout::vertical(if !has_text {
            vec![Constraint::Length(0), Constraint::Min(1)]
        } else {
            vec![Constraint::Min(1), Constraint::Length(children_height)]
        })
        .split(inner);

        if has_text && sections[0].height > 0 {
            let width = sections[0].width.max(1);

            if self.preview_width != width {
                self.preview_rendered = clean(
                    &html2text::from_read(page.text.as_bytes(), width as usize)
                        .unwrap_or_else(|_| "Could not render description".into()),
                );
                self.preview_width = width;
            }

            f.render_widget(
                Paragraph::new(self.preview_rendered.as_str()).wrap(Wrap { trim: false }),
                sections[0],
            );
        }

        if sections[1].height > 0 {
            let width = sections[1].width.saturating_sub(1).max(1) as usize;
            let rows = page
                .entries
                .iter()
                .map(|entry| {
                    let is_new = self
                        .tracker
                        .as_ref()
                        .is_some_and(|tracker| tracker.is_new(&entry.action));
                    let title = if is_new {
                        format!("● {}", entry.title)
                    } else {
                        entry.title.clone()
                    };

                    let mut lines = textwrap::wrap(&title, width)
                        .iter()
                        .map(|s| {
                            Line::styled(
                                s.to_string(),
                                Style::default().fg(if is_new {
                                    GOLD
                                } else if matches!(entry.action, Action::File(_)) {
                                    GREEN
                                } else {
                                    INK
                                }),
                            )
                        })
                        .collect::<Vec<_>>();

                    if matches!(entry.action, Action::File(_)) && !entry.subtitle.is_empty() {
                        lines.push(Line::styled(
                            entry.subtitle.clone(),
                            Style::default().fg(MUTED),
                        ));
                    }

                    ListItem::new(lines)
                })
                .collect::<Vec<_>>();

            f.render_widget(List::new(rows), sections[1]);
        }
    }

    fn draw_current(&mut self, f: &mut Frame, area: Rect) {
        self.visible_rows.clear();
        self.list_hitboxes.clear();

        let Some(view) = &self.nav.current else {
            f.render_widget(
                Paragraph::new("Loading…")
                    .block(panel("", CYAN))
                    .wrap(Wrap { trim: true }),
                area,
            );

            return;
        };

        let has_text = !view.page.text.is_empty();
        let has_entries = !view.page.entries.is_empty();
        let split_wide = has_text && has_entries && area.width >= 90;
        let body = Layout::default()
            .direction(if split_wide {
                Direction::Horizontal
            } else {
                Direction::Vertical
            })
            .constraints(if !has_text {
                vec![Constraint::Length(0), Constraint::Min(1)]
            } else if !has_entries {
                vec![Constraint::Min(1), Constraint::Length(0)]
            } else {
                vec![Constraint::Percentage(57), Constraint::Percentage(43)]
            })
            .split(area);

        if has_text {
            let width = body[0].width.saturating_sub(2).max(1);

            if self.render_width != width {
                self.rendered = clean(
                    &html2text::from_read(view.page.text.as_bytes(), width as usize)
                        .unwrap_or_else(|_| "Could not render description".into()),
                );
                self.render_width = width;
            }

            f.render_widget(
                Paragraph::new(self.rendered.as_str())
                    .block(panel(" Text ", if self.text_focus { GREEN } else { EDGE }))
                    .wrap(Wrap { trim: false })
                    .scroll((view.scroll, 0)),
                body[0],
            );
        }

        if body[1].height > 0 {
            let entries = view.visible();
            let list_title = format!(
                " {}/{} ",
                if entries.is_empty() {
                    0
                } else {
                    view.selected + 1
                },
                entries.len()
            );

            let border = if self.jump.is_some() {
                PURPLE
            } else if !self.text_focus {
                CYAN
            } else {
                EDGE
            };

            if entries.is_empty() {
                f.render_widget(
                    Paragraph::new(if view.filter.is_empty() {
                        "No items."
                    } else {
                        "No matches. Esc clears."
                    })
                    .block(panel(&list_title, border))
                    .wrap(Wrap { trim: true }),
                    body[1],
                );
            } else {
                let width = body[1].width.saturating_sub(6).max(1) as usize;

                let mut heights = Vec::new();
                let rows: Vec<_> = entries
                    .iter()
                    .enumerate()
                    .map(|(index, e)| {
                        let tag = if let Some(jump) = &self.jump {
                            jump.label(index).to_string()
                        } else {
                            if self
                                .tracker
                                .as_ref()
                                .is_some_and(|tracker| tracker.is_new(&e.action))
                            {
                                "●".into()
                            } else {
                                String::new()
                            }
                        };

                        let active = self.jump.as_ref().is_none_or(|j| {
                            j.label(index).starts_with(&j.prefix) && !j.label(index).is_empty()
                        });

                        let mut lines = Vec::new();

                        for (line_index, line) in
                            textwrap::wrap(&e.title, width).iter().take(3).enumerate()
                        {
                            lines.push(Line::from(if line_index == 0 {
                                vec![
                                    Span::styled(
                                        format!("{tag:>2} "),
                                        Style::default()
                                            .fg(if self.jump.is_some() {
                                                PURPLE
                                            } else if !tag.is_empty() {
                                                GOLD
                                            } else {
                                                MUTED
                                            })
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        line.to_string(),
                                        Style::default().fg(if active { INK } else { EDGE }),
                                    ),
                                ]
                            } else {
                                vec![
                                    Span::raw("   "),
                                    Span::styled(line.to_string(), Style::default().fg(INK)),
                                ]
                            }));
                        }

                        if index == view.selected
                            && !e.subtitle.is_empty()
                            && !matches!(
                                e.action,
                                Action::Navigate(
                                    Location::Course(_)
                                        | Location::Catalog(_)
                                        | Location::Collection(_, _)
                                        | Location::Week(_, _)
                                )
                            )
                        {
                            lines.push(Line::styled(
                                format!("   {}", e.subtitle),
                                Style::default().fg(MUTED),
                            ));
                        }

                        heights.push(lines.len());

                        ListItem::new(Text::from(lines))
                    })
                    .collect();

                self.list_state.select(Some(view.selected));
                f.render_stateful_widget(
                    List::new(rows)
                        .block(panel(&list_title, border))
                        .highlight_symbol("› ")
                        .highlight_style(
                            Style::default()
                                .bg(Color::Rgb(35, 57, 78))
                                .add_modifier(Modifier::BOLD),
                        ),
                    body[1],
                    &mut self.list_state,
                );

                let mut room = body[1].height.saturating_sub(2) as usize;
                let mut row_y = body[1].y + 1;

                for (index, height) in heights.iter().enumerate().skip(self.list_state.offset()) {
                    if room == 0 {
                        break;
                    }

                    self.visible_rows.push(index);

                    let visible_height = (*height).min(room) as u16;
                    self.list_hitboxes.push((
                        Rect::new(
                            body[1].x + 1,
                            row_y,
                            body[1].width.saturating_sub(2),
                            visible_height,
                        ),
                        index,
                    ));

                    row_y += visible_height;
                    room = room.saturating_sub(*height);
                }
            }
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        if self.login_screen && !self.help && !self.authenticating {
            if event.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, index)) = self.login_hitboxes.iter().find(|(r, _)| {
                    r.contains(ratatui::layout::Position::new(event.column, event.row))
                })
            {
                self.login_fields.focus = *index;

                if *index == 3 {
                    self.submit_login();
                }
            }

            return;
        }

        if self.login_screen || self.help || self.searching || self.jump.is_some() {
            return;
        }

        match event.kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, index)) = self.list_hitboxes.iter().find(|(rect, _)| {
                    rect.contains(ratatui::layout::Position::new(event.column, event.row))
                }) && let Some(view) = &mut self.nav.current
                {
                    view.selected = *index;
                    self.text_focus = false;
                }
            }
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    self.scroll_or_select(KeyCode::Down);
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    self.scroll_or_select(KeyCode::Up);
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            _ => {}
        }
    }
}

fn panel(title: &str, color: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            title,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(PANEL).fg(INK))
}

fn draw_captcha(f: &mut Frame, image: &image::RgbImage, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let width = (area.width as u32 * 2)
        .min(image.width())
        .min(area.height as u32 * 4 * image.width() / image.height())
        .max(1);
    let height = (width * image.height() / image.width()).max(1);
    let resized =
        image::imageops::resize(image, width, height, image::imageops::FilterType::Triangle);

    // Each braille cell covers 2x4 pixels.
    for y in (0..height).step_by(4) {
        let mut line = String::new();

        for x in (0..width).step_by(2) {
            let mut dots = 0u8;

            for (dx, dy, bit) in [
                (0, 0, 1),
                (0, 1, 2),
                (0, 2, 4),
                (1, 0, 8),
                (1, 1, 16),
                (1, 2, 32),
                (0, 3, 64),
                (1, 3, 128),
            ] {
                if x + dx < width && y + dy < height {
                    let c = resized.get_pixel(x + dx, y + dy).0;

                    if (c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114) / 1000 < 160 {
                        dots |= bit;
                    }
                }
            }

            line.push(char::from_u32(0x2800 + dots as u32).unwrap());
        }

        f.render_widget(
            Paragraph::new(line).style(Style::default().fg(Color::Black).bg(Color::White)),
            Rect::new(area.x, area.y + (y / 4) as u16, area.width, 1),
        );
    }
}

fn open_label(action: &Action) -> &'static str {
    match action {
        Action::Navigate(Location::Course(_)) => "open course",
        Action::Navigate(Location::Catalog(_)) => "browse semester",
        Action::Navigate(Location::Week(_, _)) => "open week",
        Action::Navigate(Location::Collection(_, _)) => "open section",
        Action::Navigate(_) => "open item",
        Action::File(_) => "download + open file",
        Action::External(_) => "open in browser",
        Action::None => "view item",
    }
}

fn hint_lines(hints: &[Hint], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut used = 0;

    for h in hints {
        let key = if width < 76 {
            match h.key.as_str() {
                "→/l/Enter/o" => "→/l/↵",
                "←/h/b/⌫" => "←/h/⌫",
                "→/l/Enter/Tab" => "→/l/Tab",
                "gg/Home · G/End" => "gg/G",
                "PgUp/PgDn" => "Pg↑↓",
                "q/Ctrl-C" => "q/^C",
                "?/F1" => "?",
                other => other,
            }
        } else {
            &h.key
        };

        let action = if width < 76 {
            match h.action.as_str() {
                "select item" => "select",
                "open course" => "course",
                "open section" => "section",
                "open item" => "item",
                "open week" => "week",
                "browse semester" => "semester",
                "jump labels" => "jump",
                "move 8 items" => "page",
                "focus attachments" => "files",
                "download + open file" => "open file",
                "open in browser" => "browser",
                other => other,
            }
        } else {
            &h.action
        };

        let size = key.chars().count() + action.chars().count() + 6;

        if used > 0 && used + size > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }

        spans.push(Span::styled(
            format!(" {} ", key),
            Style::default()
                .bg(EDGE)
                .fg(GOLD)
                .add_modifier(Modifier::BOLD),
        ));

        spans.push(Span::styled(
            format!(" {}  ", action),
            Style::default().fg(INK),
        ));
        used += size;
    }

    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }

    lines
}

const HELP: &str = "↑ / ↓ or j / k    Move / scroll focused pane\nEnter / → / l    Open selected item\n← / h / b        Back\n/                Filter; Enter finishes, Esc clears\nf                Jump: type a row label, then Enter to open\ngg / G           First / last\nTab              Switch text / files\nPgUp / PgDn      Scroll a page\nd                Download selected branch; focused text: all files here\nx                Cancel download\nr                Refresh\nL                Sign in\nCtrl-Home        Courses (from any screen)\n●                New since the previous fetched list; opening marks read locally\nq / Ctrl-C       Quit\n\nLogin\nTab / Shift-Tab / ↑↓   Change field\nEnter            Next field / sign in\n← / →            Move caret\nHome / End       Start / end of field\nDelete / Backspace   Delete at / before caret\nF2               New CAPTCHA\nF4               Enlarge CAPTCHA\nF5               Suggest code with local OCR\nF6               Save password in desktop keyring (desktop only)\nEsc              Cancel and clear password\nCtrl-C           Quit\n\nEsc / ? / F1     Close help";

#[cfg(not(target_arch = "wasm32"))]
pub async fn run(session: PathBuf, download_dir: PathBuf) -> Result<()> {
    run_mode(session, download_dir, false).await
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn run_login(session: PathBuf, download_dir: PathBuf) -> Result<()> {
    run_mode(session, download_dir, true).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn run_mode(session: PathBuf, download_dir: PathBuf, force_login: bool) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(session, download_dir, tx)?;

    if force_login {
        app.login_screen = true;
        app.needs_login_form = true;
    } else if app.api.is_some() {
        app.start_location();
    }

    let mut terminal = ratatui::init();
    let result: Result<()> = async {
        crossterm::execute!(
            std::io::stdout(),
            event::EnableBracketedPaste,
            event::EnableMouseCapture
        )?;

        loop {
            while let Ok((generation, result)) = rx.try_recv() {
                app.complete(generation, result);
            }

            if app.login_screen && app.needs_login_form && !app.busy {
                app.prepare_login();
            }

            terminal.draw(|f| app.draw(f))?;

            if !event::poll(Duration::from_millis(80))? {
                runtime::task::yield_now().await;
                continue;
            }

            match event::read()? {
                Event::Key(key) if app.key(key) => break,
                Event::Paste(text) if app.login_screen && !app.authenticating => {
                    let text = zeroize::Zeroizing::new(text);

                    for c in text.chars() {
                        app.login_fields.input(c);
                    }
                }
                Event::Paste(text) if app.searching => {
                    for c in text.chars() {
                        app.edit_filter(Edit::Insert(c));
                    }
                }
                Event::Mouse(event) => app.mouse(event),
                Event::Resize(_, _) => {
                    app.jump = None;
                    app.render_width = 0;
                }
                _ => {}
            }
        }

        Ok(())
    }
    .await;

    app.cancel_pending();
    app.cancel_downloads();
    app.clear_preview();

    let _ = crossterm::execute!(
        std::io::stdout(),
        event::DisableBracketedPaste,
        event::DisableMouseCapture
    );

    app.remember_view();

    if let Some(task) = app.credential_job.take() {
        task.abort();
    }

    app.tracker.take();
    app.login_fields.clear_secrets();
    ratatui::restore();

    result
}

pub(crate) struct BrowserApp {
    app: App,
    rx: mpsc::UnboundedReceiver<(u64, Result<Completed>)>,
    terminal: ratatui::Terminal<ratatui::backend::TestBackend>,
}

#[derive(serde::Serialize)]
pub(crate) struct BrowserRun {
    x: u16,
    y: u16,
    text: String,
    widths: Vec<u8>,
    fg: String,
    bg: String,
    modifiers: u16,
}

#[derive(serde::Serialize)]
pub(crate) struct BrowserHint {
    label: String,
    action: String,
    keys: Vec<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct BrowserFrame {
    width: u16,
    height: u16,
    runs: Vec<BrowserRun>,
    hints: Vec<BrowserHint>,
    mode: &'static str,
    input: Option<&'static str>,
    username: Option<String>,
    opens_browser: bool,
    error: bool,
}

fn browser_keys(label: &str) -> Vec<String> {
    match label {
        "↑↓" | "↑↓/jk" => vec!["ArrowUp".into(), "ArrowDown".into()],
        "←→" => vec!["ArrowLeft".into(), "ArrowRight".into()],
        "Tab/↑↓" => vec!["Shift+Tab".into(), "Tab".into()],
        "Enter/→" => vec!["Enter".into()],
        "Tab/→" => vec!["Tab".into()],
        "←/h" | "←" => vec!["ArrowLeft".into()],
        "Ctrl-C" => vec!["Ctrl+c".into()],
        "Ctrl-Home" => vec!["Ctrl+Home".into()],
        "Type label" => vec!["TextInput".into()],
        other => vec![other.into()],
    }
}

fn browser_color(color: Color, default: &str) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => "#000000".into(),
        Color::White => "#ffffff".into(),
        _ => default.into(),
    }
}

impl BrowserApp {
    pub(crate) fn new(session: PathBuf, download_dir: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();

        let mut app = App::new(session, download_dir, tx)?;
        app.browser = true;
        app.remember_password = false;

        if app.api.is_some() {
            app.start_location();
        }

        Ok(Self {
            app,
            rx,
            terminal: ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 32))?,
        })
    }

    pub(crate) fn resize(&mut self, width: u16, height: u16) -> Result<()> {
        let columns_changed = self.terminal.backend().buffer().area.width != width.clamp(24, 240);
        self.terminal
            .backend_mut()
            .resize(width.clamp(24, 240), height.clamp(10, 100));
        self.terminal
            .resize(Rect::new(0, 0, width.clamp(24, 240), height.clamp(10, 100)))?;

        if columns_changed {
            self.app.jump = None;
        }

        self.app.render_width = 0;

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn completion(&mut self) {
        if let Some((generation, result)) = self.rx.recv().await {
            self.app.complete(generation, result);
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn poll(&mut self) {
        while let Ok((generation, result)) = self.rx.try_recv() {
            self.app.complete(generation, result);
        }
    }

    pub(crate) fn animating(&self) -> bool {
        self.app.busy
    }

    pub(crate) fn key(&mut self, key: &str, ctrl: bool, alt: bool, shift: bool) -> bool {
        let mut modifiers = KeyModifiers::NONE;

        if ctrl {
            modifiers |= KeyModifiers::CONTROL;
        }

        if alt {
            modifiers |= KeyModifiers::ALT;
        }

        if shift {
            modifiers |= KeyModifiers::SHIFT;
        }

        let code = match key {
            "ArrowUp" => KeyCode::Up,
            "ArrowDown" => KeyCode::Down,
            "ArrowLeft" => KeyCode::Left,
            "ArrowRight" => KeyCode::Right,
            "Enter" => KeyCode::Enter,
            "Escape" | "Esc" => KeyCode::Esc,
            "Backspace" => KeyCode::Backspace,
            "Delete" => KeyCode::Delete,
            "Tab" if shift => KeyCode::BackTab,
            "Tab" => KeyCode::Tab,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "F1" => KeyCode::F(1),
            "F2" => KeyCode::F(2),
            "F4" => KeyCode::F(4),
            "F5" => KeyCode::F(5),
            s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
            _ => return false,
        };

        self.app.key(KeyEvent::new(code, modifiers))
    }

    pub(crate) fn action(&mut self, index: usize, choice: usize, expected: &str) -> bool {
        let hints = self.app.hints();
        let Some(hint) = hints.get(index) else {
            return false;
        };
        let keys = browser_keys(&hint.key);
        let Some(key) = keys.get(choice) else {
            return false;
        };

        if key != expected {
            return false;
        }

        let (key, ctrl, shift) = if let Some(k) = key.strip_prefix("Ctrl+") {
            (k, true, false)
        } else if let Some(k) = key.strip_prefix("Shift+") {
            (k, false, true)
        } else {
            (key.as_str(), false, false)
        };

        self.key(key, ctrl, false, shift)
    }

    pub(crate) fn text(&mut self, text: &str) {
        if self.app.help {
            return;
        }

        for c in text.chars().filter(|c| !c.is_control()).take(4096) {
            if self.app.login_screen && !self.app.authenticating {
                self.app.login_fields.input(c);
            } else if self.app.searching {
                self.app.edit_filter(Edit::Insert(c));
            } else if self.app.jump.is_some() {
                self.app
                    .key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        }
    }

    pub(crate) fn restore_username(&mut self, username: &str) {
        if self.app.login_screen
            && !self.app.authenticating
            && self.app.login_fields.username.is_empty()
        {
            let username: String = username
                .chars()
                .filter(|c| !c.is_control())
                .take(60)
                .collect();

            self.app.login_fields.cursors[0].position = username.chars().count();
            self.app.login_fields.username = username;
        }
    }

    pub(crate) fn pointer(&mut self, x: u16, y: u16, kind: &str) {
        let kind = match kind {
            "move" => MouseEventKind::Moved,
            "down" => MouseEventKind::Down(MouseButton::Left),
            "scroll_down" => MouseEventKind::ScrollDown,
            "scroll_up" => MouseEventKind::ScrollUp,
            _ => return,
        };

        self.app.mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            #[cfg(not(target_arch = "wasm32"))]
            modifiers: KeyModifiers::NONE,
        });
    }

    pub(crate) fn effects(&mut self) -> Vec<BrowserEffect> {
        std::mem::take(&mut self.app.browser_effects)
    }

    pub(crate) fn message(&mut self, message: String) {
        self.app.message(message, true);
    }

    pub(crate) fn frame(&mut self) -> Result<BrowserFrame> {
        if self.app.login_screen && self.app.needs_login_form && !self.app.busy {
            self.app.prepare_login();
        }

        self.terminal.draw(|frame| self.app.draw(frame))?;

        let buffer = self.terminal.backend().buffer();

        let mut runs: Vec<BrowserRun> = Vec::new();

        for y in 0..buffer.area.height {
            let mut x = 0;

            while x < buffer.area.width {
                let cell = &buffer[(x, y)];
                let fg = browser_color(cell.fg, "#dee7f2");
                let bg = browser_color(cell.bg, "#0f141e");
                let modifiers = cell.modifier.bits();
                let width = unicode_width::UnicodeWidthStr::width(cell.symbol()).max(1) as u8;

                if let Some(last) = runs.last_mut()
                    && last.y == y
                    && last.fg == fg
                    && last.bg == bg
                    && last.modifiers == modifiers
                {
                    last.text.push_str(cell.symbol());
                    last.widths.push(width);
                } else {
                    runs.push(BrowserRun {
                        x,
                        y,
                        text: cell.symbol().into(),
                        widths: vec![width],
                        fg,
                        bg,
                        modifiers,
                    });
                }

                x += u16::from(width);
            }
        }

        let hints = self
            .app
            .hints()
            .into_iter()
            .map(|hint| BrowserHint {
                keys: browser_keys(&hint.key),
                label: hint.key,
                action: hint.action,
            })
            .collect();

        let (mode, input) = if self.app.help {
            ("help", None)
        } else if self.app.login_screen {
            (
                "login",
                if self.app.authenticating || self.app.login_fields.focus == 3 {
                    None
                } else {
                    Some(match self.app.login_fields.focus {
                        0 => "username",
                        1 => "password",
                        _ => "code",
                    })
                },
            )
        } else if self.app.searching {
            ("search", Some("filter"))
        } else if self.app.jump.is_some() {
            ("jump", Some("label"))
        } else {
            ("browse", None)
        };

        Ok(BrowserFrame {
            width: buffer.area.width,
            height: buffer.area.height,
            runs,
            hints,
            mode,
            input,
            username: self
                .app
                .login_screen
                .then(|| self.app.login_fields.username.clone()),
            error: self.app.status_error,
            opens_browser: !self.app.login_screen
                && !self.app.help
                && !self.app.searching
                && self.app.jump.is_none()
                && !self.app.pending_g
                && !self.app.busy
                && !self.app.text_focus
                && self
                    .app
                    .nav
                    .current
                    .as_ref()
                    .and_then(|v| v.selected_entry())
                    .is_some_and(|e| matches!(e.action, Action::File(_) | Action::External(_))),
        })
    }
}

impl Drop for BrowserApp {
    fn drop(&mut self) {
        self.app.cancel_pending();
        self.app.cancel_downloads();
        self.app.clear_preview();
        self.app.remember_view();

        if let Some(task) = self.app.credential_job.take() {
            task.abort();
        }

        self.app.tracker.take();
        self.app.login_fields.clear_secrets();
    }
}
