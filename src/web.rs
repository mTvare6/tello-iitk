use crate::{
    auth,
    tui::{BrowserApp, BrowserEffect},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use zeroize::Zeroizing;

#[derive(Clone)]
struct Server {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<Session>>>>>,
    port: u16,
    saved_session: PathBuf,
    download_dir: PathBuf,
}

struct Session {
    directory: tempfile::TempDir,
    files: HashMap<String, File>,
    active: bool,
    touched: Instant,
}

#[derive(Clone)]
struct File {
    path: PathBuf,
    inline: bool,
    mime: &'static str,
    _temporary: Option<Arc<tempfile::NamedTempFile>>,
}

struct CancelArchive(Arc<AtomicBool>);

impl Drop for CancelArchive {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Failure(anyhow::Error);

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":self.0.to_string()})),
        )
            .into_response()
    }
}

type WebResult<T> = std::result::Result<T, Failure>;

fn nonce() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("Cannot create browser session"))?;

    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| c.trim().strip_prefix("hello_session="))
}

async fn session(server: &Server, headers: &HeaderMap) -> Result<Arc<Mutex<Session>>> {
    let sessions = server.sessions.lock().await;
    let session = cookie(headers)
        .and_then(|key| sessions.get(key))
        .cloned()
        .context("Browser session expired. Reload to reconnect.")?;
    drop(sessions);

    ensure!(
        session.lock().await.touched.elapsed() < Duration::from_secs(7200),
        "Browser session expired. Reload to reconnect."
    );

    Ok(session)
}

async fn guard(State(server): State<Server>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let expected = format!("http://{host}");
    let valid_host = host == format!("127.0.0.1:{}", server.port)
        || host == format!("localhost:{}", server.port);
    let valid_origin = origin.is_none_or(|value| value == expected);
    let path = request.uri().path();
    let protected = if path == "/socket" {
        origin == Some(expected.as_str())
    } else if path.starts_with("/api/") {
        request
            .headers()
            .get("x-hello-client")
            .is_some_and(|v| v == "1")
    } else {
        true
    };

    if !valid_host || !valid_origin || !protected {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut response = next.run(request).await;

    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "same-origin"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }

    response
}

async fn bootstrap(State(server): State<Server>, headers: HeaderMap) -> WebResult<Response> {
    if session(&server, &headers).await.is_ok() {
        return Ok(bootstrap_response(cookie(&headers).unwrap()));
    }

    let directory = tempfile::Builder::new()
        .prefix("hello-web-session-")
        .tempdir()
        .map_err(anyhow::Error::from)?;

    if auth::load(&server.saved_session).is_ok() {
        std::fs::copy(&server.saved_session, directory.path().join("session.json"))
            .map_err(anyhow::Error::from)?;
    }

    let key = nonce()?;

    let mut sessions = server.sessions.lock().await;
    sessions.retain(|_, state| {
        state.try_lock().map_or(true, |s| {
            s.active || s.touched.elapsed() < Duration::from_secs(7200)
        })
    });

    if sessions.len() >= 32 {
        return Err(anyhow::anyhow!("Too many browser sessions. Restart the server.").into());
    }

    sessions.insert(
        key.clone(),
        Arc::new(Mutex::new(Session {
            directory,
            files: HashMap::new(),
            active: false,
            touched: Instant::now(),
        })),
    );

    Ok(bootstrap_response(&key))
}

fn bootstrap_response(key: &str) -> Response {
    let mut response = Json(json!({"ready":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "hello_session={key}; HttpOnly; SameSite=Strict; Path=/"
        ))
        .unwrap(),
    );

    response
}

