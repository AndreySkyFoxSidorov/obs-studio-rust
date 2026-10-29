# Staged RTMP publisher integration

Source13 update: this module is now integrated into the application. Windows
RTMPS uses the owned Schannel TLS1.2 adapter described in
`doc/migration/native-rtmps.md`. New source is unbuilt/unexecuted by user request;
the initial std-only staging notes and test instructions below are historical.

No Cargo.toml or workspace wiring is provided. Root can compile this module in an
isolated test harness with `#[path = "<absolute>/staging/rtmp/mod.rs"] mod rtmp;`
using Rust1.88/std only. Every test is currently source-only, not claimed passed.
The ignored `localhost_publish_roundtrip_and_optional_flv_receipt` is the sole
socket test and binds127.0.0.1. The implementation itself connects only after
explicit `Publisher::start(options)`.

1. Resolve the caller's RTMP authority off the GUI and construct `ResolvedTarget`.
   It retains exact app/tcUrl/stream/user/password strings and does no DNS.
2. Assign a checked nonzero, nonreused connection identity per output lifetime.
   ConnectOptions also carries separate native video/audio Generation triples.
   They must share session/output, while encoder generation IDs may differ.
3. Wait for the actual native AVC configuration (Mac Ready may have None).
   Convert its avcC directly through VideoConfig::new, never synthesize SPS/PPS.
   Convert actual native ASC through AudioConfig::new, never infer ADTS.
4. Convert each signed100ns native timestamp with checked multiplication by100.
   Preserve original metadata in receipts. Choose ONE explicit nonnegative shift
   for both tracks if native priming produces negative DTS; do not cast signed
   values to u64, zero tracks independently or guess/remove codec delay. The
   packet domain is shifted ns and timestamp_origin_ns is the shared epoch.
   Current native AVC is Baseline/noB; generic FLV composition offsets are signed.
5. Keep a small bounded interleaver upstream (not an unbounded transport spool).
   Enqueue globally nondecreasing DTS via try_packet, exact native track generation,
   first video IDR. Queue success is admission only. Publishing means server
   Publish.Start plus config writes; Progress counts bytes written, not delivery.
6. Consume key-tagged events only for current session/output/connection. stop()
   closes admission, drains queued packets, writes AVC EOS/unpublish/delete and
   closes the socket on the worker. Stopped acknowledges local completion and
   destruction, not a remote archive or decoder receipt. Dropping cancels/detaches;
   headless tests may join. Two global owner slots bound uncancellable connects;
   a slot survives until the actual worker exits. Stop before Publishing cancels
   setup and may report Cancelled; an OS connect remains bounded by its timeout.

Initial historical profile: RTMP only (no TLS/challenge authentication), Baseline66
avcC with4-byte NAL lengths, AVC AUs≤2MiB/128NALs, PPS≤32, AAC-LC44.1/48k mono/stereo
ASC≤64B and raw AAC AU≤64KiB (native encoder's2MiB limit is wider). Unsupported
profiles fail explicitly. No codec decoder, H264/AAC parser implementation, native
encoder, UI, scene graph, file tail or persistence is imported into the transport.

Optional local receipt: set SKYLIVE_RTMP_RECEIPT to a NEW writable file before the
ignored test. Default tiny NAL/AAC bytes exercise framing only and are NOT decoder
fixtures. For actual independent decode supply SKYLIVE_RTMP_TEST_FIXTURES pointing
to a directory with checked synthetic `video.avcc`, `video.avcc-au`, `audio.asc`,
`audio.raw-au`. The test bounds every read and writes an FLV receipt create_new;
root can then run ffprobe and decoded pixel/PCM oracles separately. No child test,
network operation or encoder execution was performed while adding these sources.
