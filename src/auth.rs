use anyhow::{Context, Result, bail};
use reqwest::cookie::Jar;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use url::Url;

pub const ORIGIN: &str = "https://hello.iitk.ac.in";

#[derive(Debug)]
pub struct AccessDenied;

impl std::fmt::Display for AccessDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session expired or access denied. Sign in again to continue"
        )
    }
}

impl std::error::Error for AccessDenied {}

#[derive(Deserialize, Serialize)]
pub(crate) struct Session {
    pub(crate) cookies: Vec<SessionCookie>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SessionCookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) domain: String,
    pub(crate) path: String,
    pub(crate) expires: f64,
    pub(crate) secure: bool,
}

pub(crate) fn save(path: &Path, session: &Session) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }

    builder.create(parent)?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temporary.as_file_mut(), session)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;

    Ok(())
}

pub fn session_path() -> Result<PathBuf> {
    Ok(
        directories::ProjectDirs::from("in.ac.iitk", "hello", "hello-iitk")
            .context("Cannot determine local data directory")?
            .data_local_dir()
            .join("session.json"),
    )
}

pub fn load(path: &Path) -> Result<Arc<Jar>> {
    let metadata =
        std::fs::symlink_metadata(path).context("No session. Run `hello-iitk login` first")?;

    if !metadata.is_file() {
        bail!("Session must be a regular file");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("Session file is accessible to other users. Set its permissions to 600");
        }
    }

    let session: Session =
        serde_json::from_slice(&std::fs::read(path)?).context("Invalid session file")?;

    let jar = Arc::new(Jar::default());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs_f64();

    let base = Url::parse(ORIGIN)?;

    let mut count = 0;

    for c in session.cookies {
        if !matches!(c.domain.as_str(), "hello.iitk.ac.in" | ".hello.iitk.ac.in") {
            continue;
        }

        if c.expires > 0.0 && c.expires < now {
            continue;
        }

        if c.name.contains(['\r', '\n', ';', '='])
            || c.value.contains(['\r', '\n', ';'])
            || c.path.contains(['\r', '\n', ';'])
        {
            bail!("Invalid session cookie");
        }

        let mut value = format!("{}={}; Path={}", c.name, c.value, c.path);

        if c.secure {
            value.push_str("; Secure");
        }

        jar.add_cookie_str(&value, &base);
        count += 1;
    }

    if count == 0 {
        bail!("Session has no unexpired HelloIITK cookies. Run `hello-iitk login`");
    }

    Ok(jar)
}

pub fn identity(path: &Path) -> Result<(String, String)> {
    let metadata = std::fs::symlink_metadata(path)?;

    if !metadata.is_file() {
        bail!("Session must be a regular file");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("Session file is not private");
        }
    }

    let session: Session = serde_json::from_slice(&std::fs::read(path)?)?;
    let uid = session
        .cookies
        .iter()
        .find(|c| {
            c.name == "uid" && matches!(c.domain.as_str(), "hello.iitk.ac.in" | ".hello.iitk.ac.in")
        })
        .map(|c| c.value.clone())
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit()))
        .context("Session has no account identity")?;

    let username = session
        .cookies
        .iter()
        .find(|c| {
            c.name == "username"
                && matches!(c.domain.as_str(), "hello.iitk.ac.in" | ".hello.iitk.ac.in")
        })
        .map(|c| c.value.clone())
        .unwrap_or_default();

    Ok((uid, username))
}