async fn upgrade(
    State(server): State<Server>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let session = session(&server, &headers).await?;

    {
        let mut state = session.lock().await;

        if state.active {
            return Err(anyhow::anyhow!("This browser already has an open terminal tab.").into());
        }

        state.active = true;
    }

    let failed_session = session.clone();

    Ok(ws
        .max_message_size(16384)
        .max_frame_size(16384)
        .on_failed_upgrade(move |_| {
            tokio::spawn(async move {
                failed_session.lock().await.active = false;
            });
        })
        .on_upgrade(move |socket| async move {
            let _ = serve_terminal(socket, &server, &session).await;
            session.lock().await.active = false;
        }))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Input {
    RestoreUsername {
        username: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Key {
        key: String,
        ctrl: bool,
        alt: bool,
        shift: bool,
    },
    Text {
        text: Zeroizing<String>,
    },
    Action {
        index: usize,
        choice: usize,
        key: String,
    },
    Pointer {
        x: u16,
        y: u16,
        kind: String,
    },
}

async fn publish(session: &Arc<Mutex<Session>>, file: File) -> Result<String> {
    let key = nonce()?;

    let mut state = session.lock().await;

    if state.files.len() >= 4096 {
        state.files.clear();
    }

    state.files.insert(key.clone(), file);

    Ok(format!("/files/{key}"))
}

async fn effect(session: &Arc<Mutex<Session>>, effect: BrowserEffect) -> Result<serde_json::Value> {
    match effect {
        BrowserEffect::OpenUrl(url) => {
            let parsed = url::Url::parse(&url)?;

            ensure!(
                matches!(parsed.scheme(), "https" | "http")
                    && parsed.host_str() != Some("hello.iitk.ac.in"),
                "Protected links require the platform session"
            );

            Ok(json!({"type":"open","url":url}))
        }
        BrowserEffect::OpenFile(path) => {
            let mime = match path
                .extension()
                .and_then(|s| s.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("pdf") => "application/pdf",
                Some("txt") => "text/plain; charset=utf-8",
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                _ => "application/octet-stream",
            };

            let inline = mime != "application/octet-stream";
            let url = publish(
                session,
                File {
                    path,
                    inline,
                    mime,
                    _temporary: None,
                },
            )
            .await?;

            Ok(json!({"type":if inline {"open"}else{"download"},"url":url}))
        }
        BrowserEffect::Image(bytes) => {
            let extension = if image::guess_format(&bytes)? == image::ImageFormat::Jpeg {
                "jpg"
            } else {
                "png"
            };

            let directory = session.lock().await.directory.path().to_owned();
            let temporary =
                tokio::task::spawn_blocking(move || -> Result<tempfile::NamedTempFile> {
                    use std::io::Write;

                    let mut file = tempfile::Builder::new()
                        .prefix("captcha-")
                        .suffix(&format!(".{extension}"))
                        .tempfile_in(directory)?;
                    file.write_all(&bytes)?;

                    Ok(file)
                })
                .await??;

            let url = publish(
                session,
                File {
                    path: temporary.path().to_owned(),
                    inline: true,
                    mime: if extension == "jpg" {
                        "image/jpeg"
                    } else {
                        "image/png"
                    },
                    _temporary: Some(Arc::new(temporary)),
                },
            )
            .await?;

            Ok(json!({"type":"open","url":url}))
        }
        BrowserEffect::DownloadFiles(files) => {
            ensure!(!files.is_empty(), "No downloaded files");

            if files.len() == 1 {
                let url = publish(
                    session,
                    File {
                        path: files[0].clone(),
                        inline: false,
                        mime: "application/octet-stream",
                        _temporary: None,
                    },
                )
                .await?;

                return Ok(json!({"type":"download","url":url}));
            }

            ensure!(
                files.len() <= 2000,
                "ZIP is limited to 2,000 files; files remain in the download directory"
            );

            let directory = session.lock().await.directory.path().to_owned();
            let cancelled = Arc::new(AtomicBool::new(false));
            let _cancel_on_drop = CancelArchive(cancelled.clone());
            let temporary =
                tokio::task::spawn_blocking(move || -> Result<tempfile::NamedTempFile> {
                    let mut archive = tempfile::Builder::new()
                        .prefix("attachments-")
                        .suffix(".zip")
                        .tempfile_in(directory)?;

                    {
                        let mut zip = zip::ZipWriter::new(archive.as_file_mut());
                        let mut total = 0u64;

                        for path in files {
                            ensure!(!cancelled.load(Ordering::Relaxed),"ZIP preparation cancelled");

                            ensure!(
                                std::fs::symlink_metadata(&path)?.is_file(),
                                "Attachment is no longer a regular file"
                            );

                            let source = std::fs::File::open(&path)?;

                            ensure!(
                                source.metadata()?.len() <= 1024 * 1024 * 1024 - total,
                                "ZIP is limited to 1 GiB; files remain in the download directory"
                            );

                            ensure!(
                                source.metadata()?.is_file(),
                                "Attachment is no longer a regular file"
                            );

                            zip.start_file(
                                path.file_name()
                                    .context("Missing filename")?
                                    .to_string_lossy(),
                                zip::write::SimpleFileOptions::default()
                                    .compression_method(zip::CompressionMethod::Stored)
                                    .unix_permissions(0o600),
                            )?;

                            let mut bounded = std::io::Read::take(source,1024*1024*1024-total+1);
                            let mut buffer=[0u8;65536];

                            loop {
                                use std::io::{Read,Write};

                                ensure!(!cancelled.load(Ordering::Relaxed),"ZIP preparation cancelled");

                                let n=bounded.read(&mut buffer)?;

                                if n==0 {break;}

                                total+=n as u64;

                                ensure!(total<=1024*1024*1024,"ZIP is limited to 1 GiB; files remain in the download directory");

                                zip.write_all(&buffer[..n])?;
                            }
                        }

                        zip.finish()?;
                    }

                    Ok(archive)
                })
                .await??;

            let url = publish(
                session,
                File {
                    path: temporary.path().to_owned(),
                    inline: false,
                    mime: "application/zip",
                    _temporary: Some(Arc::new(temporary)),
                },
            )
            .await?;

            Ok(json!({"type":"download","url":url}))
        }
    }
}

async fn send(socket: &mut WebSocket, message: Message) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), socket.send(message))
        .await
        .context("Browser stopped receiving updates")??;

    Ok(())
}

