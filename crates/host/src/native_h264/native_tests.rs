//! Explicit native acceptance; optional bounded packet evidence uses new owned paths only.
use super::*;
use crate::native_packet_evidence::{BoundedFile, Directory};
use std::io::Write;
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
            "native encode reply deadline; call not claimed stopped"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn annex_b(configuration: &CodecConfiguration, packet: &AccessUnit) -> Vec<u8> {
    let mut bytes = Vec::new();
    if packet.keyframe() {
        for nal in configuration.sps().iter().chain(configuration.pps()) {
            bytes.extend_from_slice(&[0, 0, 0, 1]);
            bytes.extend_from_slice(nal);
        }
    }
    let mut offset = 0;
    while offset < packet.bytes().len() {
        let length =
            u32::from_be_bytes(packet.bytes()[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(&packet.bytes()[offset..offset + length]);
        offset += length;
    }
    bytes
}
type ConfigurationEvidence = (Box<[u8]>, Box<[u8]>, bool);
struct Evidence {
    directory: Directory,
    metadata: BoundedFile,
    annex: BoundedFile,
    configurations: Vec<ConfigurationEvidence>,
    packets: usize,
}
impl Evidence {
    fn new(directory: Directory) -> Self {
        let metadata = directory.create("packets.jsonl").unwrap();
        let annex = directory.create("stream.h264").unwrap();
        Self {
            directory,
            metadata,
            annex,
            configurations: vec![],
            packets: 0,
        }
    }
    fn packet(&mut self, configuration: &CodecConfiguration, packet: &AccessUnit, annex: &[u8]) {
        let index = if let Some(index) =
            self.configurations
                .iter()
                .position(|(output, input, rewritten)| {
                    output.as_ref() == configuration.avcc()
                        && input.as_ref() == configuration.input_avcc()
                        && *rewritten == configuration.color_metadata_rewritten()
                }) {
            index
        } else {
            let index = self.configurations.len();
            let filename = if index == 0 {
                "config.avcc".into()
            } else {
                format!("config-{index:03}.avcc")
            };
            self.directory
                .header(&filename, configuration.avcc())
                .unwrap();
            let input_filename = if index == 0 {
                "input-config.avcc".into()
            } else {
                format!("input-config-{index:03}.avcc")
            };
            self.directory
                .header(&input_filename, configuration.input_avcc())
                .unwrap();
            self.configurations.push((
                configuration.avcc().into(),
                configuration.input_avcc().into(),
                configuration.color_metadata_rewritten(),
            ));
            index
        };
        let configuration_file = if index == 0 {
            "config.avcc".into()
        } else {
            format!("config-{index:03}.avcc")
        };
        let filename = format!("au-{:03}.avcc", self.packets);
        let input_configuration_file = if index == 0 {
            "input-config.avcc".into()
        } else {
            format!("input-config-{index:03}.avcc")
        };
        self.directory.packet(&filename, packet.bytes()).unwrap();
        self.annex.write_all(annex).unwrap();
        let generation = packet.generation();
        self.metadata.json_line(&serde_json::json!({"file":filename,"configuration":configuration_file,"input_configuration":input_configuration_file,"color_metadata_rewritten":configuration.color_metadata_rewritten(),"color_metadata_policy":"BT709 limited from SkyLive input contract","bytes":packet.bytes().len(),"pts":packet.pts(),"dts":packet.dts(),"duration":packet.duration(),"timebase":"100ns","keyframe":packet.keyframe(),"generation":{"session":generation.session,"output":generation.output,"encoder":generation.encoder}})).unwrap();
        self.packets += 1;
    }
    fn finish(self, receipt: &serde_json::Value) {
        self.metadata.finish().unwrap();
        self.annex.finish().unwrap();
        self.directory.success(receipt).unwrap();
    }
}
fn decode_batch(
    decoder: &mut skylive_h264::Decoder,
    batch: PacketBatch,
    pictures: &mut Vec<skylive_h264::Picture>,
    times: &mut Vec<(i64, i64)>,
    keys: &mut usize,
    evidence: &mut Option<Evidence>,
    color_rewritten: &mut bool,
) {
    if batch.packets.is_empty() {
        return;
    }
    let configuration = batch
        .configuration
        .as_ref()
        .expect("nonempty packet config");
    assert!(!configuration.avcc().is_empty());
    *color_rewritten |= configuration.color_metadata_rewritten();
    for packet in batch.packets {
        assert_eq!(packet.generation(), batch.generation);
        assert_eq!(packet.pts(), packet.dts());
        *keys += usize::from(packet.keyframe());
        times.push((packet.pts(), packet.duration()));
        let bytes = annex_b(configuration, &packet);
        if let Some(evidence) = evidence {
            evidence.packet(configuration, &packet, &bytes);
        }
        pictures.extend(decoder.decode(&bytes, packet.pts()).unwrap());
    }
}
#[test]
#[ignore = "explicit opt-in native H264 packet encoder acceptance; no devices/network/GUI"]
fn native_h264_packets_decode_count_color_timestamps_and_eos() {
    assert_eq!(
        std::env::var("SKYLIVE_NATIVE_H264_ACCEPTANCE").as_deref(),
        Ok("1"),
        "set explicit native acceptance opt-in"
    );
    let config = Config::new(
        Generation {
            session: 1,
            output: 1,
            encoder: 1,
        },
        64,
        48,
        30,
        1,
        1_000_000,
        6,
    )
    .unwrap();
    let evidence_directory = Directory::from_env("SKYLIVE_NATIVE_H264_EVIDENCE_DIR").unwrap();
    if let Some(directory) = &evidence_directory {
        directory.json("run.json",&serde_json::json!({"codec":"h264","width":config.width(),"height":config.height(),"fps":config.fps(),"bitrate":config.bitrate(),"gop":config.gop(),"frames":12,"input":"synthetic NV12 BT709 limited","network":false,"physical_input":false})).unwrap();
    }
    let evidence_path = evidence_directory
        .as_ref()
        .map(|directory| directory.path().to_owned());
    let mut evidence = evidence_directory.map(Evidence::new);
    let mut worker = EncoderWorker::native(config.clone()).unwrap();
    assert!(matches!(next(&mut worker), Event::Ready { .. }));
    let mut decoder = skylive_h264::Decoder::new();
    let mut pictures = Vec::new();
    let mut times = Vec::new();
    let mut keys = 0;
    let mut color_rewritten = false;
    for index in 0..12 {
        let mut nv12 = vec![64 + index as u8; config.nv12_bytes()];
        for uv in nv12[64 * 48..].chunks_exact_mut(2) {
            uv[0] = 102;
            uv[1] = 240;
        }
        let (pts, duration) = config.frame_timing(index).unwrap();
        worker
            .submit(Nv12Input::new(&config, pts, duration, nv12.into_boxed_slice()).unwrap())
            .unwrap();
        match next(&mut worker) {
            Event::Packets(batch) => decode_batch(
                &mut decoder,
                batch,
                &mut pictures,
                &mut times,
                &mut keys,
                &mut evidence,
                &mut color_rewritten,
            ),
            other => panic!("{other:?}"),
        }
    }
    worker.finish().unwrap();
    match next(&mut worker) {
        Event::Packets(batch) => {
            assert!(batch.drained);
            decode_batch(
                &mut decoder,
                batch,
                &mut pictures,
                &mut times,
                &mut keys,
                &mut evidence,
                &mut color_rewritten,
            );
        }
        other => panic!("{other:?}"),
    }
    pictures.extend(decoder.flush());
    assert_eq!(pictures.len(), 12);
    assert_eq!(times.len(), 12);
    assert!(keys >= 2, "explicit six-frame GOP");
    for (index, picture) in pictures.iter().enumerate() {
        assert_eq!((picture.width, picture.height), (64, 48));
        assert_eq!(
            (
                picture.color.full_range,
                picture.color.primaries,
                picture.color.transfer,
                picture.color.matrix
            ),
            (false, 1, 1, 1)
        );
        assert_eq!(picture.pts, config.frame_timing(index as u64).unwrap().0);
        assert_eq!(times[index], config.frame_timing(index as u64).unwrap());
        let y = picture.y[24 * picture.y_stride + 32];
        let u = picture.u[12 * picture.uv_stride + 16];
        let v = picture.v[12 * picture.uv_stride + 16];
        assert!(y.abs_diff(64 + index as u8) <= 8, "decoded luma {y}");
        assert!(
            u.abs_diff(102) <= 8 && v.abs_diff(240) <= 8,
            "decoded chroma {u}/{v}"
        );
    }
    worker.request_close();
    assert!(matches!(next(&mut worker), Event::Closed { .. }));
    assert!(worker.is_closed());
    if let Some(evidence) = &evidence {
        assert_eq!(evidence.packets, 12);
    }
    let receipt = serde_json::json!({"success":true,"codec":"h264","width":64,"height":48,"fps":[30,1],"bitrate":1000000,"gop":6,"packets":times.len(),"decoded_frames":pictures.len(),"keyframes":keys,"native_pts_duration":times,"actual_worker_closed":worker.is_closed(),"evidence_directory":evidence_path,"evidence_counters_before_receipt":evidence.as_ref().map(|capture|capture.directory.counters_before_receipt()),"packet_framing":"four-byte big-endian NAL lengths","input_configuration":"input-config.avcc is actual input SPS/PPS normalized to avcC before color signaling","color_metadata_rewritten":color_rewritten,"color_metadata_policy":"BT709 limited from SkyLive input contract; not an original native metadata claim","decoded_color":{"range":"limited","primaries":1,"transfer":1,"matrix":1},"annex_b_file":"stream.h264","independent_decoder":"existing pure Rust H264","physical_input":false,"network":false,"streaming_ui_exercised":false});
    if let Some(evidence) = evidence {
        evidence.finish(&receipt);
    }
    println!("NATIVE_H264_ACCEPTANCE {receipt}");
}
