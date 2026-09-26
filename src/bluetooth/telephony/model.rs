use std::collections::{BTreeMap, HashMap};

use zbus::zvariant::{OwnedValue, Value};

use crate::ipc::proto::{CallInfo, CallStatus};

pub(super) const GATEWAY_INTERFACE: &str = "org.pipewire.Telephony.AudioGateway1";
pub(super) const CALL_INTERFACE: &str = "org.pipewire.Telephony.Call1";

pub(super) type Properties = HashMap<String, OwnedValue>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Call {
    number: String,
    name: String,
    status: Option<CallStatus>,
    answered_unix_ms: Option<u64>,
}

/// The audio gateways (phones) PipeWire's telephony service knows and their calls, keyed by
/// object path; a call's path lies under its gateway's.
#[derive(Debug, Default)]
pub(super) struct TelephonyModel {
    gateways: BTreeMap<String, String>,
    calls: BTreeMap<String, Call>,
}

pub(super) fn call_status(state: &str) -> Option<CallStatus> {
    Some(match state {
        "incoming" | "waiting" => CallStatus::Ringing,
        "dialing" | "alerting" => CallStatus::Dialing,
        "active" => CallStatus::Active,
        "held" => CallStatus::Held,
        "disconnected" => CallStatus::Ended,
        _ => return None,
    })
}

fn string(properties: &Properties, key: &str) -> Option<String> {
    match properties.get(key).map(|value| &**value) {
        Some(Value::Str(text)) => Some(text.as_str().to_owned()),
        _ => None,
    }
}

impl TelephonyModel {
    /// Folds in an object's interfaces; returns the gateway whose calls changed.
    pub(super) fn add_object(
        &mut self,
        path: &str,
        interfaces: &HashMap<String, Properties>,
        now_ms: u64,
    ) -> Option<String> {
        if let Some(properties) = interfaces.get(GATEWAY_INTERFACE) {
            let address = string(properties, "Address").unwrap_or_default();
            self.gateways.insert(path.to_owned(), address);
            return Some(path.to_owned());
        }
        let properties = interfaces.get(CALL_INTERFACE)?;
        self.calls.entry(path.to_owned()).or_default();
        self.update_call(path, properties, now_ms)
    }

    pub(super) fn update_call(
        &mut self,
        path: &str,
        properties: &Properties,
        now_ms: u64,
    ) -> Option<String> {
        let call = self.calls.get_mut(path)?;
        if let Some(number) = string(properties, "LineIdentification") {
            call.number = number;
        }
        if let Some(name) = string(properties, "Name") {
            call.name = name;
        }
        if let Some(status) = string(properties, "State").as_deref().and_then(call_status) {
            if status == CallStatus::Active && call.answered_unix_ms.is_none() {
                call.answered_unix_ms = Some(now_ms);
            }
            call.status = Some(status);
        }
        self.gateway_of(path).map(str::to_owned)
    }

    pub(super) fn remove_object(&mut self, path: &str) -> Option<String> {
        if self.gateways.remove(path).is_some() {
            let prefix = format!("{path}/");
            self.calls.retain(|call, _| !call.starts_with(&prefix));
            return Some(path.to_owned());
        }
        self.calls.remove(path)?;
        self.gateway_of(path).map(str::to_owned)
    }