async fn serve_terminal(
    mut socket: WebSocket,
    server: &Server,
    session: &Arc<Mutex<Session>>,
) -> Result<()> {
    let path = session.lock().await.directory.path().join("session.json");

    let mut app = BrowserApp::new(path, server.download_dir.clone())?;
    let mut last_input = tokio::time::Instant::now();
    let mut last_frame = String::new();
    let mut effects = tokio::task::JoinSet::new();

    loop {
        let frame = app.frame()?;
        let rendered = serde_json::to_string(&json!({"type":"frame","frame":frame}))?;

        if rendered != last_frame {
            send(&mut socket, Message::Text(rendered.clone().into())).await?;
            last_frame = rendered;
        }

        let mut rejected = false;

        for pending in app.effects() {
            if effects.len() >= 4 {
                app.message("Browser file preparation is busy. Try again shortly.".into());
                rejected = true;
                continue;
            }

            let session = session.clone();
            effects.spawn(async move { effect(&session, pending).await });
        }

        if rejected {
            continue;
        }

        tokio::select! {
            message=socket.recv()=> {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => break,
                };
                last_input=tokio::time::Instant::now();
                session.lock().await.touched=Instant::now();
                let input:Input=match serde_json::from_str(&text) {
                    Ok(input)=>input,
                    Err(_)=> { app.message("Invalid browser input. Reload if this persists.".into()); continue; }
                };
                let quit=match input {
                    Input::Resize{cols,rows}=>{app.resize(cols,rows)?;false},
                    Input::Key{key,ctrl,alt,shift}=>app.key(&key,ctrl,alt,shift),
                    Input::Text{text}=>{app.text(&text);false},
                    Input::RestoreUsername{username}=>{app.restore_username(&username);false},
                    Input::Action{index,choice,key}=>app.action(index,choice,&key),
                    Input::Pointer{x,y,kind}=>{app.pointer(x,y,&kind);false},
                };
                if quit { let _=send(&mut socket,Message::Text(json!({"type":"closed"}).to_string().into())).await; break; }
            }
            ()=app.completion()=>{},
            result=effects.join_next(),if !effects.is_empty()=> {
                match result {
                    Some(Ok(Ok(message)))=>send(&mut socket,Message::Text(serde_json::to_string(&message)?.into())).await?,
                    Some(Ok(Err(error)))=>app.message(error.to_string()),
                    Some(Err(_))=>app.message("Could not prepare browser file".into()),
                    None=>{},
                }
            }
            ()=tokio::time::sleep(Duration::from_millis(150)),if app.animating()=>{},
            ()=tokio::time::sleep_until(last_input+Duration::from_secs(7200))=>break,
        }
    }

    let _ = send(&mut socket, Message::Close(None)).await;

    Ok(())
}

async fn file(
    State(server): State<Server>,
    headers: HeaderMap,
    Path(key): Path<String>,
    request: Request,
) -> WebResult<Response> {
    let session = session(&server, &headers).await?;
    let record = session
        .lock()
        .await
        .files
        .get(&key)
        .cloned()
        .context("File link expired")?;

    let response = tower_http::services::ServeFile::new(&record.path)
        .oneshot(request)
        .await
        .map_err(anyhow::Error::from)?;
    let name = record
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let encoded = url::form_urlencoded::byte_serialize(name.as_bytes())
        .collect::<String>()
        .replace('+', "%20");

    let mut response = response.map(|body| {
        Body::from_stream(Body::new(body).into_data_stream().map(move |chunk| {
            let _keep_session = &session;

            chunk
        }))
    });

    if response.status().is_success() {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(record.mime));
    }

    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "{}; filename=\"download\"; filename*=UTF-8''{encoded}",
            if record.inline {
                "inline"
            } else {
                "attachment"
            }
        ))
        .unwrap(),
    );

    Ok(response)
}

pub async fn run(port: u16, saved_session: PathBuf, download_dir: PathBuf) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let port = listener.local_addr()?.port();
    let state = Server {
        sessions: Arc::default(),
        port,
        saved_session,
        download_dir,
    };

    let router = Router::new()
        .route(
            "/",
            get(|| async { axum::response::Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route("/api/bootstrap", get(bootstrap))
        .route("/socket", get(upgrade))
        .route("/files/{key}", get(file))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);

    println!("Open http://127.0.0.1:{port} · Press Ctrl+C to stop");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    Ok(())
}
