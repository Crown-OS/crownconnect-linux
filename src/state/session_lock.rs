//! Locking this computer's screen when a paired device asks, through logind.

use llts_signaling::message::Lock;
use tokio::sync::mpsc;

/// logind resolves this to the session of the caller.
const CALLER_SESSION: &str = "auto";

#[derive(Debug, thiserror::Error)]
pub enum SessionLockError {
    #[error("system bus: {0}")]
    Bus(#[from] zbus::Error),
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    fn lock_session(&self, session_id: &str) -> zbus::Result<()>;
}

/// Locks the session for every request until `requests` closes.
///
/// # Errors
///
/// Fails without a system bus.
pub async fn run(mut requests: mpsc::Receiver<Lock>) -> Result<(), SessionLockError> {
    let connection = zbus::Connection::system().await?;
    let manager = LoginManagerProxy::new(&connection).await?;
    while requests.recv().await.is_some() {
        if let Err(error) = manager.lock_session(CALLER_SESSION).await {
            tracing::warn!(%error, "cannot lock the session");
        }
    }
    Ok(())
}
