//! Calls on a paired phone reach this computer twice: as the phone's `CallState` over llts,
//! and as PipeWire telephony calls over this computer's own hands-free link. Both are shown
//! as the phone's calls, and controlling one prefers hands-free, which also moves the audio.

use std::collections::BTreeMap;

use llts_node::{CallCommand, OutgoingCommand};
use llts_signaling::device::{DeviceClass, DeviceId};
use llts_signaling::message::Call;
use llts_signaling::state::CallState;

use super::event_loop::PeerLoop;
use crate::bluetooth::telephony::{PhoneCalls, TelephonyAction, TelephonyJob};
use crate::features::calls::remote_call_action;
use crate::ipc::proto::{CallInfo, CallStatus};
use crate::ipc::server::{DaemonEvent, Responder};
use crate::peer::convert::ipc_id;

/// The PipeWire telephony calls of each audio gateway, by the phone's Bluetooth address.
#[derive(Debug, Default)]
pub(super) struct HandsFreeCalls {
    gateways: BTreeMap<String, Vec<CallInfo>>,
}

impl HandsFreeCalls {
    /// Folds in one gateway's calls and returns those to announce; a call that vanished is
    /// announced as ended.
    pub(super) fn update(&mut self, update: PhoneCalls) -> Vec<CallInfo> {
        let PhoneCalls { address, calls } = update;
        if address.is_empty() {
            return Vec::new();
        }
        let previous = self.gateways.remove(&address).unwrap_or_default();
        let vanished = previous
            .into_iter()
            .filter(|old| !calls.iter().any(|call| call.call_id == old.call_id))
            .map(|old| CallInfo {
                status: CallStatus::Ended,
                ..old
            });
        let announced = calls.iter().cloned().chain(vanished).collect();
        self.gateways.insert(address, calls);
        announced
    }

    fn calls(&self) -> impl Iterator<Item = &CallInfo> {
        self.gateways.values().flatten()
    }

    fn contains(&self, call_id: &str) -> bool {
        self.calls().any(|call| call.call_id == call_id)
    }

    /// The unfinished hands-free call with `number`, the same call the phone reports over llts.
    fn matching(&self, number: &str) -> Option<&CallInfo> {
        self.calls()
            .find(|call| call.number == number && call.status != CallStatus::Ended)
    }

    /// The one phone connected for hands-free, if exactly one is.
    fn gateway(&self) -> Option<&str> {
        let mut addresses = self.gateways.keys();
        let first = addresses.next()?;
        addresses.next().is_none().then_some(first.as_str())
    }
}

/// What to do with a call someone asked about by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CallControl {
    Pickup,
    Decline,
    Hangup,
}

impl CallControl {
    const fn telephony(self, call_id: String) -> TelephonyAction {
        match self {
            Self::Pickup => TelephonyAction::Answer { call_id },
            Self::Decline | Self::Hangup => TelephonyAction::Hangup { call_id },
        }
    }

    const fn llts(self, call_id: &str) -> Call<'_> {
        match self {
            Self::Pickup => Call::Pickup { call_id },
            Self::Decline => Call::Decline { call_id },
            Self::Hangup => Call::Hangup { call_id },
        }
    }
}

fn single<T>(mut items: impl Iterator<Item = T>) -> Option<T> {
    let first = items.next()?;
    items.next().is_none().then_some(first)
}

impl PeerLoop {
    pub(super) fn on_phone_calls(&mut self, update: PhoneCalls) {
        let announced = self.hands_free.update(update);
        let Some(phone) = self.hands_free_phone() else {
            if !announced.is_empty() {
                tracing::debug!("no single paired phone to show hands-free calls for");
            }
            return;
        };
        for call in announced {
            self.announce(DaemonEvent::Call {
                id: ipc_id(phone),
                call,
            });
        }
    }

    /// The paired phone behind the hands-free link: the one connected phone, or failing that
    /// the one paired phone. PipeWire names gateways by Bluetooth address, which llts does not
    /// carry, so a second phone makes the link ambiguous.
    fn hands_free_phone(&self) -> Option<DeviceId> {
        let phones = || {
            self.node
                .devices()
                .filter(|device| device.class == DeviceClass::Phone)
        };
        single(phones().filter(|device| device.connected))
            .or_else(|| single(phones()))
            .map(|device| device.id)
    }

