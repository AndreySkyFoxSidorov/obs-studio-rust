//! Opt-in native codec acceptance after the deterministic transport checkpoint.
//! Physical capture/playback are never used: only the MP4 writer is native.
use super::*;
use std::io::Write;

#[derive(Clone, serde::Serialize)]
struct PcmWindow {
    first: u64,
    frames: usize,
    rms: f64,
    peak: f64,
    tone: f64,
    phase: f64,
    quiet: usize,
}
fn window(first: u64, samples: impl Iterator<Item = f64>) -> PcmWindow {
    let (mut square, mut peak, mut sin, mut cos, mut count, mut quiet) =
        (0.0f64, 0.0f64, 0.0, 0.0, 0usize, 0usize);
    for sample in samples {
        let angle = std::f64::consts::TAU * 1000.0 * (first + count as u64) as f64 / 48000.0;
        square += sample * sample;
        peak = peak.max(sample.abs());
        sin += sample * angle.sin();
        cos += sample * angle.cos();
        quiet += usize::from(sample.abs() < 1e-5);
        count += 1;
    }
    PcmWindow {
        first,
        frames: count,
        rms: (square / count as f64).sqrt(),
        peak,
        tone: 2.0 * sin.hypot(cos) / count as f64,
        phase: (-sin).atan2(cos),
        quiet,
    }
}
struct TappedWriter {
    inner: Box<dyn RecordingWriter>,
    windows: Arc<Mutex<Vec<PcmWindow>>>,
}
impl RecordingWriter for TappedWriter {
    fn video(
        &mut self,
        frame: &skylive_media::RgbaFrame,
        index: u64,
    ) -> Result<(), RecordingError> {
        self.inner.video(frame, index)
    }
    fn audio(&mut self, pcm: &[i16], first: u64) -> Result<(), RecordingError> {
        self.inner.audio(pcm, first)?;
        let stats = window(
            first,
            pcm.chunks_exact(2)
                .map(|frame| f64::from(frame[0]) / if frame[0] < 0 { 32768.0 } else { 32767.0 }),
        );
        let mut windows = self.windows.lock().unwrap();
        assert!(windows.len() < 720);
        windows.push(stats);
        Ok(())
    }
    fn finalize(&mut self) -> Result<(), RecordingError> {
        self.inner.finalize()
    }
}

