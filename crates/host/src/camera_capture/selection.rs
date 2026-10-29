//! Bounded, deterministic suggestions over reported modes. Never opens a device
//! and never manufactures a resolution, frame rate, subtype or conversion.
use super::{CameraError, CameraModeInfo};
use skylive_core::{CameraFpsPolicy, VideoCaptureMode, VideoColorMatrix, VideoColorRange, VideoPixelFormat};
use std::cmp::Ordering;

const LIMIT: usize = 1024;
const FULL_HD_PIXELS: u64 = 1920 * 1080;
/// DirectShow stores an integer 100 ns frame interval; Media Foundation may
/// report the same interval with a different rational representation.
pub(crate) fn directshow_mode_matches(a:VideoCaptureMode,b:VideoCaptureMode)->bool {
    a.width()==b.width() && a.height()==b.height() && a.format()==b.format()
        && u128::from(a.fps_den())*10_000_000/u128::from(a.fps_num())
            == u128::from(b.fps_den())*10_000_000/u128::from(b.fps_num())
}
/// Resolve against this device's current capabilities without mutating the
/// authored mode. Automatic policies preserve resolution and native format.
pub(super) fn resolve_camera_fps_policy(modes:&[CameraModeInfo], current:VideoCaptureMode,
    policy:CameraFpsPolicy, output:(u32,u32))->Result<Option<VideoCaptureMode>,CameraError> {
    match policy {
        CameraFpsPolicy::Fixed=>{validate(modes)?;Ok(modes.iter().filter_map(|info|info.mode).find(|mode|*mode==current))},
        CameraFpsPolicy::Highest=>highest_camera_frame_rate(modes,current),
        CameraFpsPolicy::MatchOutput=>camera_frame_rate_matching_project(modes,current,output.0,output.1),
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraModeRecommendations {
    pub recommended: Option<VideoCaptureMode>,
    pub full_hd: Option<VideoCaptureMode>,
}
impl CameraModeRecommendations {
    pub fn from_modes(modes: &[CameraModeInfo]) -> Result<Self, CameraError> {
        validate(modes)?;
        let full_hd = best_camera_resolution_mode(modes, 1920, 1080)?;
        let realtime = modes
            .iter()
            .filter_map(|info| info.mode)
            .filter(|mode| usable_rate(*mode) && area(*mode) <= FULL_HD_PIXELS)
            .max_by(|a, b| area(*a).cmp(&area(*b)).then_with(|| rate_format(*a, *b)));
        let any = modes.iter().filter_map(|info| info.mode).max_by(|a, b| {
            usable_rate(*a)
                .cmp(&usable_rate(*b))
                .then_with(|| area(*a).cmp(&area(*b)))
                .then_with(|| rate_format(*a, *b))
        });
        Ok(Self {
            recommended: full_hd
                .filter(|mode| usable_rate(*mode))
                .or(realtime)
                .or(full_hd)
                .or(any),
            full_hd,
        })
    }
}
pub fn best_camera_resolution_mode(
    modes: &[CameraModeInfo],
    width: u32,
    height: u32,
) -> Result<Option<VideoCaptureMode>, CameraError> {
    validate(modes)?;
    Ok(modes
        .iter()
        .filter_map(|info| info.mode)
        .filter(|mode| mode.width() == width && mode.height() == height)
        .max_by(|a, b| rate_format(*a, *b)))
}
/// Select the fastest reported mode at the current resolution and exact native
/// format. The selected subtype is a separate camera property in OBS and must
/// not change as a side effect of choosing a frame rate.
pub fn highest_camera_frame_rate(
    modes: &[CameraModeInfo], current: VideoCaptureMode,
) -> Result<Option<VideoCaptureMode>, CameraError> {
    validate(modes)?;
    Ok(modes.iter().filter_map(|info| info.mode)
        .filter(|mode| mode.width() == current.width() && mode.height() == current.height()
            && mode.format() == current.format())
        .max_by(|a, b| {
            (u64::from(a.fps_num()) * u64::from(b.fps_den()))
                .cmp(&(u64::from(b.fps_num()) * u64::from(a.fps_den())))
                .then_with(|| b.fps_den().cmp(&a.fps_den()))
                .then_with(|| b.fps_num().cmp(&a.fps_num()))
        }))
}
/// Choose one reported fixed mode closest to the project's frame interval at
/// the selected resolution and exact format. OBS compares frame intervals,
/// rather than the absolute difference between the displayed FPS values.
/// This is a one-time choice, not an automatic binding policy.
pub fn camera_frame_rate_matching_project(
    modes: &[CameraModeInfo], current: VideoCaptureMode, project_num: u32, project_den: u32,
) -> Result<Option<VideoCaptureMode>, CameraError> {
    validate(modes)?;
    if project_num == 0 || project_den == 0 {
        return Err(CameraError::InvalidFrame("project frame rate is zero"));
    }
    let distance = |mode: VideoCaptureMode| {
        (u128::from(mode.fps_den()) * u128::from(project_num))
            .abs_diff(u128::from(project_den) * u128::from(mode.fps_num()))
    };
    Ok(modes.iter().filter_map(|info| info.mode)
        .filter(|mode| mode.width() == current.width() && mode.height() == current.height()
            && mode.format() == current.format())
        .min_by(|a, b| {
            (distance(*a) * u128::from(b.fps_num()))
                .cmp(&(distance(*b) * u128::from(a.fps_num())))
                // Prefer the faster rate for ties; preserve a deterministic
                // exact rational when cameras report equivalent fractions.
                .then_with(|| (u64::from(b.fps_num()) * u64::from(a.fps_den()))
                    .cmp(&(u64::from(a.fps_num()) * u64::from(b.fps_den()))))
                .then_with(|| a.fps_den().cmp(&b.fps_den()))
                .then_with(|| a.fps_num().cmp(&b.fps_num()))
        }))
}
pub fn arrange_camera_modes(modes: &mut [CameraModeInfo]) -> Result<(), CameraError> {
    validate(modes)?;
    modes.sort_unstable_by(|a, b| {
        (u64::from(b.width) * u64::from(b.height))
            .cmp(&(u64::from(a.width) * u64::from(a.height)))
            .then_with(|| b.width.cmp(&a.width))
            .then_with(|| b.height.cmp(&a.height))
            .then_with(|| reported_rate_desc(a, b))
            .then_with(|| b.mode.is_some().cmp(&a.mode.is_some()))
            .then_with(|| a.native_subtype.cmp(&b.native_subtype))
            .then_with(|| match (a.mode, b.mode) {
                (Some(a), Some(b)) => format_key(a.format()).cmp(&format_key(b.format())),
                _ => Ordering::Equal,
            })
            .then_with(|| a.fps_den.cmp(&b.fps_den))
            .then_with(|| a.fps_num.cmp(&b.fps_num))
    });
    Ok(())
}
fn validate(modes: &[CameraModeInfo]) -> Result<(), CameraError> {
    if modes.len() > LIMIT {
        return Err(CameraError::BudgetExceeded(
            "camera recommendation mode count",
        ));
    }
    for info in modes {
        if info.native_subtype.is_empty()
            || info.native_subtype.len() > 128
            || info.native_subtype.chars().any(char::is_control)
        {
            return Err(CameraError::InvalidFrame("camera native subtype"));
        }
        if let Some(mode) = info.mode {
            if (mode.width(), mode.height(), mode.fps_num(), mode.fps_den())
                != (info.width, info.height, info.fps_num, info.fps_den)
                || info.unsupported_reason.is_some()
            {
                return Err(CameraError::InvalidFrame(
                    "camera recommendation metadata mismatch",
                ));
            }
        } else if info.unsupported_reason.is_none() {
            return Err(CameraError::InvalidFrame(
                "unsupported camera mode lacks reason",
            ));
        }
    }
    Ok(())
}
fn reported_rate_desc(a: &CameraModeInfo, b: &CameraModeInfo) -> Ordering {
    match (a.fps_den != 0, b.fps_den != 0) {
        (true, true) => (u64::from(b.fps_num) * u64::from(a.fps_den))
            .cmp(&(u64::from(a.fps_num) * u64::from(b.fps_den))),
        (a, b) => b.cmp(&a),
    }
}
fn area(mode: VideoCaptureMode) -> u64 {
    u64::from(mode.width()) * u64::from(mode.height())
}
fn usable_rate(mode: VideoCaptureMode) -> bool {
    u64::from(mode.fps_num()) >= 24 * u64::from(mode.fps_den())
}
fn rate_format(a: VideoCaptureMode, b: VideoCaptureMode) -> Ordering {
    let distance = |mode: VideoCaptureMode| {
        u128::from(mode.fps_num()).abs_diff(30 * u128::from(mode.fps_den()))
    };
    // Greater means a better suggestion. Cross-products retain exact rational
    // rates including29.97fps and avoid overflow at the public u32 boundaries.
    usable_rate(a)
        .cmp(&usable_rate(b))
        .then_with(|| {
            (distance(b) * u128::from(a.fps_den())).cmp(&(distance(a) * u128::from(b.fps_den())))
        })
        .then_with(|| format_key(b.format()).cmp(&format_key(a.format())))
        .then_with(|| b.fps_den().cmp(&a.fps_den()))
        .then_with(|| b.fps_num().cmp(&a.fps_num()))
}
fn format_key(format: VideoPixelFormat) -> (u8, u8, u8, bool) {
    let yuv = |rank, matrix, range, chroma| {
        (
            rank,
            match matrix {
                VideoColorMatrix::Bt601 => 0,
                VideoColorMatrix::Bt709 => 1,
            },
            match range {
                VideoColorRange::Limited => 0,
                VideoColorRange::Full => 1,
            },
            chroma,
        )
    };
    match format {
        VideoPixelFormat::Mjpeg => (5,0,0,false),
        VideoPixelFormat::H264 => (9,0,0,false),
        VideoPixelFormat::Bgra8 => (0, 0, 0, false),
        VideoPixelFormat::Rgb32 => (1, 0, 0, false),
        VideoPixelFormat::Rgb24 => (2, 0, 0, false),
        VideoPixelFormat::Yuy2 {
            matrix,
            range,
            chroma_left,
        } => yuv(3, matrix, range, chroma_left),
        VideoPixelFormat::Uyvy { matrix, range, chroma_left } => yuv(6, matrix, range, chroma_left),
        VideoPixelFormat::I420 { matrix, range, chroma_left } => yuv(7, matrix, range, chroma_left),
        VideoPixelFormat::Yv12 { matrix, range, chroma_left } => yuv(8, matrix, range, chroma_left),
        VideoPixelFormat::Nv12 {
            matrix,
            range,
            chroma_left,
        } => yuv(4, matrix, range, chroma_left),
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
