use anyhow::{Result, bail};
use zeroize::Zeroizing;

pub async fn load(_username: &str) -> Result<Option<Zeroizing<String>>> {
    bail!("Password saving is available in the desktop client")
}

pub async fn save(_username: &str, _password: &str) -> Result<()> {
    bail!("Password saving is available in the desktop client")
}

pub async fn forget(_username: &str) -> Result<()> {
    bail!("Password saving is available in the desktop client")
}
