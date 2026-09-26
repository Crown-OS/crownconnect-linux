//! Call commands a paired device sends this computer, carried out on its own hands-free link to
//! the phone.

use llts_node::CallCommand;

use crate::bluetooth::telephony::TelephonyAction;

/// What `command` means for the hands-free link. `hands_free_call` names the local call a
/// peer's call id stands for; `gateway` is the one phone connected for hands-free.
///
/// # Errors
///
/// A reason for the log when no local call or phone fits.
pub(crate) fn remote_call_action(
    command: &CallCommand,
    hands_free_call: impl Fn(&str) -> Option<String>,
    gateway: Option<&str>,
) -> Result<TelephonyAction, &'static str> {
    const NO_CALL: &str = "no hands-free call matches";
    Ok(match command {
        CallCommand::Pickup { call_id } => TelephonyAction::Answer {
            call_id: hands_free_call(call_id).ok_or(NO_CALL)?,
        },
        CallCommand::Decline { call_id } | CallCommand::Hangup { call_id } => {
            TelephonyAction::Hangup {
                call_id: hands_free_call(call_id).ok_or(NO_CALL)?,
            }
        }
        CallCommand::Dial { number } => TelephonyAction::Dial {
            address: gateway
                .ok_or("no single phone is connected for hands-free")?
                .to_owned(),
            number: number.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(call_id: &str) -> Option<String> {
        (call_id == "/call1").then(|| call_id.to_owned())
    }

    #[test]
    fn peer_call_commands_act_on_matching_hands_free_calls() {
        let pickup = CallCommand::Pickup {
            call_id: "/call1".to_owned(),
        };
        assert!(matches!(
            remote_call_action(&pickup, known, None),
            Ok(TelephonyAction::Answer { call_id }) if call_id == "/call1"
        ));
        let unknown = CallCommand::Hangup {
            call_id: "/other".to_owned(),
        };
        assert!(remote_call_action(&unknown, known, None).is_err());
        let dial = CallCommand::Dial {
            number: "+100".to_owned(),
        };
        assert!(remote_call_action(&dial, known, None).is_err());
        assert!(matches!(
            remote_call_action(&dial, known, Some("AA:BB")),
            Ok(TelephonyAction::Dial { address, number }) if address == "AA:BB" && number == "+100"
        ));
    }
}
