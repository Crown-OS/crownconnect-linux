use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// The parts of an MPRIS `Metadata` map a paired device shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TrackMetadata {
    pub(super) title: String,
    pub(super) artist: String,
    pub(super) length_us: Option<u64>,
    pub(super) track_id: Option<OwnedObjectPath>,
}

impl TrackMetadata {
    pub(super) fn from_map(metadata: &HashMap<String, OwnedValue>) -> Self {
        let field = |key: &str| metadata.get(key).map(|value| unwrap_variant(value));
        Self {
            title: field("xesam:title")
                .and_then(as_str)
                .unwrap_or_default()
                .to_owned(),
            artist: field("xesam:artist")
                .map(joined_strings)
                .unwrap_or_default(),
            length_us: field("mpris:length").and_then(as_unsigned),
            track_id: field("mpris:trackid").and_then(as_object_path),
        }
    }
}

fn unwrap_variant<'v>(value: &'v Value<'v>) -> &'v Value<'v> {
    match value {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn as_str<'v>(value: &'v Value<'v>) -> Option<&'v str> {
    match value {
        Value::Str(text) => Some(text.as_str()),
        _ => None,
    }
}

fn joined_strings(value: &Value<'_>) -> String {
    match value {
        Value::Array(items) => items
            .inner()
            .iter()
            .filter_map(|item| as_str(unwrap_variant(item)))
            .collect::<Vec<_>>()
            .join(", "),
        other => as_str(other).unwrap_or_default().to_owned(),
    }
}

fn as_unsigned(value: &Value<'_>) -> Option<u64> {
    match value {
        Value::I64(signed) => u64::try_from(*signed).ok(),
        Value::U64(unsigned) => Some(*unsigned),
        Value::I32(signed) => u64::try_from(*signed).ok(),
        Value::U32(unsigned) => Some(u64::from(*unsigned)),
        _ => None,
    }
}

fn as_object_path(value: &Value<'_>) -> Option<OwnedObjectPath> {
    match value {
        Value::ObjectPath(path) => Some(path.clone().into()),
        Value::Str(text) => OwnedObjectPath::try_from(text.as_str()).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::{Array, ObjectPath};

    use super::*;

    fn owned(value: Value<'_>) -> Result<OwnedValue, zbus::zvariant::Error> {
        OwnedValue::try_from(value)
    }

    #[test]
    fn reads_title_artists_length_and_track() -> Result<(), zbus::zvariant::Error> {
        let artists = Array::from(vec!["Daft Punk", "Pharrell"]);
        let metadata = HashMap::from([
            ("xesam:title".to_owned(), owned(Value::from("Get Lucky"))?),
            ("xesam:artist".to_owned(), owned(Value::Array(artists))?),
            ("mpris:length".to_owned(), owned(Value::I64(248_000_000))?),
            (
                "mpris:trackid".to_owned(),
                owned(Value::ObjectPath(ObjectPath::try_from("/track/7")?))?,
            ),
        ]);
        let track = TrackMetadata::from_map(&metadata);
        assert_eq!(track.title, "Get Lucky");
        assert_eq!(track.artist, "Daft Punk, Pharrell");
        assert_eq!(track.length_us, Some(248_000_000));
        assert_eq!(
            track.track_id.as_deref().map(|path| path.as_str()),
            Some("/track/7")
        );
        Ok(())
    }

    #[test]
    fn missing_and_mistyped_fields_become_defaults() -> Result<(), zbus::zvariant::Error> {
        let metadata = HashMap::from([
            ("xesam:title".to_owned(), owned(Value::U32(3))?),
            ("mpris:length".to_owned(), owned(Value::I64(-5))?),
        ]);
        assert_eq!(TrackMetadata::from_map(&metadata), TrackMetadata::default());
        Ok(())
    }
}
