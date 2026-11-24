use anyhow::{Result, bail};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

pub use crate::session::{AccessDenied, ORIGIN};

thread_local! {
    static ACCOUNT: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

pub(crate) fn set_account(account: Option<(String, String)>) {
    ACCOUNT.with(|value| *value.borrow_mut() = account);
}

pub fn session_path() -> Result<PathBuf> {
    Ok(PathBuf::from("session"))
}

pub fn identity(_path: &Path) -> Result<(String, String)> {
    ACCOUNT.with(|value| match &*value.borrow() {
        Some(account) => Ok(account.clone()),
        None => bail!("Sign in to open your courses"),
    })
}
