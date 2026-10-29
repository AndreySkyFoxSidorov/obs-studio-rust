//! Ignored composition acceptance: synthetic owned Program frames/PCM only;
//! actual Windows encoders and explicit localhost RTMP, never physical devices.
use super::*;
use super::{backend::*, timing::Track, video::Renderer};
use crate::{
    audio_capture::ProgramCaptureKey,
    audio_output::{AudioOutput, ProgramOutputRouter},
    capture::CaptureSnapshot,
    playback::PlaybackSnapshot,
    rtmp::streaming_test_support::Loopback,
    transition::Controller,
};
use skylive_core::{Command, Session, StreamingSettings};
use std::{
    io::Write,
    sync::{Mutex, atomic::AtomicUsize},
    thread,
    time::{Duration, Instant},
};
type Observation = (bool, i64, i64);
struct TapFactory {
    native: NativeFactory,
    times: Arc<Mutex<Vec<Observation>>>,
}
struct TapVideo {
    port: Box<dyn VideoPort>,
    times: Arc<Mutex<Vec<Observation>>>,
}
struct TapAudio {
    port: Box<dyn AudioPort>,
    times: Arc<Mutex<Vec<Observation>>>,
}
fn observe(e: &CodecEvent, times: &Mutex<Vec<Observation>>) {
    if let CodecEvent::Packets { packets, .. } = e {
        let mut t = times.lock().unwrap();
        assert!(t.len() + packets.len() <= 128);
        for p in packets {
            t.push((p.track == Track::Audio, p.dts, p.duration));
        }
    }
}
impl Factory for TapFactory {
    fn video(&mut self, c: &Config, k: StreamKey) -> Result<Box<dyn VideoPort>, Error> {
        Ok(Box::new(TapVideo {
            port: self.native.video(c, k)?,
            times: self.times.clone(),
        }))
    }
    fn audio(&mut self, c: &Config, k: StreamKey) -> Result<Box<dyn AudioPort>, Error> {
        Ok(Box::new(TapAudio {
            port: self.native.audio(c, k)?,
            times: self.times.clone(),
        }))
    }
    fn publisher(
        &mut self,
        c: &Config,
        k: StreamKey,
        v: Arc<[u8]>,
        a: Option<Arc<[u8]>>,
    ) -> Result<Box<dyn PublisherPort>, Error> {
        self.native.publisher(c, k, v, a)
    }
    fn renderer(&mut self) -> Box<dyn Renderer> {
        self.native.renderer()
    }
}
impl VideoPort for TapVideo {
    fn busy(&self) -> bool {
        self.port.busy()
    }
    fn closed(&self) -> bool {
        self.port.closed()
    }
    fn encode(&mut self, i: u64, b: Box<[u8]>) -> Result<(), Error> {
        self.port.encode(i, b)
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.port.finish()
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        let e = self.port.poll()?;
        if let Some(e) = &e {
            observe(e, &self.times);
        }
        Ok(e)
    }
    fn close(&mut self) {
        self.port.close();
    }
}
impl AudioPort for TapAudio {
    fn busy(&self) -> bool {
        self.port.busy()
    }
    fn closed(&self) -> bool {
        self.port.closed()
    }
    fn encode(&mut self, i: u64, b: Box<[i16]>) -> Result<(), Error> {
        self.port.encode(i, b)
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.port.finish()
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        let e = self.port.poll()?;
        if let Some(e) = &e {
            observe(e, &self.times);
        }
        Ok(e)
    }
    fn close(&mut self) {
        self.port.close();
    }
}
fn fresh(s: &mut Session, ns: u64) -> ProgramVideoFrame {
    let time = MediaTime(ns);
    let output = Controller::new().sync(s, time).unwrap().output;
    ProgramVideoFrame {
        session: s.identity(),
        time,
        model: s.snapshot(),
        output,
        playback: PlaybackSnapshot::at_time(s, time),
        capture: Arc::new(CaptureSnapshot::empty(s.identity())),
        resource_generation: 0,
    }
}
fn reference(frame: usize, channel: usize) -> f64 {
    let t = frame as f64 / 48000.;
    let (f, m, phase, a) = if channel == 0 {
        (1337., 17., 0.31, 0.18)
    } else {
        (2633., 23., -0.47, 0.11)
    };
    a * (1. + 0.2 * (std::f64::consts::TAU * m * t).sin())
        * (std::f64::consts::TAU * f * t + phase).sin()
}
fn deadline(until: Instant) {
    assert!(
        Instant::now() < until,
        "native composition deadline: actual nested owners may still be retiring"
    );
}
#[test]
#[ignore = "explicit opt-in actual Windows AAC/H264 + localhost RTMP composition; no physical capture"]
fn native_program_common_origin_packets_decode_and_every_owner_retires() {
    assert_eq!(
        std::env::var("SKYLIVE_STREAMING_LOOPBACK_ACCEPTANCE").as_deref(),
        Ok("1")
    );
    let evidence =
        crate::native_packet_evidence::Directory::from_env("SKYLIVE_STREAMING_EVIDENCE_DIR")
            .unwrap();
    let server = Loopback::bind();
    let mut s = Session::new();
    s.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    })
    .unwrap();
    let scene = s.preview();
    s.apply(Command::AddColorSource {
        scene,
        name: "Synthetic red Program".into(),
        rgba: [255, 0, 0, 255],
    })
    .unwrap();
    let model = s.snapshot();
    let settings = StreamingSettings::new(
        format!("rtmp://{}/live", server.address),
        "literal/key?token=abc".into(),
        1000,
        1,
        true,
        128,
    )
    .unwrap();
    let config = Config::new(
        s.identity(),
        model.profiles().selected_id(),
        model.video().clone(),
        model.audio(),
        settings,
    )
    .unwrap();
    let observations = Arc::new(Mutex::new(vec![]));
    let factory = Box::new(TapFactory {
        native: NativeFactory,
        times: observations.clone(),
    });
    let StartedOwner {
        mut owner,
        audio_sink,
    } = StreamingOwner::with_factory(config, factory, Arc::new(AtomicUsize::new(0))).unwrap();
    let key = ProgramCaptureKey {
        session: s.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: model.audio(),
    };
    let mut router = ProgramOutputRouter::default();
    router
        .attach(key, AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(25);
    loop {
        match owner.poll() {
            Some(Event::ReadyForInputs { .. }) => break,
            Some(e) => panic!("unexpected startup: {e:?}"),
            None => {
                deadline(until);
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    owner
        .activate(owner.key(), owner.epoch(), fresh(&mut s, 1))
        .unwrap();
    while owner.status().is_none_or(|s| s.origin.is_none()) {
        deadline(until);
        thread::sleep(Duration::from_millis(1));
    }
    let total = 48123usize;
    let mut fed = 0usize;
    let mut video = 1u64;
    let mut video_sealed = false;
    // No Started/event or GUI audio poll occurs during the entire >8-block run.
    while fed < total || video < 12 {
        deadline(until);
        let status = owner.status().unwrap();
        assert!(status.failure.is_none(), "{status:?}");
        if fed < total && (fed as u64).saturating_sub(status.audio_input_frames) < 12 * 1024 {
            let frames = (total - fed).min(1024);
            let samples = (0..frames)
                .flat_map(|i| (0..2).map(move |ch| reference(1 + fed + i, ch) as f32))
                .collect();
            router.submit(
                key,
                Arc::new(crate::audio_program::ProgramAudioBlock {
                    tracks:Vec::new(),
        instance: 1,
                    epoch: 1,
                    session: s.identity(),
                    data_generation: 1,
                    routing_serial: 1,
                    settings: model.audio(),
                    start_frame: 1 + fed as u64,
                    frames,
                    samples,
                    program_meters: vec![],
                    source_meters: vec![],
                    source_meter_revision: None,
                    clipped_samples: vec![],
                    gaps: vec![],
                }),
            );
            fed += frames;
        }
        if video < 12 {
            let ns = 1 + (u128::from(video) * 1_000_000_000u128).div_ceil(30) as u64;
            match owner.video(owner.key(), owner.epoch(), fresh(&mut s, ns)) {
                Ok(()) => video += 1,
                Err(e) if matches!(e.error, Error::Limit(_)) => {}
                Err(e) => panic!("{e:?}"),
            }
        }
        if video == 12 && !video_sealed {
            owner.finish_video(owner.key(), owner.epoch()).unwrap();
            video_sealed = true;
        }
        router.prune();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(video_sealed);
    owner.stop();
    router.prune();
    while !owner.is_closed() {
        deadline(until);
        router.prune();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        owner.poll(),
        Some(Event::Started {
            origin: MediaTime(1),
            ..
        })
    ));
    let Some(Event::Stopped(status)) = owner.poll() else {
        panic!("actual successful terminal: {:?}", owner.status())
    };
    assert_eq!(status.video_frames, 12);
    assert_eq!(status.skipped_video_frames, 0);
    assert_eq!(status.audio_input_frames, total as u64);
    assert_eq!(status.audio_converted_frames, total as u64);
    assert_eq!(status.audio_gap_frames, 0);
    assert_eq!(status.native_aac.explicit_padding_frames, 5);
    assert_eq!(status.video_packets, 12);
    assert_eq!(status.audio_packets, 47);
    assert_eq!(status.packets_admitted, 59);
    assert_eq!(status.packets_published, 59);
    let tags = server.finish();
    assert!(tags.len() <= 128);
    let video_config = tags
        .iter()
        .find(|t| t.type_id == 9 && t.data.get(1) == Some(&0))
        .unwrap();
    let video_config =
        crate::native_h264::CodecConfiguration::parse(&video_config.data[5..]).unwrap();
    let audio_config = tags
        .iter()
        .find(|t| t.type_id == 8 && t.data.get(1) == Some(&0))
        .unwrap();
    let mut audio = skylive_aac::Decoder::new(&audio_config.data[2..]).unwrap();
    let mut h264 = skylive_h264::Decoder::new();
    let mut pictures = vec![];
    let mut decoded = [vec![], vec![]];
    let observations = observations.lock().unwrap();
    let video_times: Vec<_> = observations.iter().filter(|p| !p.0).collect();
    let audio_times: Vec<_> = observations.iter().filter(|p| p.0).collect();
    assert_eq!(video_times.len(), 12);
    assert_eq!(audio_times.len(), 47);
    let shift = (-i128::from(video_times[0].1).min(i128::from(audio_times[0].1) + 208)).max(0);
    let mut vi = 0;
    let mut ai = 0;
    let mut wire_dts = vec![];
    for tag in &tags {
        if tag.type_id == 9 && tag.data.get(1) == Some(&1) {
            assert_eq!(
                u128::from(tag.timestamp),
                ((i128::from(video_times[vi].1) + shift) * 100 / 1_000_000) as u128
            );
            let mut annex = vec![];
            if vi == 0 {
                for n in video_config.sps().iter().chain(video_config.pps()) {
                    annex.extend_from_slice(&[0, 0, 0, 1]);
                    annex.extend_from_slice(n);
                }
            }
            let bytes = &tag.data[5..];
            let mut at = 0;
            while at < bytes.len() {
                let n = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
                at += 4;
                annex.extend_from_slice(&[0, 0, 0, 1]);
                annex.extend_from_slice(&bytes[at..at + n]);
                at += n;
            }
            pictures.extend(h264.decode(&annex, video_times[vi].1).unwrap());
            wire_dts.push((i128::from(video_times[vi].1) + shift, false));
            vi += 1;
        } else if tag.type_id == 8 && tag.data.get(1) == Some(&1) {
            assert_eq!(
                u128::from(tag.timestamp),
                ((i128::from(audio_times[ai].1) + 208 + shift) * 100 / 1_000_000) as u128
            );
            let pcm = audio.decode(&tag.data[2..]).unwrap();
            assert_eq!(pcm.len(), 2);
            for (ch, p) in pcm.into_iter().enumerate() {
                assert_eq!(p.len(), 1024);
                decoded[ch].extend(p);
            }
            wire_dts.push((i128::from(audio_times[ai].1) + 208 + shift, true));
            ai += 1;
        }
    }
    assert!(
        wire_dts
            .windows(2)
            .all(|w| w[0].0 < w[1].0 || (w[0].0 == w[1].0 && (!w[0].1 || w[1].1))),
        "actual AU wire order must be globally nondecreasing with video-first exact ties"
    );
    pictures.extend(h264.flush());
    assert_eq!(pictures.len(), 12);
    for p in &pictures {
        assert_eq!((p.width, p.height), (64, 48));
        assert!(p.y[24 * p.y_stride + 32].abs_diff(63) <= 8);
        assert!(p.u[12 * p.uv_stride + 16].abs_diff(102) <= 8);
        assert!(p.v[12 * p.uv_stride + 16].abs_diff(240) <= 8);
    }
    let mut gains = vec![];
    let mut snr = vec![];
    for (ch, pcm) in decoded.iter().enumerate() {
        assert_eq!(pcm.len(), 48128);
        let (mut power, mut error, mut dot) = (0., 0., 0.);
        for (i, value) in pcm.iter().enumerate().take(total - 1024).skip(8192) {
            let r = reference(i + 1, ch);
            let v = f64::from(*value);
            power += r * r;
            error += (v - r) * (v - r);
            dot += v * r;
        }
        let gain = dot / power;
        let db = 10. * (power / error).log10();
        assert!((0.97..=1.03).contains(&gain), "channel{ch} gain{gain}");
        assert!(db > 35., "channel{ch} SNR{db}");
        gains.push(gain);
        snr.push(db);
    }
    let receipt = serde_json::json!({"success":true,"scope":"actual Windows native AAC/H264 plus localhost RTMP; synthetic Program only","video_frames":vi,"audio_packets":ai,"real_audio_frames":total,"decoded_audio_frames":decoded[0].len(),"explicit_padding_frames":status.native_aac.explicit_padding_frames,"origin_ns":1,"first_audio_absolute_sample":1,"audio_offset_100ns":208,"common_shift_100ns":shift,"gain":gains,"snr_db":snr,"actual_owners_closed":owner.is_closed(),"raw_native_times":*observations,"physical_devices":false,"gui_poll_during_media":false,"upper_mux":"strict_peer_100ns_watermark_with_real_video_EOF","wire_dts_100ns":wire_dts});
    if let Some(directory) = evidence {
        directory
            .header("config.avcc", video_config.avcc())
            .unwrap();
        directory
            .header("config.asc", &audio_config.data[2..])
            .unwrap();
        let mut flv = directory.create("stream.flv").unwrap();
        flv.write_all(&[b'F', b'L', b'V', 1, 5, 0, 0, 0, 9, 0, 0, 0, 0])
            .unwrap();
        for tag in &tags {
            let n = tag.data.len();
            let ts = tag.timestamp;
            let header = [
                tag.type_id,
                (n >> 16) as u8,
                (n >> 8) as u8,
                n as u8,
                (ts >> 16) as u8,
                (ts >> 8) as u8,
                ts as u8,
                (ts >> 24) as u8,
                0,
                0,
                0,
            ];
            flv.write_all(&header).unwrap();
            flv.write_all(&tag.data).unwrap();
            flv.write_all(&((n + 11) as u32).to_be_bytes()).unwrap();
        }
        flv.finish().unwrap();
        directory.success(&receipt).unwrap();
    }
    println!("NATIVE_STREAMING_COMPOSITION {receipt}");
}