    pub(super) fn control_call(
        &mut self,
        control: CallControl,
        call_id: &str,
        reply: Responder<()>,
    ) {
        if self.hands_free.contains(call_id) {
            return self.telephony(control.telephony(call_id.to_owned()), Some(reply));
        }
        let Some((peer, number)) = self.llts_call(call_id) else {
            return reply.fail(format!("no device has a call {call_id}"));
        };
        if self.hands_free_phone() == Some(peer)
            && let Some(call) = self.hands_free.matching(&number)
        {
            let action = control.telephony(call.call_id.clone());
            return self.telephony(action, Some(reply));
        }
        let outcome = self.send_command(peer, OutgoingCommand::Call(control.llts(call_id)));
        Self::respond(reply, outcome);
    }

    pub(super) fn dial(&mut self, peer: DeviceId, number: &str, reply: Responder<()>) {
        if self.hands_free_phone() == Some(peer)
            && let Some(address) = self.hands_free.gateway()
        {
            let action = TelephonyAction::Dial {
                address: address.to_owned(),
                number: number.to_owned(),
            };
            return self.telephony(action, Some(reply));
        }
        let outcome = self.send_command(peer, OutgoingCommand::Call(Call::Dial { number }));
        Self::respond(reply, outcome);
    }

    fn llts_call(&self, call_id: &str) -> Option<(DeviceId, String)> {
        self.node.devices().find_map(|device| {
            let state = self
                .node
                .remote_state::<CallState<'_>>(&device.id)
                .ok()
                .flatten()?;
            state
                .calls
                .iter()
                .find(|call| call.call_id == call_id)
                .map(|call| (device.id, call.number.to_owned()))
        })
    }

    /// Carries out a call command a paired device sent, on this computer's hands-free link.
    pub(super) fn on_remote_call(&self, peer: DeviceId, command: &CallCommand) {
        let action = remote_call_action(
            command,
            |call_id| self.hands_free_call(call_id),
            self.hands_free.gateway(),
        );
        match action {
            Ok(action) => self.telephony(action, None),
            Err(reason) => tracing::info!(%peer, ?command, reason, "cannot act on the call"),
        }
    }

    /// The hands-free call a peer means by `call_id`: one of ours, or the one with the number
    /// of that call in some device's `CallState`.
    fn hands_free_call(&self, call_id: &str) -> Option<String> {
        if self.hands_free.contains(call_id) {
            return Some(call_id.to_owned());
        }
        let (_, number) = self.llts_call(call_id)?;
        self.hands_free
            .matching(&number)
            .map(|call| call.call_id.clone())
    }

    fn telephony(&self, action: TelephonyAction, reply: Option<Responder<()>>) {
        if let Err(refused) = self
            .services
            .telephony
            .try_send(TelephonyJob { action, reply })
        {
            match refused.into_inner().reply {
                Some(reply) => reply.fail("PipeWire telephony is not available"),
                None => tracing::warn!("PipeWire telephony is not available"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(call_id: &str, status: CallStatus) -> CallInfo {
        CallInfo {
            call_id: call_id.to_owned(),
            number: "+1".to_owned(),
            contact_name: None,
            status,
            answered_unix_ms: None,
        }
    }

    #[test]
    fn a_vanished_call_is_announced_as_ended() {
        let mut calls = HandsFreeCalls::default();
        let ringing = PhoneCalls {
            address: "AA".to_owned(),
            calls: vec![call("/c1", CallStatus::Ringing)],
        };
        assert_eq!(calls.update(ringing).len(), 1);
        assert!(calls.contains("/c1"));
        assert_eq!(calls.gateway(), Some("AA"));
        assert!(calls.matching("+1").is_some());
        let gone = calls.update(PhoneCalls {
            address: "AA".to_owned(),
            calls: Vec::new(),
        });
        assert_eq!(gone, [call("/c1", CallStatus::Ended)]);
        assert!(!calls.contains("/c1"));
    }

    #[test]
    fn two_gateways_leave_dialing_ambiguous() {
        let mut calls = HandsFreeCalls::default();
        for address in ["AA", "BB"] {
            calls.update(PhoneCalls {
                address: address.to_owned(),
                calls: Vec::new(),
            });
        }
        assert_eq!(calls.gateway(), None);
        assert!(calls
            .update(PhoneCalls {
                address: String::new(),
                calls: vec![call("/x", CallStatus::Active)],
            })
            .is_empty());
    }
}
