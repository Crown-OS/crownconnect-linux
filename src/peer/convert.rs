//! Crossings between the IPC schema and llts types, which describe the same things separately
//! so neither crate depends on the other.

use llts_node::FeatureState as NodeFeatureState;
use llts_signaling::device::{
    DeviceClass as SignalingClass, DeviceId as SignalingId, Feature as SignalingFeature,
    FeatureSet as SignalingFeatureSet,
};
use llts_signaling::message::{
    CameraLens as SignalingLens, Direction as SignalingDirection, Edge as SignalingEdge,
    StartCamera, StartMirror, StartMonitor, VideoLimits as SignalingLimits,
};

use crate::ipc::proto::{
    CameraLens, CameraOptions, DeviceClass, DeviceId, Direction, Edge, Feature, FeatureSet,
    FeatureState, MirrorOptions, MonitorOptions, VideoLimits,
};

/// A monitor is drawn at 1:1; scale is in 1/120 steps.
const UNSCALED: u16 = 120;
const MILLIHERTZ_PER_HERTZ: u32 = 1_000;

pub(crate) const fn signaling_id(id: DeviceId) -> SignalingId {
    SignalingId(id.0)
}

pub(crate) const fn ipc_id(id: SignalingId) -> DeviceId {
    DeviceId(id.0)
}

pub(crate) const fn signaling_feature(feature: Feature) -> SignalingFeature {
    match feature {
        Feature::Mirror => SignalingFeature::Mirror,
        Feature::Camera => SignalingFeature::Camera,
        Feature::Mic => SignalingFeature::Mic,
        Feature::Monitor => SignalingFeature::Monitor,
        Feature::Unicursor => SignalingFeature::Unicursor,
        Feature::Calls => SignalingFeature::Calls,
        Feature::Clipboard => SignalingFeature::Clipboard,
        Feature::Notifications => SignalingFeature::Notifications,
        Feature::Battery => SignalingFeature::Battery,
        Feature::Hotspot => SignalingFeature::Hotspot,
        Feature::Files => SignalingFeature::Files,
    }
}

pub(crate) const fn ipc_feature(feature: SignalingFeature) -> Feature {
    match feature {
        SignalingFeature::Mirror => Feature::Mirror,
        SignalingFeature::Camera => Feature::Camera,
        SignalingFeature::Mic => Feature::Mic,
        SignalingFeature::Monitor => Feature::Monitor,
        SignalingFeature::Unicursor => Feature::Unicursor,
        SignalingFeature::Calls => Feature::Calls,
        SignalingFeature::Clipboard => Feature::Clipboard,
        SignalingFeature::Notifications => Feature::Notifications,
        SignalingFeature::Battery => Feature::Battery,
        SignalingFeature::Hotspot => Feature::Hotspot,
        SignalingFeature::Files => Feature::Files,
    }
}

pub(crate) fn ipc_features(features: SignalingFeatureSet) -> FeatureSet {
    features.iter().map(ipc_feature).collect()
}

pub(crate) const fn ipc_class(class: SignalingClass) -> DeviceClass {
    match class {
        SignalingClass::Phone => DeviceClass::Phone,
        SignalingClass::Tablet => DeviceClass::Tablet,
        SignalingClass::Watch => DeviceClass::Watch,
        SignalingClass::Computer => DeviceClass::Computer,
    }
}

pub(crate) const fn ipc_feature_state(state: NodeFeatureState) -> FeatureState {
    match state {
        NodeFeatureState::Disabled => FeatureState::Disabled,
        NodeFeatureState::Enabled => FeatureState::Enabled,
        NodeFeatureState::Active => FeatureState::Active,
    }
}

const fn signaling_limits(limits: VideoLimits) -> SignalingLimits {
    SignalingLimits {
        max_width: limits.max_width,
        max_height: limits.max_height,
        max_fps: limits.max_fps,
    }
}

pub(crate) const fn signaling_edge(edge: Edge) -> SignalingEdge {
    match edge {
        Edge::Left => SignalingEdge::Left,
        Edge::Right => SignalingEdge::Right,
        Edge::Top => SignalingEdge::Top,
        Edge::Bottom => SignalingEdge::Bottom,
    }
}

pub(crate) const fn start_mirror(options: MirrorOptions) -> StartMirror {
    StartMirror {
        direction: match options.direction {
            Direction::ToPeer => SignalingDirection::ToPeer,
            Direction::FromPeer => SignalingDirection::FromPeer,
        },
        limits: signaling_limits(options.limits),
        remote_input: options.remote_input,
    }
}

pub(crate) const fn start_camera(options: CameraOptions) -> StartCamera {
    StartCamera {
        lens: match options.lens {
            CameraLens::Back => SignalingLens::Back,
            CameraLens::Front => SignalingLens::Front,
        },
        limits: signaling_limits(options.limits),
    }
}

pub(crate) fn start_monitor(options: MonitorOptions) -> StartMonitor {
    StartMonitor {
        width: options.limits.max_width,
        height: options.limits.max_height,
        refresh_mhz: u32::from(options.limits.max_fps) * MILLIHERTZ_PER_HERTZ,
        scale_120: UNSCALED,
        placement: signaling_edge(options.placement),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_crosses_both_ways() {
        for feature in Feature::ALL {
            assert_eq!(ipc_feature(signaling_feature(feature)), feature);
        }
        let all: SignalingFeatureSet = SignalingFeature::ALL.into_iter().collect();
        assert_eq!(ipc_features(all).iter().count(), Feature::ALL.len());
    }

    #[test]
    fn a_monitor_asks_for_its_limits_as_a_mode() {
        let monitor = start_monitor(MonitorOptions {
            placement: Edge::Left,
            limits: VideoLimits {
                max_width: 2560,
                max_height: 1600,
                max_fps: 60,
            },
        });
        assert_eq!(
            (monitor.width, monitor.height, monitor.refresh_mhz),
            (2560, 1600, 60_000)
        );
        assert_eq!(monitor.placement, SignalingEdge::Left);
    }
}
