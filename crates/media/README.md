# Media boundary

Owns pure Rust CPU composition, rational frame timing, bounded decoded-frame
admission and a nonallocating interleaved floating-point audio mixer. No UI,
devices, filesystem, codecs, networking or native plugin dependencies. Worker
sample-rate conversion uses pinned pure Rust Rubato with FFT/log features off.

`RgbaFrame` validates nonzero dimensions and tightly packed straight-alpha RGBA8
buffers, capped at 8192 pixels per edge and 256 MiB per frame. Incoming vectors
are normalized to boxed slices so spare allocation capacity cannot bypass frame
queue byte accounting. `composite` validates all layers and cumulative
input/output/work budgets before allocation; layers are
ordered bottom to top. Placement is axis-aligned, clipped to the canvas and uses
nearest-neighbor scaling. Color channels blend in encoded RGBA8 space; this is
explicitly not an HDR/color-managed OBS renderer. Opacity is finite in `[0,1]`.

`composite_affine` adds rotation, reflection and cropped normalized UV sampling
without changing the existing rectangle compositor. `Affine2` maps unit local
coordinates to destination pixels; `UvRect` maps those local coordinates into a
positive-area rectangle inside source UV `[0,1]`. The implementation inverse maps
destination pixel centers and uses nearest-neighbor sampling of half-open local
coordinates `[0,1)`. It validates finite corners and a representable nonsingular
inverse, including negative determinants, with no arbitrary determinant epsilon
or minimum visible size. Tiny geometry may cover no pixel center. Every clipped
conservative destination bounding-box pixel counts against the work budget,
including pixels outside rotated geometry and zero-opacity layers. Input bytes,
layer count, canvas bytes and background fill also count before output allocation.
Bounds are clipped as floating-point values before integer conversion so distant
or huge placements cannot create unbounded destination loops. There is no GPU,
filtering, perspective, antialiasing or color-management claim.

Image providers use `AffineLayer::from_texels` with an exact `TexelRect` for
native pixel crops. Its nonzero integer window must fit the source allocation;
offset+extent validation widens before addition. Sampling adds an integer origin
to a clamped in-window texel offset, avoiding rational UV rounding into cropped
pixels at either edge. `AffineLayer::new` retains arbitrary normalized UV support.
Normal affine coverage divides scaled displacements directly before solving;
it does not multiply a rounded reciprocal that could turn an excluded exact
right edge (`49/49`) into an included value just below one. Extreme mixed-axis
fallback behavior remains explicit and does not add a determinant epsilon.

`AffineLayer::with_blend_mode` selects an SDR source policy without changing
decoded RGBA bytes. `SourceBlend::Encoded` remains the default for all existing
constructors and the generic rectangle compositor. `LinearSrgb` decodes source
RGB before alpha, matching OBS Color. `EncodedPremultipliedSrgb` rounds encoded
RGB times the original alpha into bytes before decoding, matching OBS Image
with `linear_alpha=false`. Both then apply layer opacity and blend against the
linearized destination; the output stores straight encoded RGB and rounded
alpha. Thus white alpha 128 over black is 188 for Color and 128 for Image; over
gray 128 it is 205 and 154. Byte premultiplication uses `(c*a+127)/255`, checked
against all 65536 byte pairs of OBS's f32 helper. Fully opaque sources copy exact
bytes and zero alpha preserves the destination. No extra pixel buffers or work
admission exemptions are introduced. These are the supported SDR policies;
HDR, other color spaces and GPU/device behavior remain outside this primitive.

`directional_rgba` implements SDR Swipe/Slide with independent typed media
directions. It follows OBS's original f32 cubic easing and shader texture-role
selection, including Swipe In's reversed offset and swapped textures. Integer
time ratios round directly to f32 before easing. Pixel-center UVs select with
inclusive [0,1] bounds; normalized bilinear sampling clamps all four taps and
filters in linear sRGB. Opaque scenes and transparent-black null frames match
the supported shader path. For general straight-alpha inputs, interpolation
premultiplies in linear light and converts back to straight output, an explicit
adaptation of the GPU texture representation. Endpoint bytes remain exact.
Admission always counts two layers, unique input bytes, output bytes and five
conservative visits per pixel before allocation. The numeric
`directional_audio_gains` applies the same easing without an audio device.
Runtime ownership and unavailable endpoints belong to the host; see
[the module card](../../doc/migration/directional-transitions.md).

