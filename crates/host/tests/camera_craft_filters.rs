//! Injected camera port -> admitted CaptureUnion -> real filter renderer.
//! No native factory, device enumeration, OS camera or neural inference is used.
use skylive_core::{
    CRAFT_VIDEO_PLUGIN, Command, Id, Session, VideoCaptureBinding, VideoCaptureMode,
    VideoCaptureTarget, VideoEffect, VideoFilter, VideoPixelFormat,
};
use skylive_host::{
    camera_capture::{
        CameraCapabilities, CameraDeviceInfo, CameraError, CameraLimits, CameraModeInfo,
        CameraOwner, NativeCameraBackend, NativeCameraStream, NativeVideoFrame,
    },
    capture::{CaptureSnapshot, CaptureUnion},
    playback::PlaybackSnapshot,
    preview::render_output_with_capture,
    transition::{FrameComposition, FrameScene},
};
use skylive_media::{MediaTime, RgbaFrame};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct Backend;
struct Stream {
    sent: bool,
}
impl NativeCameraBackend for Backend {
    fn enumerate_devices(&mut self, _: CameraLimits) -> Result<Vec<CameraDeviceInfo>, CameraError> {
        Ok(vec![])
    }
    fn enumerate_modes(
        &mut self,
        _: &VideoCaptureTarget,
        _: CameraLimits,
    ) -> Result<Vec<CameraModeInfo>, CameraError> {
        Ok(vec![])
    }
    fn open(
        &mut self,
        _: &VideoCaptureBinding,
        _: CameraLimits,
    ) -> Result<Box<dyn NativeCameraStream>, CameraError> {
        Ok(Box::new(Stream { sent: false }))
    }
}
impl NativeCameraStream for Stream {
    fn poll(&mut self, _: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError> {
        if self.sent {
            return Ok(None);
        }
        self.sent = true;
        Ok(Some(NativeVideoFrame {
            image: RgbaFrame::solid(2, 2, [0, 255, 0, 255]).unwrap(),
            presentation_ns: 0,
            device_qpc_ns: None,
            directshow_clock: None,
            discontinuity: false,
            dropped_frames: 0,
        }))
    }
    fn stop(&mut self) -> Result<bool, CameraError> {
        Ok(true)
    }
}
fn camera() -> CameraOwner {
    CameraOwner::new(
        Box::new(|| Ok(Box::new(Backend))),
        CameraCapabilities {
            available: true,
            backend: "injected-camera-filter-test",
            video: true,
            audio: false,
            note: None,
        },
        CameraLimits::default(),
    )
    .unwrap()
}
fn fixture() -> (Session, Id, Id, VideoCaptureBinding) {
    let mut owner = Session::new();
    let scene = owner.preview();
    owner
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let binding = VideoCaptureBinding::new(
        VideoCaptureTarget::Windows {
            symbolic_link: "inert-camera-frame-fixture".into(),
        },
        VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap(),
        false,
    )
    .unwrap();
    owner
        .apply(Command::AddVideoCaptureSource {
            scene,
            name: "Camera".into(),
            binding: binding.clone(),
        })
        .unwrap();
    let source = owner.snapshot().sources()[0].id();
    (owner, scene, source, binding)
}
fn acquire(
    camera: &mut CameraOwner,
    owner: &Session,
    source: Id,
    binding: VideoCaptureBinding,
) -> Arc<CaptureSnapshot> {
    camera
        .synchronize_bindings(owner, vec![(source, binding)])
        .unwrap();
    camera.request_frame(owner, MediaTime(0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let _ = camera.poll(owner).unwrap();
        if !camera.is_busy() && camera.snapshot().frame(source).is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "injected camera frame deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
    CaptureUnion::default()
        .merge(
            owner,
            Arc::new(CaptureSnapshot::empty(owner.identity())),
            camera,
        )
        .unwrap()
}
fn pixel(owner: &Session, scene: Id, frame: &CaptureSnapshot) -> [u8; 4] {
    render_output_with_capture(
        &owner.snapshot(),
        &FrameComposition::Scene(FrameScene::Live(scene)),
        16,
        16,
        &PlaybackSnapshot::at_time(owner, MediaTime(0)),
        frame,
    )
    .unwrap()
    .pixels()[..4]
        .try_into()
        .unwrap()
}
#[test]
fn admitted_camera_frame_obeys_craft_add_bypass_enable_undo_and_native_roundtrip() {
    let (mut owner, scene, source, binding) = fixture();
    let mut port = camera();
    let frame = acquire(&mut port, &owner, source, binding.clone());
    assert!(
        frame
            .frame(source)
            .unwrap()
            .binding
            .matches_source(owner.snapshot().source(source).unwrap().kind())
    );
    assert_eq!(pixel(&owner, scene, &frame), [0, 255, 0, 255]);
    let filter = VideoFilter::new(VideoEffect::color_key()).unwrap();
    owner
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: vec![filter.clone()],
        })
        .unwrap();
    assert_eq!(
        pixel(&owner, scene, &frame),
        [0, 0, 0, 255],
        "actual camera green pixels become transparent over black"
    );
    let filter = owner.snapshot().source(source).unwrap().video_filters()[0].clone();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: CRAFT_VIDEO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    assert_eq!(pixel(&owner, scene, &frame), [0, 255, 0, 255]);
    assert_eq!(
        owner.snapshot().source(source).unwrap().video_filters(),
        std::slice::from_ref(&filter)
    );
    owner
        .apply(Command::SetPluginEnabled {
            plugin: CRAFT_VIDEO_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    assert_eq!(pixel(&owner, scene, &frame), [0, 0, 0, 255]);
    let bytes = skylive_host::storage::encode(&owner.snapshot()).unwrap();
    let restored = skylive_host::storage::decode(&bytes, true).unwrap();
    let mut restored_port = camera();
    let restored_frame = acquire(&mut restored_port, &restored, source, binding);
    assert_eq!(pixel(&restored, scene, &restored_frame), [0, 0, 0, 255]);
    owner.apply(Command::Undo).unwrap();
    assert_eq!(pixel(&owner, scene, &frame), [0, 255, 0, 255]);
    owner.apply(Command::Redo).unwrap();
    assert_eq!(pixel(&owner, scene, &frame), [0, 0, 0, 255]);
    // Capture envelopes pin worker resources until their last render owner retires.
    drop(frame);
    drop(restored_frame);
    port.join().unwrap();
    restored_port.join().unwrap();
}