    pub(super) fn gateway_of(&self, call_path: &str) -> Option<&str> {
        self.gateways
            .keys()
            .find(|gateway| {
                call_path
                    .strip_prefix(gateway.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(String::as_str)
    }

    pub(super) fn gateway_by_address(&self, address: &str) -> Option<&str> {
        self.gateways
            .iter()
            .find(|(_, known)| known.eq_ignore_ascii_case(address))
            .map(|(path, _)| path.as_str())
    }

    pub(super) fn address(&self, gateway: &str) -> Option<&str> {
        self.gateways.get(gateway).map(String::as_str)
    }

    pub(super) fn calls_of(&self, gateway: &str) -> Vec<CallInfo> {
        self.calls
            .iter()
            .filter(|(path, _)| self.gateway_of(path) == Some(gateway))
            .filter_map(|(path, call)| {
                Some(CallInfo {
                    call_id: path.clone(),
                    number: call.number.clone(),
                    contact_name: (!call.name.is_empty()).then(|| call.name.clone()),
                    status: call.status?,
                    answered_unix_ms: call.answered_unix_ms,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(pairs: &[(&str, &str)]) -> Result<Properties, zbus::zvariant::Error> {
        pairs
            .iter()
            .map(|(key, value)| {
                Ok((
                    (*key).to_owned(),
                    OwnedValue::try_from(Value::from(*value))?,
                ))
            })
            .collect()
    }

    fn interfaces(
        interface: &str,
        pairs: &[(&str, &str)],
    ) -> Result<HashMap<String, Properties>, zbus::zvariant::Error> {
        Ok(HashMap::from([(interface.to_owned(), properties(pairs)?)]))
    }

    const GATEWAY: &str = "/org/pipewire/Telephony/ag0";
    const CALL: &str = "/org/pipewire/Telephony/ag0/call1";

    #[test]
    fn a_call_is_tracked_from_ringing_to_answered() -> Result<(), zbus::zvariant::Error> {
        let mut model = TelephonyModel::default();
        model.add_object(
            GATEWAY,
            &interfaces(GATEWAY_INTERFACE, &[("Address", "AA:BB")])?,
            0,
        );
        let ringing = interfaces(
            CALL_INTERFACE,
            &[
                ("LineIdentification", "+123"),
                ("Name", "Ada"),
                ("State", "incoming"),
            ],
        )?;
        assert_eq!(
            model.add_object(CALL, &ringing, 10).as_deref(),
            Some(GATEWAY)
        );
        assert_eq!(
            model.calls_of(GATEWAY).first().map(|call| call.status),
            Some(CallStatus::Ringing)
        );

        let answered = properties(&[("State", "active")])?;
        model.update_call(CALL, &answered, 50);
        let calls = model.calls_of(GATEWAY);
        let call = calls.first();
        assert_eq!(call.map(|call| call.status), Some(CallStatus::Active));
        assert_eq!(call.and_then(|call| call.answered_unix_ms), Some(50));
        assert_eq!(
            call.and_then(|call| call.contact_name.as_deref()),
            Some("Ada")
        );
        assert_eq!(model.gateway_by_address("aa:bb"), Some(GATEWAY));
        Ok(())
    }

    #[test]
    fn removing_a_gateway_drops_its_calls() -> Result<(), zbus::zvariant::Error> {
        let mut model = TelephonyModel::default();
        model.add_object(GATEWAY, &interfaces(GATEWAY_INTERFACE, &[])?, 0);
        model.add_object(
            CALL,
            &interfaces(CALL_INTERFACE, &[("State", "dialing")])?,
            0,
        );
        assert_eq!(model.remove_object(GATEWAY).as_deref(), Some(GATEWAY));
        assert!(model.calls_of(GATEWAY).is_empty());
        assert_eq!(model.remove_object(CALL), None);
        Ok(())
    }

    #[test]
    fn a_sibling_path_is_not_a_child() {
        let mut model = TelephonyModel::default();
        model.gateways.insert(GATEWAY.to_owned(), String::new());
        assert_eq!(model.gateway_of("/org/pipewire/Telephony/ag01/call1"), None);
        assert_eq!(model.gateway_of(CALL), Some(GATEWAY));
    }

    #[test]
    fn telephony_states_map_onto_call_statuses() {
        assert_eq!(call_status("waiting"), Some(CallStatus::Ringing));
        assert_eq!(call_status("alerting"), Some(CallStatus::Dialing));
        assert_eq!(call_status("held"), Some(CallStatus::Held));
        assert_eq!(call_status("disconnected"), Some(CallStatus::Ended));
        assert_eq!(call_status("bogus"), None);
    }
}
