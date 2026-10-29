#![cfg(target_os = "windows")]
//! Explicit synthetic encode acceptance. No camera, microphone, screen or
//! playback device is opened. FFmpeg/ffprobe inspection is a separate root run.
use skylive_core::{Command, Session};
use skylive_host::{
    audio_program::ProgramAudioBlock,
    capture::CaptureSnapshot,
    playback::PlaybackSnapshot,
    recording::{RecordingConfig, RecordingEvent, RecordingFrame, RecordingOwner},
    transition::Controller,
};
use skylive_media::MediaTime;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

fn settle(owner: &mut RecordingOwner, want_finish: bool) -> RecordingEvent {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(event) = owner.poll().unwrap() {
            match event {
                RecordingEvent::Failed(error) => panic!("native encode: {error}"),
                RecordingEvent::Finished { .. } if want_finish => return event,
                RecordingEvent::Started if !want_finish => return event,
                _ => {}
            }
        }
        assert!(
            Instant::now() < until,
            "native recording acknowledgement deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
#[ignore = "explicit native H264/AAC encode to an owned MP4; root must independently inspect output"]
fn actual_program_video_and_synthetic_pcm_finalize_as_mp4() {
    let path = std::env::var_os("SKYLIVE_RECORDING_ACCEPTANCE_PATH")
        .map(PathBuf::from)
        .expect("explicit new acceptance output path is required");
    assert!(
        !path.exists(),
        "native acceptance never overwrites an existing file"
    );
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetVideo {
            width: 64,
            height: 48,
            fps_num: 10,
            fps_den: 1,
        })
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Synthetic recording color".into(),
            rgba: [220, 35, 70, 255],
        })
        .unwrap();
    let mut config = RecordingConfig::from_session(&session, path.clone()).unwrap();
    if std::env::var_os("SKYLIVE_RECORDING_SOFTWARE").is_some() {
        config.prefer_hardware = false;
    }
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start(session.identity(), config, MediaTime(0))
        .unwrap();
    settle(&mut owner, false);
    let mut controller = Controller::new();
    let mut audio_frame = 0u64;
    for frame in 0..6 {
        let time = MediaTime(frame * 100_000_000);
        let composition = controller.sync(&mut session, time).unwrap().output;
        let value = RecordingFrame {
            session: session.identity(),
            time,
            model: session.snapshot(),
            output: composition,
            playback: PlaybackSnapshot::at_time(&session, time),
            capture: Arc::new(CaptureSnapshot::empty(session.identity())),
            resource_generation: 0,
        };
        loop {
            match owner.video(value.clone()) {
                Ok(()) => break,
                Err(error) if error.code == "recording_backpressure" => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => panic!("recording frame: {error}"),
            }
        }
        let next = (frame + 1) * 4800;
        while audio_frame < next {
            let count = (next - audio_frame).min(1024) as usize;
            // A chirp has a unique correlation delay; a single tone cannot
            // distinguish encoder priming from an integer number of periods.
            let samples = (0..count)
                .flat_map(|index| {
                    let t = (audio_frame + index as u64) as f64 / 48000.0;
                    let value =
                        (std::f64::consts::TAU * (400.0 * t + 1000.0 * t * t)).sin() as f32 * 0.2;
                    [value, value]
                })
                .collect();
            let block = Arc::new(ProgramAudioBlock {
                tracks:Vec::new(),
        instance: 1,
                epoch: 1,
                session: session.identity(),
                data_generation: 1,
                routing_serial: 1,
                settings: session.snapshot().audio(),
                start_frame: audio_frame,
                frames: count,
                samples,
                program_meters: vec![],
                source_meters: vec![],
                source_meter_revision: None,
                clipped_samples: vec![],
                gaps: vec![],
            });
            loop {
                match owner.audio(block.clone()) {
                    Ok(()) => break,
                    Err(error) if error.code == "recording_backpressure" => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("recording audio: {error}"),
                }
            }
            audio_frame += count as u64;
        }
    }
    owner.stop().unwrap();
    let result = settle(&mut owner, true);
    assert!(
        matches!(
            result,
            RecordingEvent::Finished {
                video_frames: 6,
                audio_frames: 28800,
                ..
            }
        ),
        "{result:?}"
    );
    owner.join().unwrap();
    assert!(path.metadata().unwrap().len() > 256);
    println!("recording_artifact={}", path.display());
}
