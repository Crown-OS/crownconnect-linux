use std::sync::mpsc::{SyncSender, TrySendError};

/// Where a wayland worker hands its events, never blocking the worker: a full or closed
/// receiver gives the event back, so a frame that nobody takes can still be released.
pub type EventOutlet<E> = Box<dyn Fn(E) -> Result<(), E> + Send>;

/// An outlet into a bounded channel.
pub fn channel_outlet<E: Send + 'static>(sender: SyncSender<E>) -> EventOutlet<E> {
    Box::new(move |event| {
        sender.try_send(event).map_err(|error| match error {
            TrySendError::Full(event) | TrySendError::Disconnected(event) => event,
        })
    })
}
