//! Loading what the runtime needs from disk and binding the llts socket.

use std::collections::BTreeMap;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use llts_node::bridge::driver_class;
use llts_node::{KnownPeer, LocalDevice, Node, NodeConfig};
use llts_signaling::device::{DeviceClass, DeviceId, FeatureSet};
use llts_signaling::message::{AudioCodec, Capabilities, CodecList, VideoCodec};
use llts_transport::pairing::{FileTrustStore, TrustStore};
use llts_transport::session::{Driver, DriverConfig};

use super::active_streams::ActiveStreams;
use super::calls::HandsFreeCalls;
use super::clock::RuntimeClock;
use super::event_loop::{PeerLoop, PendingReplies};
use super::{PeerRuntimeError, PeerServices};
use crate::config::DaemonConfig;
use crate::ipc::proto::Feature;
use crate::pairing::ffsp_bridge::FfspLinkStore;
use crate::peer::convert::signaling_feature;
use crate::peer::feature_store::FeatureStore;
use crate::peer::link_monitor::LinkMonitor;
use crate::peer::wakeup::{RuntimeWaker, Wakeup};
use crate::peer::{identity, PeerCommand};
use crate::util::error_chain::error_chain;

const PRODUCT_NAME: &str = "/sys/devices/virtual/dmi/id/product_name";
const FALLBACK_MODEL: &str = "Linux computer";
/// Hardware HEVC first, H.264 wherever HEVC is missing on either end.
const VIDEO_CODECS: [VideoCodec; 2] = [VideoCodec::Hevc, VideoCodec::H264];
const AUDIO_CODECS: [AudioCodec; 1] = [AudioCodec::Opus];

fn model() -> String {
    std::fs::read_to_string(PRODUCT_NAME)
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_MODEL.to_owned())
}

fn signaling_features(features: crate::ipc::proto::FeatureSet) -> FeatureSet {
    features.iter().map(signaling_feature).collect()
}

fn local_device(config: &DaemonConfig) -> LocalDevice {
    LocalDevice {
        name: config.device_name.clone(),
        model: model(),
        capabilities: Capabilities {
            video_codecs: CodecList::from_slice(&VIDEO_CODECS).unwrap_or_default(),
            audio_codecs: CodecList::from_slice(&AUDIO_CODECS).unwrap_or_default(),
            features: signaling_features(Feature::ALL.into_iter().collect()),
            device_class: DeviceClass::Computer,
        },
    }
}

fn feature_store(config: &DaemonConfig) -> FeatureStore {
    let path = config.paths.peer_features.clone();
    FeatureStore::open(path.clone()).unwrap_or_else(|error| {
        tracing::warn!(error = %error_chain(&error), "starting with default feature choices");
        FeatureStore::empty(path)
    })
}

impl PeerLoop {
    pub(super) fn open(
        config: &DaemonConfig,
        services: PeerServices,
        commands: std_mpsc::Receiver<PeerCommand>,
        wakeup: Arc<Wakeup>,
    ) -> Result<Self, PeerRuntimeError> {
        let ffsp = FfspLinkStore::new(&config.paths.ffsp_links);
        let identity = identity::load_or_create(&config.paths.identity, &ffsp)?;
        let trust = FileTrustStore::open(&config.paths.trust_store)?;
        let features = feature_store(config);
        let default_features = signaling_features(config.default_features);
        let known: Vec<KnownPeer> = trust
            .peers()
            .iter()
            .map(|peer| {
                let enabled = features
                    .enabled(&DeviceId(peer.static_key.0))
                    .unwrap_or(default_features);
                KnownPeer::from_trusted(peer, enabled)
            })
            .collect();
        let clock = RuntimeClock::start();
        let node = Node::new(
            &identity,
            local_device(config),
            known,
            NodeConfig {
                default_features,
                ..NodeConfig::default()
            },
            clock.now(),
        )?;
        let driver = Driver::bind(
            config.llts_address(),
            identity,
            trust,
            DriverConfig::new(&config.device_name, driver_class(DeviceClass::Computer)),
        )?;
        let links = LinkMonitor::open(driver.local_addr()?)?;
        tracing::info!(address = %driver.local_addr()?, id = %node.id(), "llts listening");
        let PeerServices {
            daemon_events,
            controls,
            advertiser,
            telephony,
            mut media,
        } = services;
        media.attach(RuntimeWaker::new(Arc::clone(&wakeup)));
        Ok(Self {
            driver,
            node,
            clock,
            services: super::event_loop::Services {
                daemon_events,
                controls,
                advertiser,
                telephony,
            },
            streams: ActiveStreams::new(media),
            features,
            ffsp,
            static_peers: config.static_peers.clone(),
            links,
            hands_free: HandsFreeCalls::default(),
            listed: BTreeMap::new(),
            pending: PendingReplies::default(),
            failures: Vec::new(),
            applied_clipboard: None,
            commands,
            wakeup,
            stopping: false,
        })
    }
}
