pub mod http;

use crate::tui::{BrowserApp, BrowserEffect};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

fn error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Input {
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
        text: String,
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
    RestoreUsername {
        username: String,
    },
}

#[wasm_bindgen]
pub struct Client {
    app: BrowserApp,
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(bootstrap: &str) -> Result<Client, JsValue> {
        http::set_account(&serde_json::from_str(bootstrap).map_err(error)?);
        Ok(Self {
            app: BrowserApp::new("session".into(), "downloads".into()).map_err(error)?,
        })
    }

    pub fn input(&mut self, json: &str) -> Result<bool, JsValue> {
        match serde_json::from_str(json).map_err(error)? {
            Input::Resize { cols, rows } => self.app.resize(cols, rows).map_err(error)?,
            Input::Key {
                key,
                ctrl,
                alt,
                shift,
            } => return Ok(self.app.key(&key, ctrl, alt, shift)),
            Input::Text { text } => self.app.text(&text),
            Input::Action { index, choice, key } => {
                return Ok(self.app.action(index, choice, &key));
            }
            Input::Pointer { x, y, kind } => self.app.pointer(x, y, &kind),
            Input::RestoreUsername { username } => self.app.restore_username(&username),
        }
        Ok(false)
    }

    pub fn frame(&mut self) -> Result<String, JsValue> {
        self.app.poll();
        serde_json::to_string(&self.app.frame().map_err(error)?).map_err(error)
    }

    pub fn busy(&self) -> bool {
        self.app.animating()
    }

    pub fn effects(&mut self) -> Result<String, JsValue> {
        let mut effects = Vec::new();
        for effect in self.app.effects() {
            effects.push(match effect {
                BrowserEffect::OpenUrl(url)=>serde_json::json!({"type":"open","url":url}),
                BrowserEffect::OpenFile(path)=>serde_json::json!({"type":"open","url":path.to_string_lossy()}),
                BrowserEffect::DownloadFiles(name, paths)=>serde_json::json!({"type":"files","name":name,"files":paths.into_iter().map(|path|serde_json::json!({"url":path.to_string_lossy(),"name":http::file_name(&path)})).collect::<Vec<_>>()}),
                BrowserEffect::Image(bytes)=>serde_json::json!({"type":"open","url":http::image_url(&bytes).map_err(error)?}),
            });
        }
        serde_json::to_string(&effects).map_err(error)
    }

    pub fn message(&mut self, text: String) {
        self.app.message(text);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        http::clear_files();
    }
}