struct RealtimeBackend {
    started: Instant,
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    owner: thread::ThreadId,
    _local: Rc<()>,
}
struct RealtimeStream {
    host_started: Instant,
    packet_started: Instant,
    zero: u64,
    produced: u64,
    closes: Arc<AtomicUsize>,
    owner: thread::ThreadId,
    closed: bool,
    _local: Rc<()>,
}
impl NativeAudioBackend for RealtimeBackend {
    fn capabilities(&self) -> AudioCapabilities {
        CAPS
    }
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        Ok(1_000_000_000 + self.started.elapsed().as_nanos() as u64)
    }
    fn enumerate(
        &mut self,
        _: AudioFlow,
        _: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        panic!("synthetic acceptance must not enumerate an OS audio device");
    }
    fn open(
        &mut self,
        _: &AudioCaptureBinding,
        _: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(RealtimeStream {
            host_started: self.started,
            packet_started: Instant::now(),
            zero: self.host_now_ns()?,
            produced: 0,
            closes: self.closes.clone(),
            owner: self.owner,
            closed: false,
            _local: Rc::new(()),
        }))
    }
}
impl NativeAudioStream for RealtimeStream {
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        if u128::from(self.produced + 256)
            > self.packet_started.elapsed().as_nanos() * 48000 / 1_000_000_000
        {
            return Ok(None);
        }
        let first = self.produced;
        self.produced += 256;
        let samples = (first..first + 256)
            .flat_map(|frame| {
                let value =
                    (0.1 * (std::f64::consts::TAU * 1000.0 * frame as f64 / 48000.0).sin()) as f32;
                [value, value]
            })
            .collect();
        Ok(Some(NativePcmBlock {
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 2,
                channel_mask: 3,
            },
            frames: 256,
            samples,
            device_timestamp_ns: Some(self.zero + first * 1_000_000_000 / 48000),
            graph_timestamp_ns: None,
            directshow_clock: None,
            host_received_ns: 1_000_000_000 + self.host_started.elapsed().as_nanos() as u64,
            device_frame_position: Some(first),
            flags: NativePacketFlags::default(),
        }))
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        if !self.closed {
            self.closed = true;
            self.closes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[test]
#[ignore = "Explicit native codec output acceptance; set SKYLIVE_RECORDING_FANOUT_ACCEPTANCE_DIRECTORY, never uses physical audio capture/playback"]
fn native_h264_aac_fanout_without_gui_polling_has_independent_decoded_tone() {
    let root = PathBuf::from(
        std::env::var_os("SKYLIVE_RECORDING_FANOUT_ACCEPTANCE_DIRECTORY")
            .expect("explicit owned evidence directory required"),
    );
    assert!(root.is_absolute());
    fs::create_dir_all(&root).unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = root.join(format!("native-fanout-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("synthetic-program.mp4");
    let (mut session, mut config) = setup(path.clone());
    config.prefer_hardware = false;
    let scene = session.preview();
    let binding =
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, true, false).unwrap();
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Synthetic1kHz".into(),
            binding,
        })
        .unwrap()
        .created
        .unwrap();
    let source = session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|entry| entry.id() == item)
        .unwrap()
        .source();
    let mut eq = AudioFilter::simple_eq();
    if let AudioFilter::SimpleEq { mid_db, .. } = &mut eq {
        *mid_db = 6.0;
    }
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![eq, AudioFilter::gate()],
        })
        .unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let (o, c) = (opens.clone(), closes.clone());
    let mut audio = AudioCaptureWorker::with_limits(
        Box::new(move || {
            Ok(Box::new(RealtimeBackend {
                started: Instant::now(),
                opens: o,
                closes: c,
                owner: thread::current().id(),
                _local: Rc::new(()),
            }))
        }),
        CAPS,
        AudioCaptureLimits::default(),
    )
    .unwrap();
    audio
        .synchronize(&session, &[crate::transition::FrameScene::Live(scene)])
        .unwrap();
    let mut recorder = RecordingOwner::new().unwrap();
    let before_native = Arc::new(Mutex::new(Vec::<PcmWindow>::with_capacity(720)));
    let tap = before_native.clone();
    let native = crate::recording::native_factory();
    recorder
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            Box::new(move |config, path| {
                Ok(Box::new(TappedWriter {
                    inner: native(config, path)?,
                    windows: tap,
                }))
            }),
        )
        .unwrap();
    recorder.video(frame(&mut session, 0)).unwrap();
    let sink = recorder.audio_sink(session.snapshot().audio()).unwrap();
    audio
        .request_program_with_recording(
            &session,
            &crate::transition::FrameComposition::Scene(crate::transition::FrameScene::Live(scene)),
            MediaTime(0),
            sink,
        )
        .unwrap();
    // No control acknowledgement, GUI PCM/meter read, or Started event is read
    // during >1s of autonomous native encoding. Status is metadata only.
    let until = Instant::now() + Duration::from_secs(15);
    while recorder.status().audio_frames < 49_152 {
        assert!(
            Instant::now() < until,
            "native audio progress deadline: {:?}",
            recorder.status()
        );
        thread::sleep(Duration::from_millis(2));
    }
    recorder.video(frame(&mut session, 9)).unwrap();
    recorder.stop().unwrap();
    while recorder.status().active {
        assert!(
            Instant::now() < until,
            "native finalize remains owned: {:?}",
            recorder.status()
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(settle(&mut recorder), RecordingEvent::Started));
    let RecordingEvent::Finished {
        audio_frames,
        video_frames,
        ..
    } = settle(&mut recorder)
    else {
        panic!("native fanout failed: {:?}", recorder.status());
    };
    assert_eq!(video_frames, 2);
    assert!(audio_frames >= 49_152);
    assert_eq!(recorder.status().audio_gap_frames, 0);
    // Only after recording/end-barrier completion do we consume the source
    // control acknowledgement and query final transport metadata. No PCM read.
    while audio.is_busy() {
        let _ = audio.poll(&session).unwrap();
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    let now = audio.program_now().unwrap().unwrap();
    audio
        .request_program(
            &session,
            &crate::transition::FrameComposition::Scene(crate::transition::FrameScene::Live(scene)),
            now,
        )
        .unwrap();
    while audio.is_busy() {
        let _ = audio.poll(&session).unwrap();
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    let transport_stats = audio.program_status().cloned();
    recorder.join().unwrap();
    audio.join().unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    let bytes = fs::metadata(&path).unwrap().len();
    assert!(bytes < 16 * 1024 * 1024);
    let mut reader = crate::mediafile::MediaFileReader::open(&path, Default::default()).unwrap();
    let info = reader.info().clone();
    let mut pictures = 0;
    while let Some(picture) = reader.next_video().unwrap() {
        assert_eq!((picture.width, picture.height), (64, 48));
        assert!((i16::from(picture.y[24 * picture.y_stride + 32]) - 16).abs() <= 8);
        pictures += 1;
        assert!(pictures <= 2);
    }
    assert_eq!(pictures, 2);
    let mut mono = vec![];
    let mut packets = 0;
    while let Some(block) = reader.next_audio().unwrap() {
        packets += 1;
        assert!(packets <= 100);
        assert_eq!((block.sample_rate, block.channels), (48000, 2));
        mono.extend(block.samples.chunks_exact(2).map(|frame| {
            assert!((frame[0] - frame[1]).abs() < 0.01);
            frame[0]
        }));
    }
    let decoded_frames = u64::try_from(mono.len()).unwrap();
    assert!(
        (audio_frames.saturating_sub(2048)..=audio_frames.checked_add(4096).unwrap())
            .contains(&decoded_frames),
        "decoded AAC count must follow actual accepted PCM, allowing priming/tail only: encoded={audio_frames}, decoded={decoded_frames}"
    );
    let tail = &mono[8192..];
    let amplitude = |hz: f64| {
        let (mut sin, mut cos) = (0.0, 0.0);
        for (index, &sample) in tail.iter().enumerate() {
            let angle = std::f64::consts::TAU * hz * index as f64 / 48000.0;
            sin += f64::from(sample) * angle.sin();
            cos += f64::from(sample) * angle.cos();
        }
        2.0 * sin.hypot(cos) / tail.len() as f64
    };
    let tone = amplitude(1000.0);
    let off = amplitude(3000.0);
    let decoded_windows: Vec<_> = mono
        .chunks(1024)
        .enumerate()
        .map(|(index, samples)| {
            window(
                index as u64 * 1024,
                samples.iter().map(|&sample| f64::from(sample)),
            )
        })
        .collect();
    let diagnostics = serde_json::json!({"encoded_audio_frames":audio_frames,"decoded_audio_frames":mono.len(),"program_transport":transport_stats,"before_native_writer":*before_native.lock().unwrap(),"decoded_windows":decoded_windows,"tail_tone":tone,"tail_offtone":off,"audio_grid_gap_frames":recorder.status().audio_gap_frames});
    let diagnostic_path = directory.join("diagnostics.json");
    let mut diagnostic_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&diagnostic_path)
        .unwrap();
    serde_json::to_writer_pretty(&mut diagnostic_file, &diagnostics).unwrap();
    diagnostic_file.write_all(b"\n").unwrap();
    diagnostic_file.sync_all().unwrap();
    eprintln!(
        "Native fanout diagnostics before assertions: {} (encoded_frames={audio_frames},decoded_frames={},tone={tone})",
        diagnostic_path.display(),
        mono.len()
    );
    assert!(
        (0.18..=0.22).contains(&tone),
        "literal0.1 amplitude with+6dB mid EQ must decode near0.199526: {tone}"
    );
    assert!(off < 0.01);
    let receipt = serde_json::json!({"path":path,"bytes":bytes,"decoded_video_frames":pictures,"encoded_audio_frames":audio_frames,"decoded_audio_frames":mono.len(),"tone_1000hz_amplitude":tone,"off_3000hz_amplitude":off,"gui_pcm_polls":0,"unread_started_during_capture":true,"synthetic_backend":true,"physical_audio_capture":false,"audio_playback":false,"native_writer":true,"requested_prefer_hardware":false,"actual_encoder_identity":"not queried","media_info":info});
    let receipt_path = directory.join("acceptance.json");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&receipt_path)
        .unwrap();
    serde_json::to_writer_pretty(&mut file, &receipt).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    eprintln!(
        "Native synthetic fanout evidence: {}",
        receipt_path.display()
    );
}
