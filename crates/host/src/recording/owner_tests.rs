//! Structural fake writers test ownership/publication only. Actual codecs have
//! the separate opt-in native acceptance and external decoder oracle.
use super::*;
#[path = "fanout_tests.rs"]
mod fanout_tests;
#[path = "live_render_tests.rs"]
mod live_render_tests;
#[path = "prepared_owner_tests.rs"]
mod prepared_owner_tests;
use crate::transition::Controller;
use skylive_core::{Command, Session};
use std::{
    io::Write,
    rc::Rc,
    time::{Duration, Instant},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "skylive-recording-test-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn output(&self) -> PathBuf {
        self.0.join("output.mp4")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".skylive-recording-")
                {
                    if let Ok(files) = fs::read_dir(&path) {
                        for file in files.flatten() {
                            let _ = fs::remove_file(file.path());
                        }
                    }
                    let _ = fs::remove_dir(path);
                } else {
                    let _ = fs::remove_file(path);
                }
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}
fn skeleton(path: &Path) {
    let mut file = fs::File::create(path).unwrap();
    for kind in [b"ftyp", b"moov", b"mdat"] {
        file.write_all(&8u32.to_be_bytes()).unwrap();
        file.write_all(kind).unwrap();
    }
}
#[derive(Debug, PartialEq)]
enum Call {
    Create(thread::ThreadId),
    Video(u64),
    Audio(u64, Vec<i16>),
    Finalize,
}
struct Fake {
    path: PathBuf,
    calls: Arc<Mutex<Vec<Call>>>,
    fail_video: bool,
    _worker_local: Rc<()>,
}
impl RecordingWriter for Fake {
    fn video(&mut self, _: &skylive_media::RgbaFrame, index: u64) -> Result<(), RecordingError> {
        self.calls.lock().unwrap().push(Call::Video(index));
        if self.fail_video {
            Err(RecordingError::invalid("injected writer failure"))
        } else {
            Ok(())
        }
    }
    fn audio(&mut self, pcm: &[i16], first: u64) -> Result<(), RecordingError> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Audio(first, pcm.to_vec()));
        Ok(())
    }
    fn finalize(&mut self) -> Result<(), RecordingError> {
        self.calls.lock().unwrap().push(Call::Finalize);
        skeleton(&self.path);
        Ok(())
    }
}
fn factory(
    calls: Arc<Mutex<Vec<Call>>>,
    fail_video: bool,
    gate: Option<mpsc::Receiver<()>>,
) -> RecordingFactory {
    Box::new(move |_, path| {
        calls
            .lock()
            .unwrap()
            .push(Call::Create(thread::current().id()));
        if let Some(gate) = gate {
            gate.recv().unwrap();
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        Ok(Box::new(Fake {
            path: path.into(),
            calls,
            fail_video,
            _worker_local: Rc::new(()),
        }))
    })
}
fn setup(path: PathBuf) -> (Session, RecordingConfig) {
    let mut session = Session::new();
    session
        .apply(Command::SetVideo {
            width: 64,
            height: 48,
            fps_num: 10,
            fps_den: 1,
        })
        .unwrap();
    let config = RecordingConfig::from_session(&session, path).unwrap();
    (session, config)
}
fn frame(session: &mut Session, index: u64) -> RecordingFrame {
    let time = MediaTime(index * 100_000_000);
    let output = Controller::new().sync(session, time).unwrap().output;
    RecordingFrame {
        session: session.identity(),
        time,
        model: session.snapshot(),
        output,
        playback: PlaybackSnapshot::at_time(session, time),
        capture: Arc::new(CaptureSnapshot::empty(session.identity())),
        resource_generation: 0,
    }
}
fn settle(owner: &mut RecordingOwner) -> RecordingEvent {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = owner.poll().unwrap() {
            return event;
        }
        assert!(
            Instant::now() < until,
            "fake recording acknowledgement deadline"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn send(owner: &mut RecordingOwner, frame: RecordingFrame) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match owner.video(frame.clone()) {
            Ok(()) => return,
            Err(error) if error.code == "recording_backpressure" => {}
            Err(error) => panic!("{error}"),
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
}
fn pcm(session: &Session, index: u64) -> Arc<ProgramAudioBlock> {
    Arc::new(ProgramAudioBlock {
        tracks:Vec::new(),
        instance: 1,
        epoch: 1,
        session: session.identity(),
        data_generation: 1,
        routing_serial: 1,
        settings: session.snapshot().audio(),
        start_frame: index,
        frames: 1,
        samples: vec![0.0; usize::from(session.snapshot().audio().speakers().channels())],
        program_meters: vec![],
        source_meters: vec![],
        source_meter_revision: None,
        clipped_samples: vec![],
        gaps: vec![],
    })
}

#[test]
fn worker_owns_non_send_writer_and_drains_admitted_frames_before_stop() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    for index in 0..3 {
        send(&mut owner, frame(&mut session, index));
    }
    owner.stop().unwrap();
    assert!(owner.video(frame(&mut session, 4)).is_err());
    assert!(matches!(
        settle(&mut owner),
        RecordingEvent::Finished {
            video_frames: 3,
            ..
        }
    ));
    owner.join().unwrap();
    assert!(directory.output().exists());
    let calls = calls.lock().unwrap();
    assert!(matches!(calls[0],Call::Create(id)if id!=thread::current().id()));
    assert_eq!(
        &calls[1..],
        &[
            Call::Video(0),
            Call::Video(1),
            Call::Video(2),
            Call::Finalize
        ]
    );
}
#[test]
fn bounded_queue_rejects_overload_and_pending_stop_is_retried_without_losing_prefix() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let (release, gate) = mpsc::sync_channel(0);
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, Some(gate)),
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while calls.lock().unwrap().is_empty() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    for index in 0..3 {
        owner.video(frame(&mut session, index)).unwrap();
    }
    assert_eq!(
        owner.video(frame(&mut session, 3)).unwrap_err().code,
        "recording_backpressure"
    );
    // Saturate independent audio and video permits. Reserved control admission
    // lets Stop follow that exact prefix without requiring GUI queue polling.
    for index in 0..40 {
        owner.audio(pcm(&session, index)).unwrap();
    }
    assert_eq!(
        owner.audio(pcm(&session, 40)).unwrap_err().code,
        "recording_backpressure"
    );
    owner.stop().unwrap();
    assert!(owner.status().finishing);
    release.send(()).unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut owner),
        RecordingEvent::Finished {
            video_frames: 3,
            ..
        }
    ));
    owner.join().unwrap();
}
#[test]
fn joined_close_releases_unread_final_acknowledgement_and_finishes_off_caller() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    send(&mut owner, frame(&mut session, 0));
    owner.stop().unwrap();
    // Intentionally leave Finished unread. join drops the receiver before wait.
    owner.join().unwrap();
    assert!(directory.output().exists());
    assert_eq!(calls.lock().unwrap().last(), Some(&Call::Finalize));
}
#[test]
fn writer_failure_preserves_recovery_file_and_never_publishes_destination() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(Default::default(), true, None),
        )
        .unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    send(&mut owner, frame(&mut session, 0));
    let RecordingEvent::Failed(error) = settle(&mut owner) else {
        panic!("expected injected failure")
    };
    owner.join().unwrap();
    assert!(error.recovery_path.unwrap().is_file());
    assert!(!directory.output().exists());
}
#[test]
fn empty_recording_has_explicit_failure_and_recoverable_owned_path() {
    let directory = Directory::new();
    let (session, config) = setup(directory.output());
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(Default::default(), false, None),
        )
        .unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    owner.stop().unwrap();
    let RecordingEvent::Failed(error) = settle(&mut owner) else {
        panic!("empty recording must fail")
    };
    owner.join().unwrap();
    assert_eq!(error.code, "recording_empty");
    assert!(error.recovery_path.unwrap().exists());
    assert!(!directory.output().exists());
}
#[test]
fn duplicate_and_skipped_video_ticks_are_accounted_without_duplicate_writes() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut active = Active::new(
        session.identity(),
        config,
        MediaTime(0),
        factory(calls.clone(), false, None),
        Arc::new(AudioState::new(1, session.identity())),
    )
    .unwrap();
    for index in [0, 0, 3, 2, 4] {
        active.video(frame(&mut session, index)).unwrap();
    }
    assert_eq!(active.status.video_frames, 3);
    assert_eq!(active.status.skipped_video_frames, 2);
    active.finish().unwrap();
    assert_eq!(
        &calls.lock().unwrap()[1..],
        &[
            Call::Video(0),
            Call::Video(3),
            Call::Video(4),
            Call::Finalize
        ]
    );
}
#[test]
fn stale_session_and_format_change_are_rejected_before_writer_receives_pixels() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut active = Active::new(
        session.identity(),
        config,
        MediaTime(0),
        factory(calls.clone(), false, None),
        Arc::new(AudioState::new(1, session.identity())),
    )
    .unwrap();
    let mut value = frame(&mut session, 0);
    value.session += 1;
    assert!(active.video(value).is_err());
    session
        .apply(Command::SetVideo {
            width: 128,
            height: 48,
            fps_num: 10,
            fps_den: 1,
        })
        .unwrap();
    assert!(active.video(frame(&mut session, 0)).is_err());
    assert_eq!(calls.lock().unwrap().len(), 1);
}
#[test]
fn destination_collision_and_publication_race_preserve_both_owners() {
    let directory = Directory::new();
    fs::write(directory.output(), b"other owner's bytes").unwrap();
    assert!(OwnedFile::prepare(&directory.output()).is_err());
    assert_eq!(
        fs::read(directory.output()).unwrap(),
        b"other owner's bytes"
    );
    fs::remove_file(directory.output()).unwrap();
    let file = OwnedFile::prepare(&directory.output()).unwrap();
    skeleton(&file.temporary);
    fs::write(directory.output(), b"racing writer").unwrap();
    let error = file.publish().unwrap_err();
    assert_eq!(error.recovery_path, Some(file.temporary.clone()));
    assert!(file.temporary.is_file());
    assert_eq!(fs::read(directory.output()).unwrap(), b"racing writer");
}
#[test]
fn publication_rejects_truncated_box_and_missing_final_index() {
    let directory = Directory::new();
    let file = OwnedFile::prepare(&directory.output()).unwrap();
    fs::write(&file.temporary, b"\0\0\0\x40mdat123").unwrap();
    assert!(file.publish().is_err());
    assert!(!directory.output().exists());
    fs::write(&file.temporary, b"\0\0\0\x08mdat").unwrap();
    assert!(file.publish().is_err());
    assert!(file.temporary.exists());
}
