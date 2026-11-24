#[cfg(not(target_arch = "wasm32"))]
use crate::auth::{self, ORIGIN};
use crate::model::parse_catalog;
use anyhow::{Context, Result, bail};
use image::RgbImage;
#[cfg(not(target_arch = "wasm32"))]
use reqwest::{Client, Response, header::HeaderValue};
use scraper::{Html, Selector};
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use url::Url;
use zeroize::Zeroizing;

fn at_origin(url: &Url, origin: &url::Origin) -> bool {
    url.origin() == *origin && url.username().is_empty() && url.password().is_none()
}

#[cfg(not(target_arch = "wasm32"))]
struct Cookies {
    store: Mutex<cookie_store::CookieStore>,
    origin: url::Origin,
}

#[cfg(not(target_arch = "wasm32"))]
impl Cookies {
    fn new(origin: &Url) -> Self {
        Self {
            store: Mutex::default(),
            origin: origin.origin(),
        }
    }

    fn accepts(&self, url: &Url) -> bool {
        at_origin(url, &self.origin)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl reqwest::cookie::CookieStore for Cookies {
    fn set_cookies(&self, cookies: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        if !self.accepts(url) {
            return;
        }

        let mut store = self.store.lock().unwrap();

        for value in cookies {
            if let Ok(value) = value.to_str() {
                let _ = store.parse(value, url);
            }
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        if !self.accepts(url) {
            return None;
        }

        let store = self.store.lock().unwrap();
        let value = store
            .get_request_values(url)
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");

        if value.is_empty() {
            None
        } else {
            HeaderValue::from_str(&value).ok()
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Cookies {
    fn snapshot(&self) -> auth::Session {
        let store = self.store.lock().unwrap();
        let url = Url::parse(&self.origin.ascii_serialization()).unwrap();

        auth::Session {
            cookies: store
                .iter_unexpired()
                .filter(|c| c.domain.matches(&url))
                .map(|c| auth::SessionCookie {
                    name: c.name().into(),
                    value: c.value().into(),
                    domain: url.host_str().unwrap().into(),
                    path: c.path.as_ref().into(),
                    secure: c.secure().unwrap_or(false),
                    expires: match c.expires {
                        cookie_store::CookieExpiration::SessionEnd => -1.0,
                        cookie_store::CookieExpiration::AtUtc(t) => t.unix_timestamp() as f64,
                    },
                })
                .collect(),
        }
    }
}

struct Form {
    action: Url,
    #[cfg(not(target_arch = "wasm32"))]
    referer: Url,
    hidden: Vec<(String, String)>,
    captcha: Option<Url>,
}

fn select(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

fn parse_form(html: &str, base: &Url) -> Result<Form> {
    let doc = Html::parse_document(html);
    let form = doc
        .select(&select("form.user-login-form"))
        .next()
        .context("Login form unavailable or changed. Refresh and try again.")?;

    if !form
        .value()
        .attr("method")
        .is_some_and(|m| m.eq_ignore_ascii_case("post"))
    {
        bail!("Unexpected login form method");
    }

    let action = base.join(
        form.value()
            .attr("action")
            .context("Login form action missing")?,
    )?;

    if !at_origin(&action, &base.origin()) || action.path() != "/user/login" {
        bail!("Refusing unexpected login form destination");
    }

    for name in ["name", "pass"] {
        if !form
            .select(&select("input"))
            .any(|i| i.value().attr("name") == Some(name))
        {
            bail!("Login form fields changed");
        }
    }

    let hidden = form
        .select(&select("input[name]"))
        .filter_map(|i| {
            let kind = i.value().attr("type").unwrap_or("text");

            if !matches!(kind, "hidden" | "submit") {
                return None;
            }

            Some((
                i.value().attr("name")?.to_string(),
                i.value().attr("value").unwrap_or("").to_string(),
            ))
        })
        .collect::<Vec<_>>();

    if !hidden
        .iter()
        .any(|(k, v)| k == "form_id" && v == "user_login_form")
    {
        bail!("Unexpected login form identifier");
    }

    let captcha = form
        .select(&select("img[src]"))
        .find(|img| {
            img.value().attr("alt") == Some("Image CAPTCHA")
                || img
                    .value()
                    .attr("src")
                    .is_some_and(|s| s.starts_with("/image-captcha-generate/"))
        })
        .map(|img| base.join(img.value().attr("src").unwrap()))
        .transpose()?;

    if let Some(url) = &captcha
        && (!at_origin(url, &base.origin()) || !url.path().starts_with("/image-captcha-generate/"))
    {
        bail!("Unexpected CAPTCHA image origin/path");
    }

    if form
        .select(&select("input[name=captcha_response]"))
        .next()
        .is_some()
        && captcha.is_none()
    {
        bail!("CAPTCHA image not found. Refresh the login form.");
    }

    Ok(Form {
        action,
        #[cfg(not(target_arch = "wasm32"))]
        referer: base.clone(),
        hidden,
        captcha,
    })
}

pub struct Credentials {
    pub username: String,
    pub password: Zeroizing<String>,
    pub captcha: String,
}

#[derive(Default)]
pub struct Fields {
    pub username: String,
    pub password: Zeroizing<String>,
    pub captcha: String,
    pub focus: usize,
    pub cursors: [crate::text::Cursor; 3],
}

impl Fields {
    pub fn clear_secrets(&mut self) {
        self.password = Zeroizing::new(String::new());
        self.captcha.clear();
        self.cursors[1] = Default::default();
        self.cursors[2] = Default::default();
    }

    pub fn take(&mut self) -> Credentials {
        self.cursors[1] = Default::default();
        self.cursors[2] = Default::default();

        Credentials {
            username: self.username.clone(),
            password: std::mem::take(&mut self.password),
            captcha: std::mem::take(&mut self.captcha),
        }
    }

    pub fn input(&mut self, c: char) {
        self.edit(crate::text::Edit::Insert(c));
    }

    pub fn edit(&mut self, edit: crate::text::Edit) {
        let (field, max) = match self.focus {
            0 => (&mut self.username, 60),
            1 => (&mut *self.password, 128),
            2 => (&mut self.captcha, 128),
            _ => return,
        };

        self.cursors[self.focus].apply(field, edit, max);
    }

    pub fn backspace(&mut self) {
        self.edit(crate::text::Edit::Backspace);
    }

    pub fn set_captcha(&mut self, value: String) {
        self.cursors[2].position = value.chars().count();
        self.captcha = value;
    }
}

fn decode_image(bytes: &[u8]) -> Result<RgbImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);

    Ok(reader
        .decode()
        .context("Could not decode CAPTCHA image")?
        .to_rgb8())
}

pub struct Challenge {
    #[cfg(not(target_arch = "wasm32"))]
    client: Client,
    #[cfg(not(target_arch = "wasm32"))]
    cookies: Arc<Cookies>,
    form: Form,
    pub image: Option<RgbImage>,
    #[cfg(not(target_arch = "wasm32"))]
    preview: Option<tempfile::NamedTempFile>,
    #[cfg(target_arch = "wasm32")]
    preview: Option<Vec<u8>>,
}

pub enum Outcome {
    Authenticated,
    Rejected(Box<Challenge>, &'static str),
}

impl Challenge {
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn fetch() -> Result<Self> {
        let origin = Url::parse(ORIGIN)?;
        let cookies = Arc::new(Cookies::new(&origin));
        let allowed = origin.origin();
        let client = Client::builder()
            .cookie_provider(cookies.clone())
            .user_agent("hello-iitk-tui/0.1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(40))
            .redirect(reqwest::redirect::Policy::custom(move |a| {
                if !at_origin(a.url(), &allowed) || a.previous().len() >= 8 {
                    a.error("Unexpected login redirect")
                } else {
                    a.follow()
                }
            }))
            .build()?;

        let response = client
            .get(origin.join("/user/login")?)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Cannot load login form: {}", e.without_url()))?;

        let url = response.url().clone();
        let html = String::from_utf8(read(response, 2 * 1024 * 1024).await?)?;

        Self::from_html(client, cookies, &html, &url).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn from_html(
        client: Client,
        cookies: Arc<Cookies>,
        html: &str,
        url: &Url,
    ) -> Result<Self> {
        let form = parse_form(html, url)?;
        let (image, preview) = if let Some(url) = &form.captcha {
            let response = client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("CAPTCHA request failed: {}", e.without_url()))?;

            let bytes = read(response, 2 * 1024 * 1024).await?;

            let image = decode_image(&bytes)?;

            let mut preview = tempfile::Builder::new()
                .prefix("hello-captcha-")
                .suffix(".png")
                .tempfile()?;
            use std::io::Write;
            preview.write_all(&bytes)?;

            (Some(image), Some(preview))
        } else {
            (None, None)
        };

        Ok(Self {
            client,
            cookies,
            form,
            image,
            preview,
        })
    }

    pub fn has_captcha(&self) -> bool {
        self.form.captcha.is_some()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn ocr_image(&self) -> Option<Vec<u8>> {
        std::fs::read(self.preview.as_ref()?.path()).ok()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_image(&self) -> Result<()> {
        open::that_detached(self.preview.as_ref().context("No CAPTCHA image")?.path())?;

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn submit(self, credentials: Credentials, path: &Path) -> Result<Outcome> {
        if credentials.username.is_empty() || credentials.password.is_empty() {
            bail!("Enter your username and password");
        }

        if self.has_captcha() && credentials.captcha.is_empty() {
            bail!("Enter the CAPTCHA characters");
        }

        let mut fields = self
            .form
            .hidden
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>();

        fields.extend([
            ("name", credentials.username.as_str()),
            ("pass", credentials.password.as_str()),
        ]);

        if self.has_captcha() {
            fields.push(("captcha_response", credentials.captcha.as_str()));
        }

        let response = self
            .client
            .post(self.form.action.clone())
            .header("Origin", self.form.action.origin().ascii_serialization())
            .header("Referer", self.form.referer.as_str())
            .form(&fields)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Login request failed: {}", e.without_url()))?;

        let url = response.url().clone();
        let html = String::from_utf8(read(response, 2 * 1024 * 1024).await?)?;
        drop(fields);
        drop(credentials);

        if Html::parse_document(&html)
            .select(&select("form.user-login-form"))
            .next()
            .is_some()
        {
            let reason = rejection_reason(&html);

            return Ok(Outcome::Rejected(
                Box::new(Self::from_html(self.client, self.cookies, &html, &url).await?),
                reason,
            ));
        }

        let response = self
            .client
            .get(self.form.action.join("/courses")?)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Could not verify login: {}", e.without_url()))?;

        let html = String::from_utf8(read(response, 8 * 1024 * 1024).await?)?;
        parse_catalog(&html, "/courses")
            .context("Login was not confirmed. Refresh the form and try again.")?;
        auth::save(path, &self.cookies.snapshot())?;

        Ok(Outcome::Authenticated)
    }
}

pub(crate) const CAPTCHA_REJECTED: &str = "CAPTCHA rejected. Enter your password and the new code.";

fn rejection_reason(html: &str) -> &'static str {
    let doc = Html::parse_document(html);
    let errors = doc
        .select(&select(
            ".messages--error, .alert-danger, .form-item--error-message, [role=alert]",
        ))
        .flat_map(|e| e.text())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    if errors.contains("captcha") {
        CAPTCHA_REJECTED
    } else if errors.contains("too many") || errors.contains("blocked") {
        "Too many attempts. Wait before trying again."
    } else if errors.contains("password") || errors.contains("username") {
        "Username or password rejected."
    } else {
        "Login rejected. Check your credentials and retry."
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn read(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        bail!(
            "Login service returned HTTP {}. Retry later.",
            response.status().as_u16()
        );
    }

    let mut bytes = Vec::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("Login response interrupted: {}", e.without_url()))?
    {
        if bytes.len() + chunk.len() > limit {
            bail!("Login response exceeds limit");
        }

        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
impl Challenge {
    pub async fn fetch() -> Result<Self> {
        let response = crate::browser::http::request("/user/login", None).await?;
        if !(200..300).contains(&response.status) {
            bail!("Login service returned HTTP {}", response.status);
        }

        Self::from_html(
            &String::from_utf8(response.body)?,
            &Url::parse(&response.url)?,
        )
        .await
    }

    async fn from_html(html: &str, base: &Url) -> Result<Self> {
        let form = parse_form(html, base)?;
        let (image, preview) = if let Some(url) = &form.captcha {
            let path = format!(
                "{}{}",
                url.path(),
                url.query().map(|q| format!("?{q}")).unwrap_or_default()
            );
            let response = crate::browser::http::request(&path, None).await?;
            if response.status != 200 || response.body.len() > 2 * 1024 * 1024 {
                bail!("Could not load CAPTCHA image");
            }
            let image = decode_image(&response.body)?;
            (Some(image), Some(response.body))
        } else {
            (None, None)
        };

        Ok(Self {
            form,
            image,
            preview,
        })
    }

    pub fn ocr_image(&self) -> Option<Vec<u8>> {
        self.preview.clone()
    }

    pub fn open_image(&self) -> Result<()> {
        bail!("Open the image in the browser")
    }

    pub async fn submit(self, credentials: Credentials, _path: &Path) -> Result<Outcome> {
        if credentials.username.is_empty() || credentials.password.is_empty() {
            bail!("Enter your username and password");
        }
        let mut fields = self.form.hidden.clone();
        fields.push(("name".into(), credentials.username.clone()));
        fields.push(("pass".into(), credentials.password.to_string()));
        if self.has_captcha() {
            fields.push(("captcha_response".into(), credentials.captcha.clone()));
        }
        let body = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .finish(),
        );
        use zeroize::Zeroize;
        for (_, value) in &mut fields {
            value.zeroize();
        }
        let path = format!(
            "{}{}",
            self.form.action.path(),
            self.form
                .action
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        let response = crate::browser::http::request(&path, Some(body.as_str())).await?;
        let html = String::from_utf8(response.body)?;

        if Html::parse_document(&html)
            .select(&select("form.user-login-form"))
            .next()
            .is_some()
        {
            return Ok(Outcome::Rejected(
                Box::new(Self::from_html(&html, &Url::parse(&response.url)?).await?),
                rejection_reason(&html),
            ));
        }

        let response = crate::browser::http::request("/courses", None).await?;
        parse_catalog(std::str::from_utf8(&response.body)?, "/courses")?;
        crate::browser::http::bootstrap().await?;

        Ok(Outcome::Authenticated)
    }
}
