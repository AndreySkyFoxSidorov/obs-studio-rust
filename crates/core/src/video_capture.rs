//! Exact authored camera identity/mode; no device handles or host dependency.
use crate::{Error, MAX_DISPLAY_PIXELS};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum VideoCaptureTarget {
    Windows { symbolic_link: String },
    Macos { unique_id: String },
}
impl VideoCaptureTarget {
    fn validate(&self) -> Result<(), Error> {
        let value = match self {
            Self::Windows { symbolic_link } => symbolic_link,
            Self::Macos { unique_id } => unique_id,
        };
        if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "camera identity requires 1..=4096 UTF-8 bytes without controls",
            ));
        }
        Ok(())
    }
    fn normalized(self) -> Self {
        match self {
            Self::Windows { symbolic_link } => Self::Windows {
                symbolic_link: symbolic_link.into_boxed_str().into_string(),
            },
            Self::Macos { unique_id } => Self::Macos {
                unique_id: unique_id.into_boxed_str().into_string(),
            },
        }
    }
}
/// Native raw camera formats and explicit compressed MJPEG transport. Unsupported
/// hardware subtypes are reported by host enumeration rather than mislabeled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum VideoPixelFormat {
    /// One independently decodable native MJPEG JPEG frame, not raw RGB pixels.
    Mjpeg,
    /// Native H.264 Annex-B access units; decoded on the capture owner.
    H264,
    Bgra8,
    Rgb32,
    Rgb24,
    Yuy2 {
        matrix: VideoColorMatrix,
        range: VideoColorRange,
        chroma_left: bool,
    },
    Uyvy {
        matrix: VideoColorMatrix,
        range: VideoColorRange,
        chroma_left: bool,
    },
    I420 {
        matrix: VideoColorMatrix,
        range: VideoColorRange,
        chroma_left: bool,
    },
    Yv12 {
        matrix: VideoColorMatrix,
        range: VideoColorRange,
        chroma_left: bool,
    },
    Nv12 {
        matrix: VideoColorMatrix,
        range: VideoColorRange,
        chroma_left: bool,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoColorMatrix {
    Bt601,
    Bt709,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoColorRange {
    Limited,
    Full,
}
impl VideoPixelFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mjpeg => "MJPEG",
            Self::H264 => "H.264",
            Self::Bgra8 => "BGRA",
            Self::Rgb32 => "RGB32",
            Self::Rgb24 => "RGB24",
            Self::Yuy2 { .. } => "YUY2",
            Self::Uyvy { .. } => "UYVY",
            Self::I420 { .. } => "I420",
            Self::Yv12 { .. } => "YV12",
            Self::Nv12 { .. } => "NV12",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ModeData")]
pub struct VideoCaptureMode {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    format: VideoPixelFormat,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeData {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    format: VideoPixelFormat,
}
impl TryFrom<ModeData> for VideoCaptureMode {
    type Error = Error;
    fn try_from(v: ModeData) -> Result<Self, Error> {
        Self::new(v.width, v.height, v.fps_num, v.fps_den, v.format)
    }
}
impl VideoCaptureMode {
    pub fn new(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        format: VideoPixelFormat,
    ) -> Result<Self, Error> {
        let result = Self {
            width,
            height,
            fps_num,
            fps_den,
            format,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn fps_num(self) -> u32 {
        self.fps_num
    }
    pub fn fps_den(self) -> u32 {
        self.fps_den
    }
    pub fn format(self) -> VideoPixelFormat {
        self.format
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        match self.format {
            VideoPixelFormat::Yuy2 { .. } | VideoPixelFormat::Uyvy { .. } if self.width % 2 != 0 => {
                return Err(Error::Invalid("packed 4:2:2 camera width must be even"));
            }
            VideoPixelFormat::Nv12 { .. } | VideoPixelFormat::I420 { .. } | VideoPixelFormat::Yv12 { .. } | VideoPixelFormat::H264
                if self.width % 2 != 0 || self.height % 2 != 0 => {
                return Err(Error::Invalid("4:2:0 camera width and height must be even"));
            }
            _ => {}
        }
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > MAX_DISPLAY_PIXELS
        {
            return Err(Error::Invalid(
                "camera mode exceeds 8192-edge/32M-pixel capture profile",
            ));
        }
        if self.fps_num == 0
            || self.fps_den == 0
            || self.fps_num < self.fps_den
            || u64::from(self.fps_num) > 240 * u64::from(self.fps_den)
        {
            return Err(Error::Invalid(
                "camera frame rate must be an exact 1..=240fps rational with 32-bit numerator/denominator",
            ));
        }
        Ok(())
    }
}
/// Authored frame-rate selection policy. The resolved device mode is runtime
/// state; automatic policies must be recomputed from current capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraFpsPolicy {
    #[default]
    Fixed,
    Highest,
    MatchOutput,
}
fn fixed_fps_policy(value: &CameraFpsPolicy) -> bool { *value == CameraFpsPolicy::Fixed }

/// Resolution and pixel format can either remain authored or be resolved from
/// the physical DirectShow device when the native stream is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraModePolicy {
    #[default]
    Fixed,
    DirectShowPreferred,
}
fn fixed_mode_policy(value: &CameraModePolicy) -> bool { *value == CameraModePolicy::Fixed }

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "BindingData")]
pub struct VideoCaptureBinding {
    target: VideoCaptureTarget,
    mode: VideoCaptureMode,
    #[serde(default, skip_serializing_if = "fixed_mode_policy")]
    mode_policy: CameraModePolicy,
    #[serde(default, skip_serializing_if = "fixed_fps_policy")]
    fps_policy: CameraFpsPolicy,
    deactivate_when_hidden: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    flip_vertically: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audio: Option<crate::AudioCaptureBinding>,
    /// Capture the camera's own DirectShow audio pin in the same native graph.
    #[serde(default, skip_serializing_if = "is_false")]
    embedded_audio: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingData {
    target: VideoCaptureTarget,
    mode: VideoCaptureMode,
    #[serde(default)]
    mode_policy: CameraModePolicy,
    #[serde(default)]
    fps_policy: CameraFpsPolicy,
    deactivate_when_hidden: bool,
    #[serde(default)]
    flip_vertically: bool,
    #[serde(default)]
    audio: Option<crate::AudioCaptureBinding>,
    #[serde(default)]
    embedded_audio: bool,
}
impl TryFrom<BindingData> for VideoCaptureBinding {
    type Error = Error;
    fn try_from(v: BindingData) -> Result<Self, Error> {
        Self::new(v.target, v.mode, v.deactivate_when_hidden)?
            .with_mode_policy(v.mode_policy).with_fps_policy(v.fps_policy).with_flip_vertically(v.flip_vertically).with_audio(v.audio)?.with_embedded_audio(v.embedded_audio)
    }
}
impl VideoCaptureBinding {
    pub fn new(
        target: VideoCaptureTarget,
        mode: VideoCaptureMode,
        deactivate_when_hidden: bool,
    ) -> Result<Self, Error> {
        let result = Self {
            target,
            mode,
            mode_policy: CameraModePolicy::Fixed,
            fps_policy: CameraFpsPolicy::Fixed,
            deactivate_when_hidden,
            flip_vertically: false,
            audio: None,
            embedded_audio: false,
        };
        result.validate()?;
        Ok(Self {
            target: result.target.normalized(),
            ..result
        })
    }
    pub fn target(&self) -> &VideoCaptureTarget {
        &self.target
    }
    pub fn mode(&self) -> VideoCaptureMode {
        self.mode
    }
    pub fn mode_policy(&self) -> CameraModePolicy { self.mode_policy }
    pub fn with_mode_policy(mut self, policy: CameraModePolicy) -> Self { self.mode_policy = policy; self }
    pub fn fps_policy(&self) -> CameraFpsPolicy { self.fps_policy }
    pub fn with_fps_policy(mut self, policy: CameraFpsPolicy) -> Self {
        self.fps_policy = policy;
        self
    }
    /// Construct a runtime binding using a resolved native mode while retaining
    /// the authored policy and presentation settings.
    pub fn with_mode(mut self, mode: VideoCaptureMode) -> Result<Self, Error> {
        mode.validate()?;
        self.mode = mode;
        Ok(self)
    }
    pub fn width(&self) -> u32 {
        self.mode.width()
    }
    pub fn height(&self) -> u32 {
        self.mode.height()
    }
    pub fn deactivate_when_hidden(&self) -> bool {
        self.deactivate_when_hidden
    }
    /// Source-level orientation, applied after native row normalization and before
    /// source filters. Independent of every scene item's own transform.
    pub fn flip_vertically(&self) -> bool { self.flip_vertically }
    pub fn with_flip_vertically(mut self, enabled: bool) -> Self {
        self.flip_vertically = enabled;
        self
    }
    /// Physical acquisition identity. Presentation and custom audio edits must
    /// not restart capture, renegotiate the selected mode or revoke manual Stop.
    pub fn same_device_mode(&self, other: &Self) -> bool {
        self.target == other.target && self.mode == other.mode && self.mode_policy == other.mode_policy && self.fps_policy == other.fps_policy && self.embedded_audio == other.embedded_audio
    }
    /// Manual AV activation policy excludes source presentation orientation.
    /// All physical, visibility and custom audio settings still participate.
    pub fn same_av_activation(&self, other: &Self) -> bool {
        self.same_device_mode(other)
            && self.deactivate_when_hidden == other.deactivate_when_hidden
            && self.audio == other.audio
    }
    /// Optional explicit WASAPI input, independent of the video device identity.
    pub fn audio(&self) -> Option<&crate::AudioCaptureBinding> { self.audio.as_ref() }
    pub fn embedded_audio(&self) -> bool { self.embedded_audio }
    pub fn with_embedded_audio(mut self, enabled: bool) -> Result<Self, Error> {
        self.embedded_audio = enabled;
        self.validate()?;
        Ok(self)
    }
    pub fn with_audio(mut self, audio: Option<crate::AudioCaptureBinding>) -> Result<Self, Error> {
        self.audio = audio;
        self.validate()?;
        Ok(self)
    }
    /// Native video owners must not reopen a camera when only its audio changes.
    pub fn video_only(&self) -> Self { Self { target:self.target.clone(),mode:self.mode,mode_policy:self.mode_policy,fps_policy:self.fps_policy,
        deactivate_when_hidden:self.deactivate_when_hidden,flip_vertically:self.flip_vertically,audio:None,embedded_audio:self.embedded_audio } }
    pub fn same_video(&self, other: &Self) -> bool {
        self.target == other.target && self.mode == other.mode && self.mode_policy == other.mode_policy && self.fps_policy == other.fps_policy && self.embedded_audio == other.embedded_audio
            && self.deactivate_when_hidden == other.deactivate_when_hidden
            && self.flip_vertically == other.flip_vertically
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.target.validate()?;
        self.mode.validate()?;
        if self.embedded_audio && (!matches!(self.target, VideoCaptureTarget::Windows {..}) || self.audio.is_some()) {
            return Err(Error::Invalid("embedded camera audio requires a Windows camera and excludes custom audio"));
        }
        if self.mode_policy == CameraModePolicy::DirectShowPreferred && !matches!(self.target, VideoCaptureTarget::Windows {..}) {
            return Err(Error::Invalid("DirectShow preferred mode requires a Windows camera"));
        }
        if let Some(audio) = &self.audio {
            audio.validate()?;
            if !matches!(self.target, VideoCaptureTarget::Windows {..})
                || audio.flow() != crate::AudioFlow::Input
                || !matches!(audio.target(), crate::AudioTarget::Default | crate::AudioTarget::WindowsEndpoint {..}) {
                return Err(Error::Invalid("camera custom audio requires a Windows input endpoint"));
            }
        }
        Ok(())
    }
}
fn is_false(value: &bool) -> bool { !*value }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_yuv_mode_keeps_color_identity_and_subsampling_constraints() {
        let left = VideoPixelFormat::Yuy2 {
            matrix: VideoColorMatrix::Bt709,
            range: VideoColorRange::Limited,
            chroma_left: true,
        };
        assert!(VideoCaptureMode::new(641, 480, 30, 1, left).is_err());
        assert!(
            VideoCaptureMode::new(
                640,
                481,
                30,
                1,
                VideoPixelFormat::Nv12 {
                    matrix: VideoColorMatrix::Bt601,
                    range: VideoColorRange::Full,
                    chroma_left: false
                }
            )
            .is_err()
        );
        let first = VideoCaptureMode::new(640, 480, 30000, 1001, left).unwrap();
        let second = VideoCaptureMode::new(
            640,
            480,
            30000,
            1001,
            VideoPixelFormat::Yuy2 {
                matrix: VideoColorMatrix::Bt601,
                range: VideoColorRange::Full,
                chroma_left: false,
            },
        )
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(first.format().label(), "YUY2");
        assert!(VideoCaptureMode::new(641, 481, 30, 1, VideoPixelFormat::Rgb24).is_ok());
    }
    #[test]
    fn native_mode_retains_fraction_and_durable_identity_without_pixels() {
        let mode = VideoCaptureMode::new(1920, 1080, 30000, 1001, VideoPixelFormat::Rgb32).unwrap();
        let binding = VideoCaptureBinding::new(
            VideoCaptureTarget::Windows {
                symbolic_link: r"\\?\usb#camera".into(),
            },
            mode,
            false,
        )
        .unwrap();
        assert_eq!(
            (
                binding.width(),
                binding.height(),
                binding.mode().fps_num(),
                binding.mode().fps_den()
            ),
            (1920, 1080, 30000, 1001)
        );
        assert!(!binding.deactivate_when_hidden());
    }
    #[test]
    fn impossible_mode_or_transient_empty_identity_rejects() {
        for (num, den) in [(0, 1), (30, 0), (241, 1), (1, 2)] {
            assert!(VideoCaptureMode::new(1, 1, num, den, VideoPixelFormat::Bgra8).is_err());
        }
        assert!(VideoCaptureMode::new(8192, 4097, 30, 1, VideoPixelFormat::Bgra8).is_err());
        let mode = VideoCaptureMode::new(1, 1, 30, 1, VideoPixelFormat::Bgra8).unwrap();
        for id in ["".into(), "x\0y".into(), "x".repeat(4097)] {
            assert!(
                VideoCaptureBinding::new(VideoCaptureTarget::Macos { unique_id: id }, mode, false)
                    .is_err()
            );
        }
    }
    #[test]
    fn source_rebinding_is_shared_scale_preserving_atomic_and_undoable() {
        use crate::{Command, Session, SourceKind};
        let mut session = Session::new();
        let scene = session.preview();
        let original = VideoCaptureBinding::new(
            VideoCaptureTarget::Windows {
                symbolic_link: "camera".into(),
            },
            VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap(),
            false,
        )
        .unwrap();
        let item = session
            .apply(Command::AddVideoCaptureSource {
                scene,
                name: "Camera".into(),
                binding: original.clone(),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        let other = session
            .apply(Command::AddScene {
                name: "Other".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::AddExistingSource {
                scene: other,
                source,
            })
            .unwrap();
        let before = session.snapshot();
        session
            .apply(Command::RebindVideoCapture {
                source,
                binding: original.clone(),
            })
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let updated = VideoCaptureBinding::new(
            original.target().clone(),
            VideoCaptureMode::new(4, 2, 60000, 1001, VideoPixelFormat::Rgb32).unwrap(),
            true,
        )
        .unwrap();
        session
            .apply(Command::RebindVideoCapture {
                source,
                binding: updated.clone(),
            })
            .unwrap();
        let current = session.snapshot();
        for id in [scene, other] {
            assert_eq!(current.scene(id).unwrap().items()[0].transform().width, 4.0);
        }
        assert!(
            matches!(current.source(source).unwrap().kind(),SourceKind::VideoCapture{binding}if binding==&updated)
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),SourceKind::VideoCapture{binding}if binding==&original)
        );
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].id(),
            item
        );
    }
}
