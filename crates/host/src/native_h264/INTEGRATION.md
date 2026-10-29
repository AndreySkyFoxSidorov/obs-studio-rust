# Staged native H.264 packet encoder

Not registered in a crate or manifest. No test/build/native activation has been
run by the implementing agent. Contract: `doc/migration/native-h264-packets.md`.

After the release freeze, root may copy this directory into host as
`native_h264/` and add `pub mod native_h264;` to host lib. Existing host Windows
`windows = 0.58.0` MediaFoundation/Com/Foundation features supply all referenced
interfaces/constants, including the explicit software encoder CLSID. No package
or feature addition is proposed. Existing host dev dependency `skylive-h264`
(actually already a production media-reader dependency) supplies the independent
decoder oracle. Validate that statement during integration; staged code has not
been compiled against the current feature profile yet.

Public flow: `Config::new` → `EncoderWorker::native` → nonblocking `poll(Ready)` →
`submit(Nv12Input)` / `poll(Packets)` sequentially → `finish` / drained reply →
`request_close` / `poll(Closed)`. `is_closed` follows actual worker teardown, not
the close request. `submit` returns rejected input ownership; consumers must
retire large rejected frames on an appropriate owner. No GUI joins or native
constructors are performed by `native()` itself. SDK construction occurs on its
dedicated worker. A native call may still block indefinitely: no timeout is
misrepresented as successful cancellation, and automatic replacement workers
are not created.

Ordinary tests use only injected pure Fake adapters. Explicit authorized native
test source is ignored and also requires `SKYLIVE_NATIVE_H264_ACCEPTANCE=1`.
It uses 64×48 synthetic NV12, no camera/microphone/file/network input, and checks
real H264 access units with the existing pure Rust decoder. Its expected GOP,
timestamps and color have not yet been established on an actual encoder run.

macOS has retained VideoToolbox audit source using narrow official C ABI declarations
and already cached CoreMedia/CoreVideo/Foundation types. Ready.configuration may
be None until its first actual encoded sample supplies checked SPS/PPS; every
nonempty batch requires Some. There is no dummy initial frame. Per-frame
CompleteFrames gives one bounded preallocated callback slot and owner-side
parsing. **It is not declared as a compiled module or selected by the factory.**
Failed-CompleteFrames refcon retirement lacks independently verified callback
quiescence; active counters alone cannot protect callbacks not yet entered.
The macOS and Linux factories explicitly report unavailable. See the card for
the proof/quarantine gate before enabling. This module adds no Streaming controls or
transport and does not change recording.

Native H264 owner creation reserves one of TWO process-global per-codec permits
before spawn. The worker holds it through blocked factory/native destruction;
Failed/request_close do not free it. Closed follows actual resource destruction
and permit release. Pure tests use injected local counters; targeted fixtures
share them to prove refusal while old SDK-like owners are still retiring.
