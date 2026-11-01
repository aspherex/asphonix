//! Shared tokio runtime shim for the vendored code that used `tauri::async_runtime`.
use std::future::Future;
use std::sync::OnceLock;

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(64).enable_all().build().expect("tokio"))
}

pub fn spawn<F: Future + Send + 'static>(f: F) -> tokio::task::JoinHandle<F::Output>
where
    F::Output: Send + 'static,
{
    rt().spawn(f)
}

pub fn spawn_blocking<F: FnOnce() -> R + Send + 'static, R: Send + 'static>(f: F) -> tokio::task::JoinHandle<R> {
    rt().spawn_blocking(f)
}

pub fn block_on<F: Future>(f: F) -> F::Output {
    rt().block_on(f)
}
