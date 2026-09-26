use llts_signaling::message::{Lock, Media, SetHotspot, SetVolume};
use tokio::sync::mpsc;

use crate::state::ClipboardSnapshot;

const COMMAND_QUEUE_DEPTH: usize = 8;

/// Handles through which a paired device's commands reach this computer's services.
#[derive(Debug, Clone)]
pub struct LocalControls {
    pub media: mpsc::Sender<Media>,
    pub volume: mpsc::Sender<SetVolume>,
    pub hotspot: mpsc::Sender<SetHotspot>,
    /// A clipboard copied on another device, to become this computer's selection.
    pub clipboard: mpsc::Sender<ClipboardSnapshot>,
    pub lock: mpsc::Sender<Lock>,
}

/// The publisher ends of [`LocalControls`].
#[derive(Debug)]
pub struct LocalCommandReceivers {
    pub media: mpsc::Receiver<Media>,
    pub volume: mpsc::Receiver<SetVolume>,
    pub hotspot: mpsc::Receiver<SetHotspot>,
    pub clipboard: mpsc::Receiver<ClipboardSnapshot>,
    pub lock: mpsc::Receiver<Lock>,
}

impl LocalControls {
    pub fn channels() -> (Self, LocalCommandReceivers) {
        let (media, media_receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (volume, volume_receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (hotspot, hotspot_receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (clipboard, clipboard_receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (lock, lock_receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        (
            Self {
                media,
                volume,
                hotspot,
                clipboard,
                lock,
            },
            LocalCommandReceivers {
                media: media_receiver,
                volume: volume_receiver,
                hotspot: hotspot_receiver,
                clipboard: clipboard_receiver,
                lock: lock_receiver,
            },
        )
    }
}
