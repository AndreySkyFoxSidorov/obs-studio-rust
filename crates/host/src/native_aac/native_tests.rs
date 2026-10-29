//! Explicit opt-in native encoding; optional bounded raw-packet evidence only.
use super::*;
use crate::native_packet_evidence::Directory;
use std::{
    thread,
    time::{Duration, Instant},
};
fn next(worker: &mut EncoderWorker) -> Event {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(event) = worker.poll() {
            return event;
        }
        assert!(
            Instant::now() < deadline,
            "native AAC worker reply deadline; shutdown was not acknowledged"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn close(worker: &mut EncoderWorker) {
    worker.request_close();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker.is_closed() {
        assert!(
            Instant::now() < deadline,
            "native AAC retirement deadline; not reported as stopped"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn reference(frame: usize, channel: usize) -> f64 {
    let time = frame as f64 / 48_000.0;
    let (carrier, envelope, phase, level) = if channel == 0 {
        (1337.0, 17.0, 0.31, 0.18)
    } else {
        (2633.0, 23.0, -0.47, 0.11)
    };
    level
        * (1.0 + 0.2 * (std::f64::consts::TAU * envelope * time).sin())
        * (std::f64::consts::TAU * carrier * time + phase).sin()
}
fn measure(reference: &[f64], decoded: &[f32]) -> (usize, f64, f64) {
    let mut best = (f64::INFINITY, 0usize);
    // The slow17/23Hz envelopes distinguish absolute codec delay from carrier
    // phase aliases. Search is fixed<=4097*8192 multiply-adds per channel.
    for lag in 0..=4096 {
        if decoded.len() < lag + 10_240 {
            break;
        }
        let error = (2048..10_240)
            .map(|index| {
                let delta = f64::from(decoded[index + lag]) - reference[index];
                delta * delta
            })
            .sum::<f64>();
        if error < best.0 {
            best = (error, lag);
        }
    }
    let lag = best.1;
    let end = (reference.len() - 1024).min(decoded.len().saturating_sub(lag + 1024));
    assert!(end > 16_384);
    let mut signal = 0.0;
    let mut error = 0.0;
    let mut dot = 0.0;
    for index in 8192..end {
        let expected = reference[index];
        let actual = f64::from(decoded[index + lag]);
        signal += expected * expected;
        dot += actual * expected;
        error += (actual - expected) * (actual - expected);
    }
    (lag, dot / signal, 10.0 * (signal / error).log10())
}
#[test]
#[ignore = "requires explicit Windows native AAC packet acceptance"]
fn native_windows_raw_aac_packets_decode_stereo_phase_count_timing_and_eos() {
    assert_eq!(
        std::env::var("SKYLIVE_NATIVE_AAC_ACCEPTANCE").as_deref(),
        Ok("1"),
        "set explicit acceptance opt-in"
    );
    let evidence_root = Directory::from_env("SKYLIVE_NATIVE_AAC_EVIDENCE_DIR").unwrap();
    if let Some(directory) = &evidence_root {
        directory.json("run.json",&serde_json::json!({"codec":"aac_lc","sample_rate":48000,"input_frames_per_profile":48123,"profiles":[{"name":"mono","channels":1,"bitrate":96000},{"name":"stereo","channels":2,"bitrate":128000}],"input":"synthetic amplitude-modulated1337/2633Hz carriers","physical_input":false,"network":false})).unwrap();
    }
    let mut profile_receipts = Vec::new();
    for channels in [1u16, 2] {
        let config = Config::new(
            Generation {
                session: 71,
                output: 72,
                encoder: u64::from(channels),
            },
            channels,
            if channels == 1 { 96_000 } else { 128_000 },
        )
        .unwrap();
        let total = 48_123usize;
        let originals: Vec<Vec<f64>> = (0..usize::from(channels))
            .map(|channel| (0..total).map(|frame| reference(frame, channel)).collect())
            .collect();
        let mut worker = EncoderWorker::native(config.clone()).unwrap();
        let Event::Ready {
            generation,
            configuration,
        } = next(&mut worker)
        else {
            panic!("native AAC Ready")
        };
        assert_eq!(generation, config.generation());
        let profile_directory = evidence_root.as_ref().map(|directory| {
            directory
                .child(if channels == 1 { "mono" } else { "stereo" })
                .unwrap()
        });
        if let Some(directory) = &profile_directory {
            directory.header("config.asc", configuration.asc()).unwrap();
        }
        let mut packet_log = profile_directory
            .as_ref()
            .map(|directory| directory.create("packets.jsonl").unwrap());
        let mut decoder = skylive_aac::Decoder::new(configuration.asc()).unwrap();
        let mut decoded = vec![Vec::<f32>::new(); usize::from(channels)];
        let mut count = 0u64;
        let mut last_pts = None;
        let mut pts_observations = Vec::new();
        let mut consume = |batch: PacketBatch| {
            assert_eq!(batch.generation, config.generation());
            assert_eq!(batch.configuration, configuration);
            for packet in &batch.packets {
                assert_eq!(packet.generation(), config.generation());
                assert_eq!(packet.sequence(), count);
                assert_eq!(packet.pts(), packet.dts());
                assert!(packet.duration() > 0);
                assert!(packet.pts().checked_add(packet.duration()).is_some());
                assert!(last_pts.is_none_or(|previous| packet.pts() > previous));
                last_pts = Some(packet.pts());
                pts_observations.push((packet.sequence(), packet.pts(), packet.duration()));
                let filename = format!("au-{count:03}.aac");
                if let Some(directory) = &profile_directory {
                    directory.packet(&filename, packet.bytes()).unwrap();
                }
                if let Some(metadata) = &mut packet_log {
                    let generation = packet.generation();
                    metadata.json_line(&serde_json::json!({"file":filename,"sequence":packet.sequence(),"configuration":"config.asc","bytes":packet.bytes().len(),"pts":packet.pts(),"dts":packet.dts(),"duration":packet.duration(),"timebase":"100ns","coded_frames":1024,"generation":{"session":generation.session,"output":generation.output,"encoder":generation.encoder}})).unwrap();
                }
                let pcm = decoder.decode(packet.bytes()).unwrap();
                assert_eq!(pcm.len(), usize::from(channels));
                for (channel, samples) in pcm.into_iter().enumerate() {
                    assert_eq!(samples.len(), 1024);
                    assert!(samples.iter().all(|sample| sample.is_finite()));
                    decoded[channel].extend(samples);
                }
                count += 1;
            }
            batch.stats
        };
        let mut first = 0usize;
        // Odd packetization independently tests residual assembly, rather than
        // assuming capture/UI packet boundaries equal AAC codec frame boundaries.
        while first < total {
            let frames = (if first == 0 { 17 } else { 997 }).min(total - first);
            let samples: Vec<f32> = (first..first + frames)
                .flat_map(|frame| {
                    (0..usize::from(channels)).map(move |channel| reference(frame, channel) as f32)
                })
                .collect();
            let pcm = pcm16_interleaved(&samples, channels).unwrap();
            worker
                .submit(PcmInput::new(&config, first as u64, pcm).unwrap())
                .unwrap();
            let event = next(&mut worker);
            let Event::Packets(batch) = event else {
                panic!("native AAC packets at input frame {first}: {event:?}")
            };
            assert!(!batch.drained);
            consume(batch);
            first += frames;
        }
        worker.finish().unwrap();
        let event = next(&mut worker);
        let Event::Packets(batch) = event else {
            panic!("native AAC EOS: {event:?}")
        };
        assert!(batch.drained);
        let stats = consume(batch);
        assert_eq!(stats.input_frames, total as u64);
        assert_eq!(stats.explicit_padding_frames, 5);
        assert_eq!(stats.submitted_frames, 48_128);
        assert_eq!(stats.packets, count);
        assert!(
            decoded[0].len() >= total && decoded[0].len() <= total + 3 * 1024,
            "untrimmed coded sample count"
        );
        let measurements: Vec<_> = originals
            .iter()
            .zip(&decoded)
            .map(|(expected, actual)| measure(expected, actual))
            .collect();
        for &(lag, gain, snr) in &measurements {
            assert!(
                (0.95..=1.05).contains(&gain),
                "measured native gain {gain},lag{lag},SNR{snr}"
            );
            assert!(snr > 28.0, "native packet tone/phase SNR{snr}dB,lag{lag}");
        }
        if channels == 2 {
            assert_eq!(
                measurements[0].0, measurements[1].0,
                "native channel priming must agree"
            );
        }
        close(&mut worker);
        assert!(worker.is_closed());
        if let Some(metadata) = packet_log {
            metadata.finish().unwrap();
        }
        let receipt = serde_json::json!({"success":true,"codec":"aac_lc","channels":channels,"sample_rate":48000,"bitrate":config.bitrate(),"asc":configuration.asc(),"input_frames":stats.input_frames,"submitted_frames":stats.submitted_frames,"explicit_padding_frames":stats.explicit_padding_frames,"packets":stats.packets,"decoded_untrimmed_frames":decoded[0].len(),"measured_codec_priming_frames":measurements[0].0,"measured_coded_tail_after_real_input":decoded[0].len()as i128-total as i128-measurements[0].0 as i128,"channel_gain_snr":measurements,"native_pts_duration":pts_observations,"actual_worker_closed":worker.is_closed(),"evidence_directory":profile_directory.as_ref().map(|directory|directory.path()),"evidence_counters_before_receipt":profile_directory.as_ref().map(Directory::counters_before_receipt),"independent_decoder":"existing pure Rust AAC","packet_framing":"raw AAC AU without ADTS","physical_input":false,"sound_playback":false,"network":false,"gapless_metadata_claim":false});
        if let Some(directory) = profile_directory {
            directory.success(&receipt).unwrap();
        }
        println!("NATIVE_AAC_ACCEPTANCE {receipt}");
        profile_receipts.push(receipt);
    }
    if let Some(directory) = evidence_root {
        directory.success(&serde_json::json!({"success":true,"profiles":profile_receipts,"evidence_counters_before_receipt":directory.counters_before_receipt(),"all_profile_decoders_passed":true,"all_workers_closed":true})).unwrap();
    }
}
