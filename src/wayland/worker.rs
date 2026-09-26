use std::os::fd::OwnedFd;
use std::sync::mpsc;
use std::thread::JoinHandle;

use rustix::event::{eventfd, poll, EventfdFlags, PollFd, PollFlags};
use tokio::sync::oneshot;
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

use super::WaylandError;

type Started = oneshot::Receiver<Result<(), WaylandError>>;

/// The state a wayland worker thread owns: its protocol objects and where it sends events.
pub(crate) trait WorkerState:
    Dispatch<WlRegistry, GlobalListContents> + Sized + 'static
{
    type Command: Send + 'static;

    fn apply(&mut self, command: Self::Command, queue: &QueueHandle<Self>);

    /// Ends the thread, as when the compositor withdrew the object it serves.
    fn is_finished(&self) -> bool {
        false
    }
}

/// A wayland connection and event queue on a dedicated thread, driven by commands.
///
/// Dropping the handle stops the thread and destroys everything it created.
#[derive(Debug)]
pub(crate) struct WorkerHandle<C> {
    commands: Option<mpsc::Sender<C>>,
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl<C: Send + 'static> WorkerHandle<C> {
    /// Connects, lets `init` bind what it needs, and resolves once the first roundtrip is done.
    pub(crate) async fn spawn<S, Init>(name: &str, init: Init) -> Result<Self, WaylandError>
    where
        S: WorkerState<Command = C>,
        Init: FnOnce(&GlobalList, &QueueHandle<S>) -> Result<S, WaylandError> + Send + 'static,
    {
        let (handle, started) = Self::launch(name, init)?;
        started.await.map_err(|_| WaylandError::ThreadGone)??;
        Ok(handle)
    }

    /// [`WorkerHandle::spawn`] for a caller on a plain thread, never on the async runtime.
    pub(crate) fn spawn_blocking<S, Init>(name: &str, init: Init) -> Result<Self, WaylandError>
    where
        S: WorkerState<Command = C>,
        Init: FnOnce(&GlobalList, &QueueHandle<S>) -> Result<S, WaylandError> + Send + 'static,
    {
        let (handle, started) = Self::launch(name, init)?;
        started
            .blocking_recv()
            .map_err(|_| WaylandError::ThreadGone)??;
        Ok(handle)
    }

    fn launch<S, Init>(name: &str, init: Init) -> Result<(Self, Started), WaylandError>
    where
        S: WorkerState<Command = C>,
        Init: FnOnce(&GlobalList, &QueueHandle<S>) -> Result<S, WaylandError> + Send + 'static,
    {
        let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        let thread_wake = wake.try_clone()?;
        let (commands, receiver) = mpsc::channel();
        let (ready, started) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let mut ready = Some(ready);
                let outcome = run(init, &receiver, &thread_wake, &mut ready);
                match (outcome, ready.take()) {
                    (Err(error), Some(ready)) => {
                        let _ = ready.send(Err(error));
                    }
                    (Err(error), None) => tracing::warn!(%error, "wayland worker stopped"),
                    (Ok(()), _) => {}
                }
            })?;
        let handle = Self {
            commands: Some(commands),
            wake,
            thread: Some(thread),
        };
        Ok((handle, started))
    }

    /// # Errors
    ///
    /// Fails once the thread has exited.
    pub(crate) fn send(&self, command: C) -> Result<(), WaylandError> {
        self.commands
            .as_ref()
            .ok_or(WaylandError::ThreadGone)?
            .send(command)
            .map_err(|_| WaylandError::ThreadGone)?;
        wake(&self.wake);
        Ok(())
    }
}

impl<C> Drop for WorkerHandle<C> {
    fn drop(&mut self) {
        self.commands = None;
        wake(&self.wake);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn wake(eventfd: &OwnedFd) {
    let _ = rustix::io::write(eventfd, &1_u64.to_ne_bytes());
}

fn run<S, Init>(
    init: Init,
    commands: &mpsc::Receiver<S::Command>,
    wake: &OwnedFd,
    ready: &mut Option<oneshot::Sender<Result<(), WaylandError>>>,
) -> Result<(), WaylandError>
where
    S: WorkerState,
    Init: FnOnce(&GlobalList, &QueueHandle<S>) -> Result<S, WaylandError>,
{
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<S>(&connection)?;
    let handle = queue.handle();
    let mut state = init(&globals, &handle)?;
    queue.roundtrip(&mut state)?;
    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(()));
    }
    loop {
        loop {
            match commands.try_recv() {
                Ok(command) => state.apply(command, &handle),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if state.is_finished() {
            return Ok(());
        }
        wait(&mut queue, &mut state, wake)?;
    }
}

fn wait<S: WorkerState>(
    queue: &mut EventQueue<S>,
    state: &mut S,
    wake: &OwnedFd,
) -> Result<(), WaylandError> {
    queue.flush()?;
    queue.dispatch_pending(state)?;
    queue.flush()?;
    let Some(guard) = queue.prepare_read() else {
        return Ok(());
    };
    let (wayland_ready, woken) = {
        let wayland_fd = guard.connection_fd();
        let mut fds = [
            PollFd::new(&wayland_fd, PollFlags::IN),
            PollFd::new(wake, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
        let [wayland, woken] = fds.map(|fd| !fd.revents().is_empty());
        (wayland, woken)
    };
    if woken {
        let _ = rustix::io::read(wake, &mut [0_u8; 8]);
    }
    if wayland_ready {
        guard.read()?;
    } else {
        drop(guard);
    }
    queue.dispatch_pending(state)?;
    Ok(())
}
