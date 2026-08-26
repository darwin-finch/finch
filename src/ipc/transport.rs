//! Unix socket path helpers and accept loop.

use std::path::PathBuf;

#[cfg(test)]
static TEST_SOCK_PATH: std::sync::LazyLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[cfg(test)]
pub(crate) struct TestSockPath(Option<PathBuf>);

#[cfg(test)]
impl Drop for TestSockPath {
    fn drop(&mut self) {
        *TEST_SOCK_PATH.lock().expect("test socket path lock poisoned") = self.0.take();
    }
}

#[cfg(test)]
pub(crate) fn set_test_sock_path(path: PathBuf) -> TestSockPath {
    let previous = TEST_SOCK_PATH
        .lock()
        .expect("test socket path lock poisoned")
        .replace(path);
    TestSockPath(previous)
}

/// Default path for the IPC Unix domain socket.
pub const DAEMON_SOCK_PATH: &str = "~/.finch/daemon.sock";

/// Expand `~/` prefix in a socket path.
pub fn sock_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_SOCK_PATH
        .lock()
        .expect("test socket path lock poisoned")
        .clone()
    {
        return path;
    }
    let raw = DAEMON_SOCK_PATH;
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}
