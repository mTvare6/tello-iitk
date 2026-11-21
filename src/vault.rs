use anyhow::{Context, Result, bail};
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};
use zeroize::Zeroizing;

const SERVICE: &str = "hello-iitk";

async fn request(
    operation: &str,
    username: &str,
    password: Option<&str>,
) -> Result<Option<Zeroizing<String>>> {
    if username.is_empty() {
        bail!("Enter your username before using the desktop keyring");
    }

    let mut command = Command::new("secret-tool");
    command.arg(operation);

    if operation == "store" {
        command.arg("--label=HelloIITK password");
    }

    command
        .arg("--")
        .args([
            "application",
            SERVICE,
            "host",
            "hello.iitk.ac.in",
            "username",
            username,
        ])
        .stdin(if password.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let operation = async {
        let mut child = command.spawn().context(
            "Desktop keyring unavailable: install secret-tool and enable a Secret Service keyring",
        )?;

        if let Some(password) = password {
            let mut stdin = child
                .stdin
                .take()
                .context("Cannot contact desktop keyring")?;
            stdin.write_all(password.as_bytes()).await?;
            stdin.shutdown().await?;
            drop(stdin);
        }

        let output = child.wait_with_output().await?;

        if !output.status.success() {
            if output.stderr.is_empty() && matches!(operation, "lookup" | "clear") {
                return Ok(None);
            }

            bail!("Desktop keyring is unavailable or locked; no password was saved");
        }

        if operation == "lookup" {
            let bytes = Zeroizing::new(output.stdout);
            let password = std::str::from_utf8(&bytes)
                .map_err(|_| anyhow::anyhow!("Saved password has an unsupported encoding"))?;

            return Ok(Some(Zeroizing::new(
                password.trim_end_matches('\n').to_owned(),
            )));
        }

        Ok(None)
    };

    tokio::time::timeout(Duration::from_secs(60), operation)
        .await
        .context("Desktop keyring timed out; unlock it and retry")?
}

pub async fn load(username: &str) -> Result<Option<Zeroizing<String>>> {
    request("lookup", username, None).await
}

pub async fn save(username: &str, password: &str) -> Result<()> {
    request("store", username, Some(password)).await.map(|_| ())
}

pub async fn forget(username: &str) -> Result<()> {
    request("clear", username, None).await.map(|_| ())
}
