use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

/// NetworkManager's connection settings, `a{sa{sv}}`.
pub(super) type ConnectionSettings = HashMap<String, HashMap<String, OwnedValue>>;

const WIRELESS: &str = "802-11-wireless";

/// The SSID of a Wi-Fi connection that runs in access-point mode, or `None` for anything else.
pub(super) fn access_point_ssid(settings: &ConnectionSettings) -> Option<String> {
    let wireless = settings.get(WIRELESS)?;
    let mode = wireless.get("mode")?;
    if !matches!(&**mode, Value::Str(mode) if mode.as_str() == "ap") {
        return None;
    }
    Some(match wireless.get("ssid").map(|ssid| &**ssid) {
        Some(Value::Array(bytes)) => {
            let bytes: Vec<u8> = bytes
                .inner()
                .iter()
                .filter_map(|byte| match byte {
                    Value::U8(byte) => Some(*byte),
                    _ => None,
                })
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => String::new(),
    })
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::Array;

    use super::*;

    fn wireless(mode: &str, ssid: &[u8]) -> Result<ConnectionSettings, zbus::zvariant::Error> {
        let ssid = Array::from(ssid.to_vec());
        Ok(HashMap::from([(
            WIRELESS.to_owned(),
            HashMap::from([
                ("mode".to_owned(), OwnedValue::try_from(Value::from(mode))?),
                ("ssid".to_owned(), OwnedValue::try_from(Value::Array(ssid))?),
            ]),
        )]))
    }

    #[test]
    fn an_access_point_reports_its_ssid() -> Result<(), zbus::zvariant::Error> {
        assert_eq!(
            access_point_ssid(&wireless("ap", b"Crown Hotspot")?),
            Some("Crown Hotspot".to_owned())
        );
        Ok(())
    }

    #[test]
    fn client_connections_are_not_hotspots() -> Result<(), zbus::zvariant::Error> {
        assert_eq!(
            access_point_ssid(&wireless("infrastructure", b"Home")?),
            None
        );
        assert_eq!(access_point_ssid(&HashMap::new()), None);
        Ok(())
    }
}
