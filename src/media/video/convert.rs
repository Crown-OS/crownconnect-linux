use std::ffi::CString;
use std::ptr::{self, NonNull};

use ffmpeg_next::ffi::{
    av_buffersink_get_frame, av_buffersrc_add_frame_flags, av_buffersrc_parameters_alloc,
    av_buffersrc_parameters_set, av_free, avfilter_get_by_name, avfilter_graph_alloc,
    avfilter_graph_alloc_filter, avfilter_graph_config, avfilter_graph_free, avfilter_init_str,
    avfilter_link, AVFilterContext, AVFilterGraph, AVPixelFormat, AVRational,
    AV_BUFFERSRC_FLAG_KEEP_REF,
};

use super::hw::{AvFrame, BufferRef};
use crate::media::MediaError;
use crate::util::av::av_result;

const TO_NV12_BT709: &str = "format=nv12:out_color_matrix=bt709:out_range=tv";

/// Converts RGB VA surfaces to NV12 VA surfaces on the GPU's video processor, for encoders that
/// only take 4:2:0 input. Frames never leave the GPU.
#[derive(Debug)]
pub(crate) struct GpuColorConverter {
    _graph: FilterGraph,
    source: NonNull<AVFilterContext>,
    sink: NonNull<AVFilterContext>,
    output: AvFrame,
}

// SAFETY: the graph and its filter contexts are owned together and only used through &mut self.
unsafe impl Send for GpuColorConverter {}

impl GpuColorConverter {
    pub(crate) fn new(
        input_frames: &BufferRef,
        width: u32,
        height: u32,
    ) -> Result<Self, MediaError> {
        let graph = FilterGraph::new()?;
        let source = graph.allocate("buffer", "in")?;
        set_source_frames(source, input_frames, width, height)?;
        // SAFETY: source is allocated in graph and fully configured through its parameters.
        av_result(
            unsafe { avfilter_init_str(source.as_ptr(), ptr::null()) },
            "avfilter_init_str(in)",
        )?;
        let scale = graph.create("scale_vaapi", "to_nv12", Some(TO_NV12_BT709))?;
        let sink = graph.create("buffersink", "out", None)?;
        // SAFETY: all three contexts belong to graph, which is not yet configured.
        unsafe {
            av_result(
                avfilter_link(source.as_ptr(), 0, scale.as_ptr(), 0),
                "avfilter_link(in)",
            )?;
            av_result(
                avfilter_link(scale.as_ptr(), 0, sink.as_ptr(), 0),
                "avfilter_link(out)",
            )?;
            av_result(
                avfilter_graph_config(graph.0.as_ptr(), ptr::null_mut()),
                "avfilter_graph_config",
            )?;
        }
        Ok(Self {
            _graph: graph,
            source,
            sink,
            output: AvFrame::new()?,
        })
    }

    /// Converts `input`, which stays owned by the caller, into a reused NV12 surface frame.
    pub(crate) fn convert(&mut self, input: &AvFrame) -> Result<&mut AvFrame, MediaError> {
        // SAFETY: source is the graph's buffer source; KEEP_REF makes it take its own reference.
        let code = unsafe {
            av_buffersrc_add_frame_flags(
                self.source.as_ptr(),
                input.as_ptr().cast_mut(),
                AV_BUFFERSRC_FLAG_KEEP_REF as i32,
            )
        };
        av_result(code, "av_buffersrc_add_frame_flags")?;
        self.output.unref();
        // SAFETY: sink is the graph's buffer sink and output an empty frame.
        let code = unsafe { av_buffersink_get_frame(self.sink.as_ptr(), self.output.as_mut_ptr()) };
        av_result(code, "av_buffersink_get_frame")?;
        Ok(&mut self.output)
    }
}

fn set_source_frames(
    source: NonNull<AVFilterContext>,
    frames: &BufferRef,
    width: u32,
    height: u32,
) -> Result<(), MediaError> {
    // SAFETY: av_buffersrc_parameters_alloc has no preconditions.
    let parameters = unsafe { av_buffersrc_parameters_alloc() };
    if parameters.is_null() {
        return Err(MediaError::Unsupported(
            "av_buffersrc_parameters_alloc failed",
        ));
    }
    // SAFETY: parameters is a fresh allocation; set takes its own reference to hw_frames_ctx,
    // so the borrowed pointer is only read during the call and the struct is freed after.
    let code = unsafe {
        (*parameters).format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
        (*parameters).width = i32::try_from(width).unwrap_or_default();
        (*parameters).height = i32::try_from(height).unwrap_or_default();
        (*parameters).time_base = AVRational {
            num: 1,
            den: 1_000_000,
        };
        (*parameters).sample_aspect_ratio = AVRational { num: 1, den: 1 };
        (*parameters).hw_frames_ctx = frames.as_ptr();
        let code = av_buffersrc_parameters_set(source.as_ptr(), parameters);
        av_free(parameters.cast());
        code
    };
    av_result(code, "av_buffersrc_parameters_set").map(drop)
}

#[derive(Debug)]
struct FilterGraph(NonNull<AVFilterGraph>);

impl FilterGraph {
    fn new() -> Result<Self, MediaError> {
        // SAFETY: avfilter_graph_alloc has no preconditions.
        NonNull::new(unsafe { avfilter_graph_alloc() })
            .map(Self)
            .ok_or(MediaError::Unsupported("avfilter_graph_alloc failed"))
    }

    /// Adds an uninitialised filter instance to the graph.
    fn allocate(
        &self,
        filter: &'static str,
        name: &str,
    ) -> Result<NonNull<AVFilterContext>, MediaError> {
        let filter_name = CString::new(filter).map_err(nul_byte)?;
        let instance_name = CString::new(name).map_err(nul_byte)?;
        // SAFETY: the names are valid C strings; the graph owns the new context.
        let context = unsafe {
            let definition = avfilter_get_by_name(filter_name.as_ptr());
            if definition.is_null() {
                return Err(MediaError::Unsupported(filter));
            }
            avfilter_graph_alloc_filter(self.0.as_ptr(), definition, instance_name.as_ptr())
        };
        NonNull::new(context).ok_or(MediaError::Unsupported(filter))
    }

    /// Adds a filter instance initialised from an option string.
    fn create(
        &self,
        filter: &'static str,
        name: &str,
        args: Option<&str>,
    ) -> Result<NonNull<AVFilterContext>, MediaError> {
        let context = self.allocate(filter, name)?;
        let args = args.map(CString::new).transpose().map_err(nul_byte)?;
        let args = args.as_ref().map_or(ptr::null(), |args| args.as_ptr());
        // SAFETY: context was just allocated and args is null or a valid C string.
        av_result(
            unsafe { avfilter_init_str(context.as_ptr(), args) },
            "avfilter_init_str",
        )?;
        Ok(context)
    }
}

fn nul_byte(_: std::ffi::NulError) -> MediaError {
    MediaError::InvalidFrame("filter argument contains a NUL byte")
}

impl Drop for FilterGraph {
    fn drop(&mut self) {
        let mut raw = self.0.as_ptr();
        // SAFETY: self owns the graph, freed once here together with its filters.
        unsafe { avfilter_graph_free(&mut raw) };
    }
}
