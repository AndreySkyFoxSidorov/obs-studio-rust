# Staged native AAC packet encoder

Not registered in a Cargo member or manifest. No build/test/native activation
has been run by the implementing agent. Contract is
`doc/migration/native-aac-packets.md`.

After Release5 root may copy this directory as host/native_aac and declare it.
Existing windows0.58 MediaFoundation/Com/Foundation features expose
AACMFTEncoder, IMFTransform and MF_MT_USER_DATA; no package/feature change is
proposed. Production uses only typed Windows bindings and safe owned PCM/header
logic outside the native pointer copies. Non-Windows native factory returns an
explicit Unavailable event. No fake AudioConverter/FFmpeg fallback exists.

Public flow: Config::new(Generation,channels,bitrate) → EncoderWorker::native →
poll Ready → submit PcmInput / poll Packets, one command including unread reply
→ finish / drained Packets → request_close / actual Closed. Config is exactly
48kHz signed16 input1/2channels. Use existing AudioConverter on the Program owner
for other project formats; pcm16_interleaved performs bounded finite checking,
explicit clip/round/saturate ONLY on a worker. Its constructor does not resample.
Input packet size is1..1024frames; first_frame is contiguous from0. New encoder
or discontinuity requires a new nonzero encoder/output generation. The generation is a streaming-operation
identity, not the scene revision: ordinary scene/fader edits do not rebuild or
restart a codec. The integration owner explicitly maps the Program anchor to
first_frame0 and aligns the video operation origin; no sample index is silently
reinterpreted as a nanosecond timestamp.

ASC is always available at AAC Ready, unlike H264 VideoToolbox's deferred header.
Packets are raw AAC-LC, never ADTS, and preserve signed native100ns timing.
Input Config::sample_timing is exact floored absolute48kHz-grid boundaries.
Zero-padding of a final residual is explicitly counted; no guessed AAC priming
trim or gapless metadata is emitted. StreamStats input_frames vs submitted_frames
and padding are separate from packets/native timing. Returned packet buffers are
consumer-owned and require their own bounded queue/retirement policy.

Ordinary tests inject a !Send Fake only to test protocol/ownership. The ignored
Windows test requires SKYLIVE_NATIVE_AAC_ACCEPTANCE=1, synthesizes PCM with two
distinct carriers/envelopes, produces real MFT AUs and decodes with existing pure
Rust AAC. It prints bounded metadata/phase/count measurements, not PCM. It does
not open devices, play sound, create audio files or use the network. Its pass has
not been established until root explicitly runs it on Windows.

The worker facade belongs to the streaming/audio owner, not a GUI frame. Native
state, PCM loops and packet queues stay there; UI receives only a bounded status
projection. Large packet/rejected-input buffer retirement is that owner's duty.
request_close is cancellation, is_closed is actual native teardown. Blocking SDK
calls cannot be canceled by a timer and must not cause unbounded replacement
threads. One failed reply seals frontend admission even while SDK cleanup runs.

Idle workers block on their command channel. Close sends one bounded wake command
and sets the cancellation flag; queued work is rejected before the next SDK
entry. No idle20ms polling timer or GUI join is needed.

Native entry reserves from a process-global2-owner AAC counter BEFORE spawn. The
permit remains with the constructing thread across blocked constructors/failure/
cleanup; Closed is published only after actual native teardown and permit drop.
Private injected factories use local counters to keep parallel pure tests stable.
Per-codec staged counters are not a combined H264+AAC process budget; root decides
that assembly policy explicitly during integration.
