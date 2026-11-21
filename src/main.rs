use anyhow::Result;
use clap::{Parser, Subcommand};
use hello_iitk::{auth, tracking, tui, vault, web};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Read-only terminal and web client for HelloIITK")]
struct Cli {
    /// Session file (owner-only permissions).
    #[arg(long, global = true)]
    session: Option<PathBuf>,
    /// Save files here (defaults to the current directory).
    #[arg(long, global = true)]
    download_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the web client on localhost.
    Web {
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    /// Sign in from the terminal.
    Login,
    /// Delete the saved session.
    Logout,
    /// Remove a password from the desktop keyring.
    ForgetPassword {
        /// Username (defaults to the last account).
        username: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let session = cli.session.unwrap_or(auth::session_path()?);
    let download_dir = cli.download_dir.unwrap_or(std::env::current_dir()?);

    match cli.command {
        Some(Command::Web { port }) => web::run(port, session, download_dir).await?,
        Some(Command::Login) => tui::run_login(session, download_dir).await?,
        Some(Command::Logout) => {
            if session.exists() {
                std::fs::remove_file(session)?;
            }

            println!("Local session removed.");
        }
        Some(Command::ForgetPassword { username }) => {
            let mut preferences = tracking::Preferences::load()?;
            let username = username
                .or_else(|| auth::identity(&session).ok().map(|(_, name)| name))
                .unwrap_or_else(|| preferences.username.clone());
            vault::forget(&username).await?;

            if preferences.username == username {
                preferences.save_password = false;
                preferences.save()?;
            }

            println!("Saved desktop password removed.");
        }
        None => tui::run(session, download_dir).await?,
    }

    Ok(())
}
