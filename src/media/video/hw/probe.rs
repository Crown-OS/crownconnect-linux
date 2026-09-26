use std::ffi::c_int;

use super::va_sys::*;
use super::VaapiDevice;
use crate::media::video::codec::CodecProfile;
use crate::media::video::negotiate::CodecCapabilities;
use crate::media::MediaError;

/// Encoder tuning knobs the hardware supports for one profile.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EncodeFeatures {
    /// Only the low-power (fixed-function) entrypoint is available.
    pub low_power_only: bool,
    /// Per-frame region-of-interest QP deltas work together with bitrate control.
    pub roi_qp_delta: bool,
    pub max_slices: u32,
}

/// The probed codec support of a VA-API device.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HardwareCapabilities {
    pub codecs: CodecCapabilities,
    encode_features: [EncodeFeatures; CodecProfile::ALL.len()],
}

const ROI_REGIONS_MASK: u32 = 0xff;
const ROI_RC_QP_DELTA_BIT: u32 = 1 << 9;

impl HardwareCapabilities {
    pub fn probe(device: &VaapiDevice) -> Result<Self, MediaError> {
        let display = device.display();
        let available = query_profiles(display)?;
        let mut capabilities = Self::default();
        for profile in CodecProfile::ALL {
            let va_profile = va_profile(profile);
            if !available.contains(&va_profile) {
                continue;
            }
            let entrypoints = query_entrypoints(display, va_profile)?;
            if entrypoints.contains(&VA_ENTRYPOINT_VLD) {
                capabilities.codecs.decode = capabilities.codecs.decode.with(profile);
            }
            let encode_entrypoint = [VA_ENTRYPOINT_ENC_SLICE, VA_ENTRYPOINT_ENC_SLICE_LP]
                .into_iter()
                .find(|entrypoint| entrypoints.contains(entrypoint));
            if let Some(entrypoint) = encode_entrypoint {
                capabilities.codecs.encode = capabilities.codecs.encode.with(profile);
                let features = encode_features(display, va_profile, entrypoint);
                if let Some(slot) = capabilities.encode_features.get_mut(profile as usize) {
                    *slot = features;
                }
            }
        }
        Ok(capabilities)
    }

    pub fn encode_features(&self, profile: CodecProfile) -> EncodeFeatures {
        self.encode_features
            .get(profile as usize)
            .copied()
            .unwrap_or_default()
    }
}

const fn va_profile(profile: CodecProfile) -> VaProfile {
    match profile {
        CodecProfile::H264ConstrainedBaseline => VA_PROFILE_H264_CONSTRAINED_BASELINE,
        CodecProfile::H264High => VA_PROFILE_H264_HIGH,
        CodecProfile::HevcMain => VA_PROFILE_HEVC_MAIN,
        CodecProfile::HevcMain10 => VA_PROFILE_HEVC_MAIN10,
        CodecProfile::Av1Main => VA_PROFILE_AV1_PROFILE0,
    }
}

fn query_profiles(display: VaDisplay) -> Result<Vec<VaProfile>, MediaError> {
    // SAFETY: display is the initialised VADisplay owned by the device.
    let capacity = unsafe { vaMaxNumProfiles(display) };
    query_list(capacity, "vaQueryConfigProfiles", |list, count| {
        // SAFETY: list has room for vaMaxNumProfiles entries.
        unsafe { vaQueryConfigProfiles(display, list, count) }
    })
}

fn query_entrypoints(
    display: VaDisplay,
    profile: VaProfile,
) -> Result<Vec<VaEntrypoint>, MediaError> {
    // SAFETY: display is the initialised VADisplay owned by the device.
    let capacity = unsafe { vaMaxNumEntrypoints(display) };
    query_list(capacity, "vaQueryConfigEntrypoints", |list, count| {
        // SAFETY: list has room for vaMaxNumEntrypoints entries.
        unsafe { vaQueryConfigEntrypoints(display, profile, list, count) }
    })
}

fn query_list(
    capacity: c_int,
    operation: &'static str,
    query: impl FnOnce(*mut c_int, *mut c_int) -> VaStatus,
) -> Result<Vec<c_int>, MediaError> {
    let mut list = vec![0; usize::try_from(capacity).unwrap_or_default()];
    let mut count = 0;
    let status = query(list.as_mut_ptr(), &mut count);
    if status != VA_STATUS_SUCCESS {
        return Err(MediaError::VaApi { operation, status });
    }
    list.truncate(usize::try_from(count).unwrap_or_default());
    Ok(list)
}

fn encode_features(
    display: VaDisplay,
    profile: VaProfile,
    entrypoint: VaEntrypoint,
) -> EncodeFeatures {
    let mut attributes =
        [VA_CONFIG_ATTRIB_ENC_ROI, VA_CONFIG_ATTRIB_ENC_MAX_SLICES].map(|attrib_type| {
            VaConfigAttrib {
                attrib_type,
                value: VA_ATTRIB_NOT_SUPPORTED,
            }
        });
    // SAFETY: attributes is a valid array of two entries for the driver to fill.
    let status =
        unsafe { vaGetConfigAttributes(display, profile, entrypoint, attributes.as_mut_ptr(), 2) };
    let supported = |attribute: VaConfigAttrib| {
        (status == VA_STATUS_SUCCESS && attribute.value != VA_ATTRIB_NOT_SUPPORTED)
            .then_some(attribute.value)
    };
    let [roi, max_slices] = attributes.map(supported);
    EncodeFeatures {
        low_power_only: entrypoint == VA_ENTRYPOINT_ENC_SLICE_LP,
        roi_qp_delta: roi
            .is_some_and(|value| value & ROI_REGIONS_MASK > 0 && value & ROI_RC_QP_DELTA_BIT != 0),
        max_slices: max_slices.unwrap_or(1),
    }
}
