//! Keeping the IPC server's device list equal to the node's.

use std::collections::BTreeMap;

use llts_node::{DeviceView, Node};
use llts_signaling::state::Battery as BatteryState;

use super::event_loop::PeerLoop;
use crate::ipc::proto::{Battery, DeviceInfo, LinkKind};
use crate::ipc::server::DaemonEvent;
use crate::peer::convert::{ipc_class, ipc_features, ipc_id};

fn device_info(node: &Node, view: &DeviceView<'_>) -> DeviceInfo {
    DeviceInfo {
        id: ipc_id(view.id),
        name: view.name.to_owned(),
        class: ipc_class(view.class),
        trusted: true,
        connected: view.connected,
        link: LinkKind::Lan,
        battery: node
            .remote_state::<BatteryState>(&view.id)
            .ok()
            .flatten()
            .map(|battery| Battery {
                percent: battery.percent,
                charging: battery.charging,
            }),
        features: ipc_features(view.enabled),
        active: ipc_features(view.active),
    }
}

impl PeerLoop {
    /// Announces every device that appeared, changed or was forgotten since the last call.
    pub(super) fn sync_devices(&mut self) {
        let current: BTreeMap<_, _> = self
            .node
            .devices()
            .map(|view| {
                let info = device_info(&self.node, &view);
                (info.id, info)
            })
            .collect();
        let removed: Vec<_> = self
            .listed
            .keys()
            .filter(|id| !current.contains_key(id))
            .copied()
            .collect();
        let changed: Vec<DeviceInfo> = current
            .values()
            .filter(|info| self.listed.get(&info.id) != Some(info))
            .cloned()
            .collect();
        self.listed = current;
        for id in removed {
            self.announce(DaemonEvent::DeviceRemoved(id));
        }
        for info in changed {
            self.announce(DaemonEvent::DeviceUpdated(info));
        }
    }
}
