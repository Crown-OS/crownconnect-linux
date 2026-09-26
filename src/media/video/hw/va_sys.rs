use std::ffi::{c_int, c_void};

pub(super) type VaDisplay = *mut c_void;
pub(super) type VaStatus = c_int;
pub(super) type VaProfile = c_int;
pub(super) type VaEntrypoint = c_int;

pub(super) const VA_STATUS_SUCCESS: VaStatus = 0;
pub(super) const VA_ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;

pub(super) const VA_PROFILE_H264_HIGH: VaProfile = 7;
pub(super) const VA_PROFILE_H264_CONSTRAINED_BASELINE: VaProfile = 13;
pub(super) const VA_PROFILE_HEVC_MAIN: VaProfile = 17;
pub(super) const VA_PROFILE_HEVC_MAIN10: VaProfile = 18;
pub(super) const VA_PROFILE_AV1_PROFILE0: VaProfile = 32;

pub(super) const VA_ENTRYPOINT_VLD: VaEntrypoint = 1;
pub(super) const VA_ENTRYPOINT_ENC_SLICE: VaEntrypoint = 6;
pub(super) const VA_ENTRYPOINT_ENC_SLICE_LP: VaEntrypoint = 8;

pub(super) const VA_CONFIG_ATTRIB_ENC_MAX_SLICES: c_int = 14;
pub(super) const VA_CONFIG_ATTRIB_ENC_ROI: c_int = 25;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct VaConfigAttrib {
    pub(super) attrib_type: c_int,
    pub(super) value: u32,
}

#[link(name = "va")]
unsafe extern "C" {
    pub(super) fn vaMaxNumProfiles(display: VaDisplay) -> c_int;
    pub(super) fn vaMaxNumEntrypoints(display: VaDisplay) -> c_int;
    pub(super) fn vaQueryConfigProfiles(
        display: VaDisplay,
        profiles: *mut VaProfile,
        count: *mut c_int,
    ) -> VaStatus;
    pub(super) fn vaQueryConfigEntrypoints(
        display: VaDisplay,
        profile: VaProfile,
        entrypoints: *mut VaEntrypoint,
        count: *mut c_int,
    ) -> VaStatus;
    pub(super) fn vaGetConfigAttributes(
        display: VaDisplay,
        profile: VaProfile,
        entrypoint: VaEntrypoint,
        attributes: *mut VaConfigAttrib,
        count: c_int,
    ) -> VaStatus;
}
