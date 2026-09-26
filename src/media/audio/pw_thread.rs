use std::sync::mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{Object, Value};
use pw::spa::sys::{SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_CHANNEL_MONO};
use pw::spa::utils::SpaTypes;

use super::opus::{VoiceChannels, SAMPLE_RATE};
use crate::media::MediaError;

/// A PipeWire main loop on its own thread, running whatever `setup` connected to it until
/// dropped.
pub(crate) struct PipeWireThread {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl PipeWireThread {
    pub(crate) fn spawn<Setup, Keep>(name: &str, setup: Setup) -> Result<Self, MediaError>
    where
        Setup: FnOnce(&pw::core::CoreRc) -> Result<Keep, MediaError> + Send + 'static,
        Keep: 'static,
    {
        let (quit, quit_receiver) = pw::channel::channel::<()>();
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let notify = ready_sender.clone();
                if let Err(error) = run_loop(setup, quit_receiver, &notify) {
                    let _ = ready_sender.send(Err(error));
                }
            })?;
        let mut spawned = Self {
            quit,
            thread: Some(thread),
        };
        match ready.recv() {
            Ok(Ok(())) => Ok(spawned),
            Ok(Err(error)) => {
                spawned.join();
                Err(error)
            }
            Err(_) => {
                spawned.join();
                Err(MediaError::PipeWireThreadGone)
            }
        }
    }

    fn join(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for PipeWireThread {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PipeWireThread")
            .field("thread", &self.thread)
            .finish_non_exhaustive()
    }
}

impl Drop for PipeWireThread {
    fn drop(&mut self) {
        self.join();
    }
}

fn run_loop<Setup, Keep>(
    setup: Setup,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), MediaError>>,
) -> Result<(), MediaError>
where
    Setup: FnOnce(&pw::core::CoreRc) -> Result<Keep, MediaError>,
{
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let running = setup(&core)?;
    let quitting_loop = main_loop.clone();
    let _quit = quit.attach(main_loop.loop_(), move |()| quitting_loop.quit());
    let _ = ready.send(Ok(()));
    main_loop.run();
    drop(running);
    Ok(())
}

/// The 48 kHz interleaved f32 format both voice nodes use, as an `EnumFormat` pod.
pub(crate) fn voice_format_pod(channels: VoiceChannels) -> Result<Vec<u8>, MediaError> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(u32::try_from(channels.count()).unwrap_or(1));
    let mut position = [0; MAX_CHANNELS];
    let layout: &[u32] = match channels {
        VoiceChannels::Mono => &[SPA_AUDIO_CHANNEL_MONO],
        VoiceChannels::Stereo => &[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR],
    };
    position
        .iter_mut()
        .zip(layout)
        .for_each(|(slot, &channel)| *slot = channel);
    info.set_position(position);
    let format = Value::Object(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    });
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &format)
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|_| MediaError::Unsupported("could not serialise the audio format"))
}

/// Keeps only characters PipeWire property values and node names accept.
pub(crate) fn sanitized(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}
