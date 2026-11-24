pub mod api;
#[cfg(not(target_arch = "wasm32"))]
pub mod auth;
#[cfg(target_arch = "wasm32")]
#[path = "browser/auth.rs"]
pub mod auth;
pub mod login;
pub mod model;
pub mod navigation;
pub mod ocr;
pub mod text;
#[cfg(not(target_arch = "wasm32"))]
pub mod tracking;
#[cfg(target_arch = "wasm32")]
#[path = "browser/tracking.rs"]
pub mod tracking;
pub mod tui;
#[cfg(not(target_arch = "wasm32"))]
pub mod vault;
#[cfg(target_arch = "wasm32")]
#[path = "browser/vault.rs"]
pub mod vault;
#[cfg(not(target_arch = "wasm32"))]
pub mod web;

mod input;
mod runtime;
mod session;
mod tracking_model;

#[cfg(target_arch = "wasm32")]
mod browser;
