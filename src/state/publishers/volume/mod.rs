//! The default audio sink's volume, from the PipeWire graph, and setting it from a peer.

mod graph;
mod props;

use std::sync::mpsc as std_mpsc;

use llts_signaling::message::SetVolume;
use pipewire as pw;
use tokio::sync::mpsc;

use crate::state::StateSink;
use graph::GraphCommand;

#[derive(Debug, thiserror::Error)]
pub enum VolumeError {
    #[error("pipewire: {0}")]
    PipeWire(#[from] pw::Error),
    #[error("cannot start the PipeWire thread: {0}")]
    Thread(#[from] std::io::Error),
    #[error("the PipeWire thread exited before connecting")]
    ThreadGone,
}

/// Publishes the default sink's volume from a PipeWire thread and forwards peer volume changes
/// to it, until `commands` closes.
///
/// # Errors
///
/// Fails when PipeWire is not running.
pub async fn run(
    sink: StateSink,
    mut commands: mpsc::Receiver<SetVolume>,
) -> Result<(), VolumeError> {
    let (graph_commands, graph_receiver) = pw::channel::channel();
    let (ready_sender, ready) = std_mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("crownconnect-volume".into())
        .spawn(move || {
            if let Err(error) = graph::run(sink, graph_receiver, &ready_sender) {
                let _ = ready_sender.send(Err(error));
            }
        })?;
    let started = tokio::task::spawn_blocking(move || ready.recv())
        .await
        .map_err(|_| VolumeError::ThreadGone)?;
    match started {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(VolumeError::ThreadGone),
    }
    let _stop = StopGraph {
        commands: graph_commands.clone(),
        thread: Some(thread),
    };
    while let Some(volume) = commands.recv().await {
        if graph_commands.send(GraphCommand::Set(volume)).is_err() {
            return Err(VolumeError::ThreadGone);
        }
    }
    Ok(())
}

/// Stops the PipeWire thread however the task ends, including being aborted.
struct StopGraph {
    commands: pw::channel::Sender<GraphCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StopGraph {
    fn drop(&mut self) {
        let _ = self.commands.send(GraphCommand::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
