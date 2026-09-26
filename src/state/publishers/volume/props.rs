use llts_signaling::state::Volume;
use pipewire::spa;
use spa::param::ParamType;
use spa::pod::deserialize::PodDeserializer;
use spa::pod::serialize::PodSerializer;
use spa::pod::{Object, Property, PropertyFlags, Value, ValueArray};
use spa::sys::{SPA_PROP_channelVolumes, SPA_PROP_mute};
use spa::utils::SpaTypes;

use crate::util::numeric::narrow_f32;
use crate::util::percent::whole_percent;

/// The volume a sink node reports in its `Props` parameter.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SinkProps {
    pub(super) channel_volumes: Vec<f32>,
    pub(super) muted: bool,
}

impl SinkProps {
    /// `None` for a `Props` object without channel volumes, which nodes also emit.
    pub(super) fn parse(pod: &[u8]) -> Option<Self> {
        let (_, Value::Object(object)) = PodDeserializer::deserialize_any_from(pod).ok()? else {
            return None;
        };
        let mut channel_volumes = None;
        let mut muted = false;
        for property in object.properties {
            match property.value {
                Value::ValueArray(ValueArray::Float(volumes))
                    if property.key == SPA_PROP_channelVolumes =>
                {
                    channel_volumes = Some(volumes);
                }
                Value::Bool(mute) if property.key == SPA_PROP_mute => muted = mute,
                _ => {}
            }
        }
        Some(Self {
            channel_volumes: channel_volumes.filter(|volumes| !volumes.is_empty())?,
            muted,
        })
    }

    /// Perceived volume, as desktop mixers show it: the loudest channel, on a cubic scale.
    pub(super) fn volume(&self) -> Volume {
        let loudest = self.channel_volumes.iter().copied().fold(0.0_f32, f32::max);
        Volume {
            percent: whole_percent(f64::from(loudest).cbrt() * 100.0),
            muted: self.muted,
        }
    }
}

/// A `Props` pod setting every channel to `percent` on the cubic scale.
pub(super) fn props_pod(channels: usize, percent: u8, muted: bool) -> Option<Vec<u8>> {
    let linear = narrow_f32((f64::from(percent.min(100)) / 100.0).powi(3));
    let props = Value::Object(Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![
            Property {
                key: SPA_PROP_channelVolumes,
                flags: PropertyFlags::empty(),
                value: Value::ValueArray(ValueArray::Float(vec![linear; channels.max(1)])),
            },
            Property {
                key: SPA_PROP_mute,
                flags: PropertyFlags::empty(),
                value: Value::Bool(muted),
            },
        ],
    });
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &props)
        .ok()
        .map(|(cursor, _)| cursor.into_inner())
}

/// The sink name out of the `default.audio.sink` metadata value, `{ "name": "<node.name>" }`.
pub(super) fn default_sink_name(json: &str) -> Option<&str> {
    let after_key = json.split_once("\"name\"")?.1;
    let quoted = after_key.trim_start().strip_prefix(':')?.trim_start();
    let value = quoted.strip_prefix('"')?;
    value.split_once('"').map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_props_pod_round_trips_through_the_parser() {
        let pod = props_pod(2, 50, true);
        let parsed = pod.as_deref().and_then(SinkProps::parse);
        let volume = parsed.as_ref().map(SinkProps::volume);
        assert_eq!(parsed.map(|props| props.channel_volumes.len()), Some(2));
        assert_eq!(
            volume,
            Some(Volume {
                percent: 50,
                muted: true
            })
        );
    }

    #[test]
    fn the_loudest_channel_sets_the_volume() {
        let props = SinkProps {
            channel_volumes: vec![0.125, 0.001],
            muted: false,
        };
        assert_eq!(props.volume().percent, 50);
    }

    #[test]
    fn the_default_sink_name_is_read_from_metadata_json() {
        assert_eq!(
            default_sink_name(r#"{ "name": "alsa_output.pci-0000_00_1f.3.analog-stereo" }"#),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
        assert_eq!(default_sink_name(r#"{"name":"x"}"#), Some("x"));
        assert_eq!(default_sink_name("{}"), None);
    }
}