`fade_to_color_rgba` takes validated `FadeToColorSettings` (opaque RGB and a
switch percentage in 0..=100). It selects only A before the switch and only B
at/after it, then interpolates encoded RGB toward color with OBS's source-order
f32 smoothstep. It does not apply directional cubic easing or crossmix A/B.
The supported SDR callback disables framebuffer sRGB; its result is already
flattened over black in encoded space, with opaque output. Null-to-white at
half color weight is 128. General straight-alpha input adapts into rounded
encoded premultiplied texture bytes first. The host must not apply an additional
linear-light alpha gain to the returned frame. Both inputs, two layers and three
visits/pixel are admitted before allocation even at endpoints. The numeric
`fade_to_color_audio_gains` preserves independently faded source factors;
zero-width smoothstep uses an explicit finite right-continuous step in place
of upstream undefined 0/0. See [the module card](../../doc/migration/fade-to-color.md).

Frame timestamps are integer nanoseconds rounded down from an exact rational
frame rate using checked `u128` arithmetic. They do not identify a device clock or
variable-frame-rate decoding sequence. Queue freshness includes source, epoch
and settings generation; reset drains old buffers. Queue overload rejects the
incoming owned frame without changing queued frames. `close` drains and rejects
further input. The owning caller accounts for buffers outside this queue.

`AnimationClip` owns immutable same-canvas RGBA frames and inclusive duration
prefixes, with default admission of 512 frames and 128 MiB (each retained frame
counts conservatively, even shared Arcs). Positive integer durations have no
minimum clamp; GIF zero-delay normalization belongs to the decoder. Empty clips,
zero durations/cycle counts, mismatched canvases and duration-sum overflow reject.
`static_frame` wraps one already-admitted image without duplicating its pixels;
one-frame clips are static and request no ticks. Sampling uses integer division
and binary search, never loops proportional to elapsed time. OBS timing keeps
each frame at its exact end and changes one nanosecond later; finite counts are
total cycles, completing strictly after the final inclusive endpoint and freezing
the last frame. Future endpoints beyond `MediaTime(u64)` yield no wakeup rather
than wrapping. Finite lifetime multiplication uses u128. Sampling does no I/O,
pixel allocation or mutation, and returns one Arc plus the next relative delay.

`manual_torque_step` performs OBS's f32 torque formula for one supplied time
delta; the host owns bounded rational video-tick scheduling. `progress_rational`
preserves the exact f32 binary value as a reduced u64 fraction and rejects tiny
values whose exact denominator does not fit u64. It never silently quantizes.
`fade_single_on_black` applies the SDR sRGB transfer function around a linear
gain, multiplies source alpha in linear light, and returns opaque black-backed
pixels. White at half gain is 188, unlike the encoded-space two-scene fade's 128.
All endpoints still admit the input/output bytes, one layer and two pixel visits
before allocation. HDR, devices and runtime scheduling remain host concerns.

Audio uses supplied input/output slices, validates every unmuted sample and gain
before altering output, and allocates nothing in `mix_audio`. It sums linear
gains in f64, hard-clips once to `[-1,1]`, and reports the output peak and clipped
sample count. Input count, channels and sample count are bounded. Muted inputs
are ignored. This is a block processing primitive, not an instrumented realtime
device callback, resampler or loudness processor.

`AudioConverter` owns bounded worker-side rematrixing and bandlimited resampling,
with finite sealing, explicit counters/latency, exact logical frame counts and
preallocated processing buffers. A budgeted fractional-delay sinc stage corrects
the pinned resampler's starting phase before returned PCM is assigned to the
integer output grid. Equal-rate identity bypasses both filters. See
[the conversion contract](../../doc/migration/audio-conversion.md).

Verification: deterministic rectangle/affine pixel and sample oracles,
patterned-texture rotation/reflection/crop, inverse-mapped corner coverage,
invalid-input rollback,
rational timestamp boundaries, queue byte/count pressure, stale generations,
reset and close. Actual capture/audio/GPU/codec/OS behavior remains unverified.

`obs_audio` ports OBS built-in Gain and Invert Polarity with exact f32 sample
arithmetic, bounded preflight and no early clipping/allocation. Limiter remains
unimplemented pending processing-boundary fidelity; see
`doc/migration/obs-audio-filters.md` for provenance and acceptance.
