//! Actual asynchronous recording owner, injected writer, no native encoder.
use super::*;
use crate::recording::PreparedVideo;

#[derive(Default)]
struct State {
    video: Vec<(u64, Vec<u8>, Vec<u8>)>,
    drop_thread: Option<thread::ThreadId>,
}
struct Probe {
    path: PathBuf,
    state: Arc<Mutex<State>>,
}
impl RecordingWriter for Probe {
    fn video(
        &mut self,
        image: &skylive_media::RgbaFrame,
        index: u64,
    ) -> Result<(), RecordingError> {
        let nv12 = skylive_media::recording_nv12(image, 1024 * 1024, 1_000_000).unwrap();
        self.state
            .lock()
            .unwrap()
            .video
            .push((index, image.pixels().to_vec(), nv12));
        Ok(())
    }
    fn video_prepared(&mut self, image: &PreparedVideo, index: u64) -> Result<(), RecordingError> {
        self.state.lock().unwrap().video.push((
            index,
            image.image().pixels().to_vec(),
            image.nv12().to_vec(),
        ));
        Ok(())
    }
    fn audio(&mut self, _: &[i16], _: u64) -> Result<(), RecordingError> {
        Err(RecordingError::invalid(
            "audio not part of prepared video test",
        ))
    }
    fn finalize(&mut self) -> Result<(), RecordingError> {
        skeleton(&self.path);
        Ok(())
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.state.lock().unwrap().drop_thread = Some(thread::current().id());
    }
}

#[test]
fn repeated_visual_samples_still_reach_writer_with_fresh_pts_and_success_counters() {
    let directory = Directory::new();
    let (mut session, mut config) = setup(directory.output());
    config.audio = None;
    session
        .apply(Command::AddColorSource {
            scene: session.preview(),
            name: "Repeated red".into(),
            rgba: [255, 0, 0, 255],
        })
        .unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    let shared = state.clone();
    let factory: RecordingFactory = Box::new(move |_, path| {
        Ok(Box::new(Probe {
            path: path.into(),
            state: shared,
        }))
    });
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(session.identity(), config, MediaTime(0), factory)
        .unwrap();
    for index in 0..5 {
        let mut value = frame(&mut session, index);
        value.resource_generation = if index >= 3 { 1 } else { 0 };
        send(&mut owner, value);
        let until = Instant::now() + Duration::from_secs(5);
        while owner.status().video_frames != index + 1 {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
    }
    let status = owner.status();
    assert_eq!(
        (
            status.prepared_video_cache_hits,
            status.prepared_video_cache_misses
        ),
        (3, 2)
    );
    owner.stop().unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut owner),
        RecordingEvent::Finished {
            video_frames: 5,
            audio_frames: 0,
            ..
        }
    ));
    owner.join().unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.video.len(), 5);
    for (index, rgba, nv12) in &state.video {
        assert_eq!(
            *index,
            state
                .video
                .iter()
                .position(|entry| entry.0 == *index)
                .unwrap() as u64
        );
        assert_eq!(rgba, &state.video[0].1);
        assert_eq!(nv12, &state.video[0].2);
    }
    assert!(
        state
            .drop_thread
            .is_some_and(|thread| thread != thread::current().id())
    );
}

#[test]
fn degraded_frames_never_enter_cache_and_explicit_same_model_reload_recovers_real_pixels() {
    let directory = Directory::new();
    let path = directory.0.join("resource.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
        .save(&path)
        .unwrap();
    let asset = crate::assets::prepare_image(&path).unwrap().asset;
    let bytes = fs::read(&path).unwrap();
    let (mut session, mut config) = setup(directory.output());
    config.audio = None;
    session
        .apply(Command::AddImageSource {
            scene: session.preview(),
            name: "Reloaded PNG".into(),
            asset,
        })
        .unwrap();
    let canonical = session.snapshot();
    fs::remove_file(&path).unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    let shared = state.clone();
    let factory: RecordingFactory = Box::new(move |_, path| {
        Ok(Box::new(Probe {
            path: path.into(),
            state: shared,
        }))
    });
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(session.identity(), config, MediaTime(0), factory)
        .unwrap();
    for index in 0..4 {
        if index == 2 {
            fs::write(&path, &bytes).unwrap();
        }
        let mut value = frame(&mut session, index);
        value.resource_generation = if index >= 2 { 1 } else { 0 };
        send(&mut owner, value);
        let until = Instant::now() + Duration::from_secs(5);
        while owner.status().video_frames != index + 1 {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
    }
    let status = owner.status();
    assert_eq!(
        (
            status.prepared_video_cache_hits,
            status.prepared_video_cache_misses
        ),
        (1, 3)
    );
    assert_eq!(status.degraded_video_frames, 2);
    assert!(status.warnings.is_none());
    assert!(
        Arc::ptr_eq(&canonical, &session.snapshot()),
        "Reload resource generation does not create authored history"
    );
    owner.stop().unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut owner),
        RecordingEvent::Finished {
            video_frames: 4,
            audio_frames: 0,
            ..
        }
    ));
    owner.join().unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.video.len(), 4);
    for entry in &state.video[..2] {
        assert_eq!(&entry.1[..4], &[0, 0, 0, 255]);
    }
    for entry in &state.video[2..] {
        assert_eq!(&entry.1[..4], &[255, 0, 0, 255]);
    }
}
