use std::mem::size_of;

use ffmpeg_next::ffi::{
    av_frame_new_side_data, av_frame_remove_side_data, AVFrameSideDataType, AVPictureType,
    AVPixelFormat, AVRational, AVRegionOfInterest,
};

use crate::media::video::hw::{AvFrame, CodecContext};
use crate::media::video::image::Nv12Image;
use crate::media::MediaError;

#[derive(Debug, Clone, Copy)]
pub(super) struct Submission {
    pub(super) pts: i64,
    pub(super) keyframe: bool,
    pub(super) qp_offset: i8,
    pub(super) qp_range: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) fn submit(
    context: &mut CodecContext,
    frame: &mut AvFrame,
    submission: Submission,
) -> Result<(), MediaError> {
    let raw = frame.get_mut();
    raw.pts = submission.pts;
    raw.pict_type = if submission.keyframe {
        AVPictureType::AV_PICTURE_TYPE_I
    } else {
        AVPictureType::AV_PICTURE_TYPE_NONE
    };
    set_qp_offset(frame, submission)?;
    let sent = context.send_frame(Some(frame));
    // SAFETY: the frame is live; removing absent side data is a no-op.
    unsafe {
        av_frame_remove_side_data(
            frame.as_mut_ptr(),
            AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST,
        );
    }
    sent
}

fn set_qp_offset(frame: &mut AvFrame, submission: Submission) -> Result<(), MediaError> {
    if submission.qp_offset == 0 {
        return Ok(());
    }
    // SAFETY: the frame is live; FFmpeg allocates side data of the requested size.
    let side_data = unsafe {
        av_frame_new_side_data(
            frame.as_mut_ptr(),
            AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST,
            size_of::<AVRegionOfInterest>(),
        )
    };
    if side_data.is_null() {
        return Err(MediaError::InvalidFrame("could not attach the QP offset"));
    }
    let whole_frame = AVRegionOfInterest {
        self_size: u32::try_from(size_of::<AVRegionOfInterest>()).unwrap_or_default(),
        top: 0,
        bottom: i32::try_from(submission.height).unwrap_or(i32::MAX),
        left: 0,
        right: i32::try_from(submission.width).unwrap_or(i32::MAX),
        qoffset: AVRational {
            num: i32::from(submission.qp_offset),
            den: submission.qp_range,
        },
    };
    // SAFETY: side_data holds exactly one AVRegionOfInterest-sized, suitably aligned buffer.
    unsafe {
        (*side_data)
            .data
            .cast::<AVRegionOfInterest>()
            .write(whole_frame)
    };
    Ok(())
}

pub(super) fn describe_nv12(frame: &mut AvFrame, image: &Nv12Image<'_>) {
    let raw = frame.get_mut();
    raw.format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    raw.width = i32::try_from(image.width).unwrap_or_default();
    raw.height = i32::try_from(image.height).unwrap_or_default();
    raw.data[0] = image.luma.as_ptr().cast_mut();
    raw.linesize[0] = i32::try_from(image.luma_stride).unwrap_or_default();
    raw.data[1] = image.chroma.as_ptr().cast_mut();
    raw.linesize[1] = i32::try_from(image.chroma_stride).unwrap_or_default();
}
