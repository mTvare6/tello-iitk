#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;
#[cfg(not(target_arch = "wasm32"))]
pub use tokio::{spawn, sync, task, time};

#[cfg(target_arch = "wasm32")]
pub use web_time::Instant;

#[cfg(not(target_arch = "wasm32"))]
pub fn file_exists(path: &std::path::Path) -> bool {
    path.exists()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open(path: impl AsRef<std::ffi::OsStr>) -> anyhow::Result<()> {
    open::that_detached(path)?;

    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn file_exists(path: &std::path::Path) -> bool {
    crate::browser::http::file_exists(path)
}

#[cfg(target_arch = "wasm32")]
pub fn open(_path: impl AsRef<std::ffi::OsStr>) -> anyhow::Result<()> {
    anyhow::bail!("Open this item in the browser")
}

#[cfg(target_arch = "wasm32")]
pub mod task {
    use futures_util::future::AbortHandle;
    use std::marker::PhantomData;

    pub struct JoinHandle<T> {
        pub(super) abort: AbortHandle,
        pub(super) result: PhantomData<T>,
    }

    impl<T> JoinHandle<T> {
        pub fn abort(&self) {
            self.abort.abort();
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn spawn<F: std::future::Future + 'static>(future: F) -> task::JoinHandle<F::Output> {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(future, registration).await;
    });

    task::JoinHandle {
        abort,
        result: std::marker::PhantomData,
    }
}

#[cfg(target_arch = "wasm32")]
pub mod time {
    pub async fn sleep(duration: std::time::Duration) {
        gloo_timers::future::TimeoutFuture::new(duration.as_millis().min(u32::MAX as u128) as u32)
            .await;
    }
}

#[cfg(target_arch = "wasm32")]
pub mod sync {
    pub mod mpsc {
        pub use tokio::sync::mpsc::UnboundedReceiver;

        pub struct UnboundedSender<T>(tokio::sync::mpsc::UnboundedSender<T>);

        impl<T> Clone for UnboundedSender<T> {
            fn clone(&self) -> Self {
                Self(self.0.clone())
            }
        }

        impl<T> UnboundedSender<T> {
            pub fn send(&self, value: T) -> Result<(), tokio::sync::mpsc::error::SendError<T>> {
                self.0.send(value)?;

                if let Some(window) = web_sys::window()
                    && let Ok(event) = web_sys::Event::new("hello-update")
                {
                    let _ = window.dispatch_event(&event);
                }

                Ok(())
            }
        }

        pub fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

            (UnboundedSender(tx), rx)
        }
    }
}
