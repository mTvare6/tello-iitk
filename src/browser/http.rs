use anyhow::{Context, Result, bail, ensure};
use js_sys::{Array, Uint8Array};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, Request, RequestCredentials, RequestInit, Response};

thread_local! {
    static FILES: RefCell<HashMap<String, (String, Blob)>> = RefCell::new(HashMap::new());
}

pub struct Reply {
    pub status: u16,
    pub url: String,
    pub body: Vec<u8>,
}

fn error(value: JsValue) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        value
            .as_string()
            .unwrap_or_else(|| "Browser request failed".into())
    )
}

struct Cancel {
    controller: web_sys::AbortController,
    _timeout: gloo_timers::callback::Timeout,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.controller.abort();
    }
}

struct Fetched {
    response: Response,
    _cancel: Cancel,
}

impl std::ops::Deref for Fetched {
    type Target = Response;
    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

async fn fetch(path: &str, body: Option<&str>, timeout: u32) -> Result<Fetched> {
    let controller = web_sys::AbortController::new().map_err(error)?;
    let deadline = controller.clone();
    let cancel = Cancel {
        controller,
        _timeout: gloo_timers::callback::Timeout::new(timeout, move || deadline.abort()),
    };
    let init = RequestInit::new();
    init.set_signal(Some(&cancel.controller.signal()));
    init.set_method(if body.is_some() { "POST" } else { "GET" });
    init.set_credentials(RequestCredentials::SameOrigin);

    if let Some(body) = body {
        init.set_body(&JsValue::from_str(body));
    }

    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", path)
        .finish();
    let request =
        Request::new_with_str_and_init(&format!("/api/request?{query}"), &init).map_err(error)?;
    request
        .headers()
        .set("X-Hello-Client", "1")
        .map_err(error)?;

    if body.is_some() {
        request
            .headers()
            .set("Content-Type", "application/x-www-form-urlencoded")
            .map_err(error)?;
    }

    let window = web_sys::window().context("Browser unavailable")?;
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(error)?
        .dyn_into::<Response>()
        .map_err(error)?;

    if response
        .headers()
        .get("x-hello-error")
        .map_err(error)?
        .is_some()
    {
        let message = JsFuture::from(response.text().map_err(error)?)
            .await
            .map_err(error)?
            .as_string()
            .unwrap_or_default();
        bail!("{}", message);
    }

    Ok(Fetched {
        response,
        _cancel: cancel,
    })
}

pub async fn request(path: &str, body: Option<&str>) -> Result<Reply> {
    let response = fetch(path, body, 40_000).await?;
    let status = response.status();
    let url = response
        .headers()
        .get("x-hello-url")
        .map_err(error)?
        .context("Service response has no URL")?;
    let bytes = Uint8Array::new(
        &JsFuture::from(response.array_buffer().map_err(error)?)
            .await
            .map_err(error)?,
    )
    .to_vec();
    ensure!(
        bytes.len() <= 32 * 1024 * 1024,
        "API response exceeds 32 MiB"
    );

    Ok(Reply {
        status,
        url,
        body: bytes,
    })
}

pub async fn bootstrap() -> Result<()> {
    let init = RequestInit::new();
    init.set_credentials(RequestCredentials::SameOrigin);
    let request = Request::new_with_str_and_init("/api/bootstrap", &init).map_err(error)?;
    request
        .headers()
        .set("X-Hello-Client", "1")
        .map_err(error)?;

    let response = JsFuture::from(
        web_sys::window()
            .context("Browser unavailable")?
            .fetch_with_request(&request),
    )
    .await
    .map_err(error)?
    .dyn_into::<Response>()
    .map_err(error)?;
    ensure!(response.ok(), "Could not load session");
    let text = JsFuture::from(response.text().map_err(error)?)
        .await
        .map_err(error)?
        .as_string()
        .context("Invalid session response")?;
    set_account(&serde_json::from_str(&text)?);

    Ok(())
}

pub fn set_account(value: &serde_json::Value) {
    let account = value.get("account").and_then(|a| {
        Some((
            a.get("uid")?.as_str()?.to_owned(),
            a.get("username")?.as_str()?.to_owned(),
        ))
    });
    crate::auth::set_account(account);
}

pub async fn download(url: &url::Url, name: String, limit: u64) -> Result<PathBuf> {
    let path = format!(
        "{}{}",
        url.path(),
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    let response = fetch(&path, None, 300_000).await?;

    if matches!(response.status(), 401 | 403) {
        return Err(crate::auth::AccessDenied.into());
    }

    ensure!(
        response.ok(),
        "Download returned HTTP {}",
        response.status()
    );
    let content_type = response
        .headers()
        .get("content-type")
        .map_err(error)?
        .unwrap_or_default();

    if content_type.contains("text/html") {
        return Err(crate::auth::AccessDenied.into());
    }

    if let Some(size) = response
        .headers()
        .get("content-length")
        .map_err(error)?
        .and_then(|n| n.parse::<u64>().ok())
    {
        ensure!(size <= limit, "File exceeds 512 MiB limit");
    }

    let blob = JsFuture::from(response.blob().map_err(error)?)
        .await
        .map_err(error)?
        .dyn_into::<Blob>()
        .map_err(error)?;
    ensure!(
        blob.size() > 0.0 && blob.size() <= limit as f64,
        "File is empty or exceeds 512 MiB"
    );
    let total = FILES.with(|files| files.borrow().values().map(|(_, b)| b.size()).sum::<f64>());
    ensure!(
        total + blob.size() <= 1024.0 * 1024.0 * 1024.0,
        "Browser file cache exceeds 1 GiB; reload before downloading more"
    );
    let object_url = web_sys::Url::create_object_url_with_blob(&blob).map_err(error)?;
    FILES.with(|files| files.borrow_mut().insert(object_url.clone(), (name, blob)));

    Ok(PathBuf::from(object_url))
}

pub fn file_exists(path: &Path) -> bool {
    FILES.with(|files| {
        files
            .borrow()
            .contains_key(&path.to_string_lossy().to_string())
    })
}

pub fn file_name(path: &Path) -> String {
    FILES.with(|files| {
        files
            .borrow()
            .get(&path.to_string_lossy().to_string())
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "attachment".into())
    })
}

pub fn image_url(bytes: &[u8]) -> Result<String> {
    let parts = Array::new();
    parts.push(&Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("image/png");
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(error)?;

    web_sys::Url::create_object_url_with_blob(&blob).map_err(error)
}

pub fn clear_files() {
    FILES.with(|files| {
        for (url, _) in files.borrow_mut().drain() {
            let _ = web_sys::Url::revoke_object_url(&url);
        }
    });
}
