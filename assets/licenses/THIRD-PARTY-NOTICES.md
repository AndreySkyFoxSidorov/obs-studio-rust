# Bundled Craft modules and text fonts

SkyLive is distributed under GPL-2.0-or-later. Paths under `doc/` in historical provenance notes refer to local research material that is not included in the public repository; upstream sources can be obtained from their respective projects. This notice records the selected source transfers and fonts added to this build; Cargo dependency licenses remain available in the corresponding dependency sources.

- OBS Color Correction v2 derives its matrix and shader equations from OBS Studio's `plugins/obs-filters/color-correction-filter.c` and `data/color_correction_filter.effect`, copyright (C) 2016 c3r1c3, under GPL-2.0-or-later. The implementation is a Rust SDR CPU adaptation, not the legacy v1 filter or a binary OBS plugin. The pinned source revision/hashes and supported behavior are recorded in `doc/migration/color-correction-provenance.json` and `color-correction.md` in local research material. SkyLive's included GPL license applies to this derived implementation.

- OBS Gain and Invert Polarity implement the arithmetic from OBS Studio's `plugins/obs-filters/gain-filter.c`, `invert-audio-polarity.c` and `libobs/media-io/audio-math.h` under GPL-2.0-or-later. Source hashes, original notices and the interleaved Rust adaptation are recorded in `doc/migration/obs-audio-filters.md`; the supplied OBS checkout retains the original file notices. These stateless audio filters use no libobs binary dependency. SkyLive's included GPL license applies to this derived implementation.
- OBS Limiter, Compressor and Expander adapt OBS Studio contributors' `plugins/obs-filters/limiter-filter.c`, `compressor-filter.c`, `expander-filter.c` and `libobs/media-io/audio-math.h` under GPL-2.0-or-later. The pinned source revision/hashes, Rust changes and callback-boundary contract are recorded in `doc/migration/obs-dynamics-provenance.json` and `obs-dynamics.md`. These are Rust DSP implementations with no production C or libobs dependency; the independent C harness is test material only. External sidechain and Upward Compressor are now separately implemented in Rust; their source scope is recorded in the corresponding migration documents. SkyLive's included GPL license applies to these derived implementations.
- Craft Audio adapts FilmCraft compressor, gate, three-band equalizer and smoothing code, copyright 2026 ArtCraft Team and the FilmCraft contributors, under the MIT License. See `craft/filmcraft-MIT.txt`, `craft/filmcraft-NOTICE.txt` and `craft/filmcraft-source.json` for the complete notice, source revision, hashes and modifications.
- Craft Video adapts EffectCraft keying, brightness/contrast and spill-suppression kernels, copyright 2026 ArtCraft Team and the EffectCraft contributors, under the MIT License. See `craft/effectcraft-MIT.txt` and `craft/references.json`. This is a selected set of CPU effects, not the complete EffectCraft application.
- PhotoCraft blending sources were reviewed as references. No PhotoCraft blending kernel is connected in this build. Its retained reference license does not imply a working module.
- Text sources embed six unmodified Noto Sans family fonts; the UI also embeds a Noto Sans Symbols fallback (seven distinct font files total) under SIL Open Font License 1.1. Their complete licenses and pinned file hashes are in `assets/fonts/`. No system font scan is required.

The Craft modules are available by default and can be disabled in Tools > Plugins. They do not automatically add filters to existing sources. No ArtCraft trademarks, presets or application UI assets are transferred.

- OBS Color Key v2 and Chroma Key v2 adapt the SDR coefficient and shader equations from OBS Studio contributors under GPL-2.0-or-later. The source checkout retains its original notices; pinned files and hashes are in `doc/migration/obs-keyer-provenance.json`. Rust uses a bounded three-row CPU neighborhood and no OBS binary. Legacy v1/HDR and GPU byte-exact equivalence are not claimed.

- AI Background Removal (Person) uses the fixed Selfie Segmentation model under Apache-2.0, with exact Google/Katsuya Hyodo/Roy Shilkrot/Kaito Udagawa notices in `crates/person-matte/models/selfie-segmentation.LICENSE` and `crates/person-matte/models/NOTICE`. The accompanying manifest pins the original ONNX and converted RTen graph hashes. The runtime is pure Rust RTen0.21.0 by Robert Knight and contributors (MIT OR Apache-2.0); `vendor/rten-0.21.0/` retains its terms and the one-line lazy thread-pool fallback patch. These optional third-party filters default off. This selected CPU Selfie port does not contain the C++ OBS plugin wrapper, ONNXRuntime, OpenCV or all upstream models/settings.

- OBS Crop/Pad adapts OBS Studio contributors' `plugins/obs-filters/crop-filter.c` and `data/crop_filter.effect` at tag32.2.2 (peeled commit ba2f32bdf791005443988a4955e963663e16b1ed), under GPL-2.0-or-later. It is a Rust SDR RGBA8 CPU implementation with signed transparent padding, relative/absolute dimensions and ordered downstream filtering; no OBS/libobs binary is used. See `doc/migration/obs-crop-pad.md` and its adjacent provenance for source hashes, dimensions, resource limits and unverified native acceptance. The included GPL license applies to this derived implementation.

- OBS source-item Bicubic and Lanczos reconstruction and the eight-bilinear low-resolution sample pattern adapt OBS Studio 32.2.2 `libobs/data/bicubic_scale.effect`, `lanczos_scale.effect`, `bilinear_lowres_scale.effect` and `obs-scene.c` under GPL-2.0-or-later. The original Bicubic/Lanczos shaders credit the GPL bsnes shader. Area adapts `libobs/data/area.effect` with a bounded affine pixel-footprint extension for rotation/reflection. These are Rust CPU SDR implementations, without a libobs/GPU binary dependency. Source hashes, the exact reference commit and adaptation limits are recorded in `doc/migration/source-scale-filtering.md` and adjacent provenance. The included GPL license applies to these derived implementations.

- Whole-Program output scaling selection follows OBS Studio 32.2.2 `libobs/obs-video.c`, under GPL-2.0-or-later, with the already adapted SDR Bicubic/Lanczos/Area/low-resolution kernels. Canvas/output separation and virtual-camera view routing are documented against `frontend/utility/BasicOutputHandler.cpp` and `libobs/obs-view.c`; no C++/libobs runtime backend is linked. The Rust implementation and explicit CPU resource profiles are documented in `doc/migration/output-resolution.md` with adjacent pinned-source provenance. The included GPL license applies to this derived implementation.

- Windows Display Capture/WGC settings and target/geometry behavior derive from
  OBS Studio 32.2.2 `plugins/win-capture/duplicator-monitor-capture.c`,
  `libobs-winrt/winrt-capture.cpp` and `libobs-d3d11/d3d11-duplicator.cpp`,
  OBS Studio contributors, GPL-2.0-or-later. Annotated tag object
  fc546137470c0f4a5f1493b6e7815545ee1b170b, peeled commit
  ba2f32bdf791005443988a4955e963663e16b1ed. The implementation uses Rust and
  Windows native APIs; no OBS/C++ binary is linked. DXGI cursor AND/XOR,
  masked-color and alpha composition implement Microsoft's documented pointer
  shape protocol; no Microsoft sample code is copied. See
  `doc/migration/display-capture-methods.md` for exact sources and limits.

- Browser Interaction's aspect-fit/native-coordinate, mouse leave/up-outside,
  wheel and focus/key routing adapt OBS Studio 32.2.2
  `frontend/dialogs/OBSBasicInteraction.cpp`, copyright(C)2023 Lain Bailey
  <lain@obsproject.com>, GPL-2.0-or-later. Exact peeled reference commit
  ba2f32bdf791005443988a4955e963663e16b1ed. The Rust frontend sends real bounded
  input to the separately deployed CEF helper; no OBS/Qt binary is linked.
  See `doc/migration/browser-interaction.md` for runtime and input scope.

- Source audio sync limits, source-specific timestamp placement/truncation, and
  Windows physical-monitor delay rules adapt OBS Studio 32.2.2
  `frontend/components/OBSAdvAudioCtrl.cpp`, `libobs/obs-source.c`, and
  `libobs/audio-monitoring/win32/wasapi-output.c`, copyright OBS Studio
  contributors, GPL-2.0-or-later. Exact annotated tag object
  fc546137470c0f4a5f1493b6e7815545ee1b170b and peeled commit
  ba2f32bdf791005443988a4955e963663e16b1ed. This is a Rust implementation;
  no OBS/C++ binary is linked. `doc/migration/audio-sync-offset.md` records the
  bounded project-grid processing and remaining post-DSP transient parity scope.

- Source-owned pre-gain processed audio history and timestamped output-volume
  ordering follow OBS Studio 32.2.2 libobs/obs-source.c process_audio,
  filter_async_audio, source_output_audio_data and apply_audio_actions,
  copyright OBS Studio contributors, GPL-2.0-or-later. Peeled reference
  ba2f32bdf791005443988a4955e963663e16b1ed. This is Rust, with the existing
  project DSP quantum contract; no OBS binary is linked.
  See doc/migration/audio-post-dsp-history.md for supported behavior and bounds.

- Apply LUT adapts OBS Studio 32.2.2 `plugins/obs-filters/color-grade-filter.c`,
  `data/color_grade_filter.effect`, `libobs/graphics/srgb.h` and `half.h`,
  copyright OBS Studio contributors and copyright (C) 2023 Lain Bailey,
  under GPL-2.0-or-later. Binary16 conversion follows `half.h`'s
  DirectXMath adaptation, copyright (c) 2011-2019 Microsoft Corp, MIT.
  Complete terms and exact pinned-source SHA256 receipts are in
  `obs-color-lut/`. This is a bounded Rust SDR CPU implementation plus a
  source-only wgpu24 LUT shader, without C/libobs binaries. Native GPU pixel
  parity and performance remain unexecuted. See `doc/migration/obs-color-lut.md`
  for supported formats, original alpha branches and strict exclusions.

- Scroll adapts OBS Studio 32.2.2 `plugins/obs-filters/scroll-filter.c`,
  `data/crop_filter.effect` and `libobs/obs-source.c` showing/tick ordering,
  copyright OBS Studio contributors, under GPL-2.0-or-later. Exact peeled
  reference commit and source SHA256 receipts plus complete terms are in
  `obs-scroll-filter/`. This is Rust CPU SDR sampling with an explicit-time,
  bounded host phase owner; no C/libobs/GPU Scroll runtime is linked.
  See `doc/migration/obs-scroll-filter.md` for timing and supported scope.

- OBS compressor sidechain/ducking follows OBS Studio 32.2.2
  plugins/obs-filters/compressor-filter.c and libobs/obs-source.c
  source_signal_audio_data, OBS Studio contributors, GPL-2.0-or-later.
  Peeled reference ba2f32bdf791005443988a4955e963663e16b1ed.
  Detector data is filtered, pre-sync/pre-fader and respects the signal mute
  flag. This is Rust; no OBS binary is linked. See
  doc/migration/obs-compressor-sidechain.md for bounded scope and packetization.

- Luma Wipe adapts OBS Studio 32.2.2 `plugins/obs-transitions/transition-luma-wipe.c`
  and `data/luma_wipe_transition.effect`, OBS Studio contributors,
  GPL-2.0-or-later. The original shader credits the Rendermix Wipe shader:
  https://github.com/rectalogic/rendermix-basic-effects/blob/master/assets/com/rendermix/Wipe/Wipe.frag
  All34 stock PNG masks and wipes.json are copied unmodified from the exact
  peeled OBS commit ba2f32bdf791005443988a4955e963663e16b1ed. The mask manifest,
  file SHA256 receipts and source path are in `assets/luma-wipes/provenance.json`;
  complete supplied GPL terms are in `obs-luma-wipe/`. This is a Rust SDR CPU
  transition implementation, with no C/libobs runtime. See
  `doc/migration/luma-wipe.md` for interpolation, audio and resource scope.

- Scaling/Aspect Ratio adapts OBS Studio 32.2.2 `plugins/obs-filters/scale-filter.c`,
  `libobs/data/bicubic_scale.effect`, `lanczos_scale.effect`, `area.effect` and
  `bilinear_lowres_scale.effect`, OBS Studio contributors, GPL-2.0-or-later.
  The original Bicubic/Lanczos shader credits remain applicable as recorded
  above for source-item sampling. Exact reference peeled commit
  ba2f32bdf791005443988a4955e963663e16b1ed. This Rust SDR CPU filter reuses the
  existing licensed reconstruction kernels with a separate source-filter policy
  and original per-tap center-undistortion mapping. No C/libobs binary is linked.
  See `doc/migration/obs-scale-filter.md` for alpha, dimensions and admission.

- Windowed/fullscreen projectors and Multiview adapt OBS Studio 32.2.2
  `frontend/widgets/OBSBasic_Projectors.cpp`, `OBSProjector.cpp`,
  `components/Multiview.cpp/.hpp` and `utility/display-helpers.hpp`, copyright
  OBS Studio contributors, Lain Bailey, Zachary Lund and Philippe Groarke,
  GPL-2.0-or-later. Complete terms and exact pinned source SHA256 receipts are
  in `projectors-multiview/`. This Rust/egui implementation uses the shared
  renderer; no Qt/C++/libobs binary is linked. Scope and limits are documented
  in `doc/migration/projectors-multiview.md`.

### OBS six-track source audio routing

Core mix masks and common-grid audio routing follow OBS Studio 32.2.2, peeled commit ba2f32bdf791005443988a4955e963663e16b1ed (annotated tag fc546137470c0f4a5f1493b6e7815545ee1b170b), libobs/obs-source.c source mixers and libobs/media-io/audio-io.c per-mix rendering; GPL-2.0-or-later, OBS Project contributors. Existing Rust source DSP and linked detector equations are unchanged. This port uses source-ID routes, bounded processed history and real native per-track encoder ports; it adds no C/C++ backend.

Projector defaults and Multiview scene visibility further adapt the pinned
OBS32.2.2 General settings / scene private metadata workflows under
GPL-2.0-or-later. See assets/licenses/projector-settings/provenance.json and
assets/licenses/projectors-multiview/OBS-GPL-2.0-or-later.txt. This source-only
work does not establish native rendering, UI or performance acceptance.

Sources-list item color tags adapt OBS32.2.2 AddBackgroundColorMenu/ColorChange/
ConfirmColor/SourceTreeItem private metadata workflow under GPL-2.0-or-later.
See assets/licenses/source-color-tags/provenance.json; license is retained at
assets/licenses/projectors-multiview/OBS-GPL-2.0-or-later.txt. Source work only;
no GUI/native/runtime/persistence acceptance or source pixel/audio changes.

### Craft Mod Delay and FDN Reverb

Adapted from the local SoundCraft crates/dsp/src/plugins/modulation.rs and util.rs and FilmCraft crates/audio-dsp/src/effects/time.rs, smooth.rs and lib.rs. Copyright (c)2026 ArtCraft Team and SoundCraft/FilmCraft contributors. Both donors offer MIT OR Apache-2.0; these algorithms are included under the MIT option. Complete notices: soundcraft-audio-effects-MIT.txt and filmcraft-audio-effects-MIT.txt in this directory. No proprietary assets, presets, icons, native plugin binaries or external SDK code are included. The port preserves original scalar DSP equations and units while using bounded source-owned paged COW storage.

## RNNoise original-model Rust primitive

The pure-Rust media RNNoise analysis/synthesis, pitch/LPC, fixed960 KISS/Opus FFT, original quantized recurrent model and activation table derive from the RNNoise bundled with OBS Studio 32.2.2, peeled commit ba2f32bdf791005443988a4955e963663e16b1ed. Copyright 2017 Mozilla; 2007–2017 Jean-Marc Valin; 2005–2017 Xiph.Org Foundation; 2003–2004 Mark Borgerding; recurrent-layer source also credits 2008–2011 Octasic Inc. Complete COPYING and individual source notices: [RNNOISE.txt](RNNOISE.txt). The source-only port does not use native RNNoise FFI or proprietary weights. Application filter support is not advertised until the delayed packet pipeline is connected.

Native projector-window restoration adapts OBS32.2.2 SaveProjectors/
LoadSavedProjectors/OpenSavedProjector workflow under GPL-2.0-or-later.
Native SkyLive descriptors use bounded collection-local IDs/physical geometry;
no Qt geometry mapping is claimed. See assets/licenses/projector-restoration/
provenance.json and the retained projectors-multiview GPL license.

## Last native document startup

The independent remembered-file workflow follows supplied OBS Studio32.2.2
`OBSBasic::OBSInit` configured collection selection and `ActivateSceneCollection`
user-configuration metadata behavior. Reference hashes and GPL-2.0-or-later
notice are recorded in `last-document-startup/provenance.json` (existing OBS
license copy linked there). The Rust implementation uses native absolute file
identity and background candidate publication; it does not copy C++/Qt startup
execution, source-name resolution or profile/scene collection filesystem layout.
No runtime/platform acceptance is asserted.

## Fade to Color controls

The black/50 defaults and Peak Color Point0..100 controls follow the supplied
OBS Studio32.2.2 Fade to Color provider (GPL-2.0-or-later). Reference hashes and
existing license copy are linked in fade-to-color-controls/provenance.json.
Existing scalar media/audio implementation is preserved; no new executed
shader/native/device/performance acceptance is claimed.

### Source playback hotkeys

Source-bound playback wrappers follow OBS Studio32.2.2 GPL-2.0-or-later showing/manual/private-paused semantics from image-source and obs-ffmpeg, with source duplicate hotkey ownership from libobs/obs-source.c. The existing Rust Windows global registry is reused; no libobs/Qt/FFmpeg binary is linked. Pinned source and complete-license reference: source-playback-hotkeys/provenance.json. Source-only; native registration, playback, UI and full hotkey parity are unverified.

### OBS relative scene coordinates

The supported OBS32.2.2 relative position/scale conversion follows libobs/obs-scene.c (GPL-2.0-or-later), with collection version metadata from frontend/models/SceneCollection.hpp. Complete existing license and pinned source provenance are linked in obs-relative-coordinates/provenance.json. The Rust importer normalizes at an explicit profile canvas and exports absolute v1; it does not link libobs or claim native relative document/remigration parity. Source-only, no runtime or file-roundtrip acceptance.

## Managed Scene Collection library

The managed native collection workflow follows supplied OBS Studio32.2.2
Scene Collection New/Duplicate/Rename/Remove/Activate behavior under
GPL-2.0-or-later. Exact hashes and existing license copy are linked in
scene-collection-library/provenance.json. The Rust implementation uses owned
ID-derived native filenames, explicit dirty Save/Discard/Cancel and recoverable
trash rather than copying original automatic saves/destructive arbitrary-file
policies. No runtime/filesystem/native/platform acceptance is claimed.


## Missing Files workflow (source port)

Behavioral reference: OBS Studio32.2.2, commit
`ba2f32bdf791005443988a4955e963663e16b1ed`, GPL-2.0-or-later:
frontend/dialogs/OBSMissingFiles.cpp, frontend/utility/MissingFilesModel.cpp,
libobs/obs-missing-files.c, plugins/image-source/image-source.c and
obs-slideshow.c, plugins/obs-ffmpeg/obs-ffmpeg-source.c,
plugins/obs-filters/mask-filter.c and color-grade-filter.c.
Rust implementation uses typed expected-resource batches and the existing Rust
asset preparation ports; no OBS binary/source callback ABI is loaded.
Done/Cancel staging and authored slideshow-entry ownership follow this reference.
See doc/migration/missing-files.md for adapted required-resource Clear and
doc/migration/failed-open-resource-repair.md for the unpublished native/recovery/
startup/managed-switch candidate workflow. Supplied OBS licensing remains included.

The same-path Media relink adapter also introduces a Rust-only Reload transport
operation using SkyLive's existing typed media actor/Program-audio barrier. It
does not load OBS callbacks. Its receipt acknowledges current-source reader
retirement or absence; decoder readiness is separately observed and unverified.

## OBS Render Delay

The Rust CPU SDR Render Delay follows OBS Studio32.2.2 gpu-delay.c (GPL-2.0-or-later), pinned in obs-render-delay/provenance.json with the complete license. It uses detached source-owned RGBA history on the existing Rust filter worker, not an OBS binary or GPU texrender implementation. Stock floor-slot rotation and transparent warmup are retained; explicit privacy retirement, bounded128MiB admission and actual-render-cadence limitations are documented in doc/modules/obs-render-delay.md. HDR/tone-map/GPU parity and runtime acceptance are not claimed.

## Image Source properties

Image unload/linear-alpha controls and the scalar byte conversion follow OBS
Studio32.2.2 image-source.c and libobs/graphics/srgb.h, GPL-2.0-or-later. Exact
source identities and the included complete license are linked in
image-source-properties/provenance.json. The Rust implementation converts
exclusively owned decoded frames and uses the existing render workers. Automatic
file watching and eager hidden preloading remain separate work. Compiler, pixel,
resource-lifecycle and native acceptance have not been executed.

## Text Source case transform

The four Text Source transform choices and Startcase state machine follow OBS
Studio32.2.2 obs-text.cpp, GPL-2.0-or-later; exact source and included full license
are linked in text-case-transform/provenance.json. The Rust port uses portable
Unicode per-scalar casing with bounded expansion, retaining authored text. It
does not use the Windows locale facet or claim GDI/locale glyph equivalence.

## Source Mute and Unmute shortcuts

The source mute/unmute pair follows libobs/obs-source.c297..370 from OBS
Studio32.2.2, commit ba2f32bdf791005443988a4955e963663e16b1ed,
GPL-2.0-or-later. The complete supplied license is included under
obs-render-delay/OBS-GPL-2.0-or-later.txt. Rust uses the existing canonical mute
command and configured-key owner, not libobs or a physical device-mute API.
Held push-to-talk/mute timing remains unimplemented in this slice.

## Camera source vertical orientation

The source-level flip_vertically property follows OBS Studio32.2.2 win-dshow.cpp,
commit ba2f32bdf791005443988a4955e963663e16b1ed, GPL-2.0-or-later. The full license
is included under obs-render-delay/OBS-GPL-2.0-or-later.txt. Rust normalizes native
rows first and applies source orientation on the existing acquisition owner,
retaining actual device mode, frame origin and manual AV control semantics.
No native OBS binary is linked; camera visual/runtime acceptance is unexecuted.

- Craft Sepia (Photo Filter) adapts only EffectCraft `color_fx.rs::photo_filter`,
  fixed PHOTO_FILTERS Sepia RGB[170,120,55], `mix`, Rec.709 `color::luminance` and
  alpha-preserving `raster::Image::map_straight`, copyright2026 ArtCraft Team and
  the EffectCraft contributors, MIT selected from MIT OR Apache-2.0. Exact local
  revision991904888a20b7da72b2a4880b21c7d912154736 and file receipts are recorded
  in `doc/migration/craft-sepia-provenance.json`. Existing complete selected MIT
  license is `craft/effectcraft-MIT.txt`. SkyLive specializes the preset, retains
  density/preserve-luminosity controls, and adapts it to the existing encoded SDR8
  CPU point chain; it does not add the full Photo Filter menu or GPU/HDR engine.

## Exeldro Freeze Filter manual CPU port

The mask selection and manual enabled/capture behavior follow
exeldro/obs-freeze-filter0.3.5, commit
5de212bd067672f9349a85e0494a65c8b0156bd4, GPL version2.
The supplied complete license is included in
exeldro-freeze-filter/EXELDRO-GPL-2.0.txt; provenance.json names the exact
reference files. No native OBS plugin, graphics API implementation or binary is
linked. This finite Rust provider supports manual hold and partial edge masks
on the existing shared CPU worker. Automatic duration/refresh/lifecycle actions,
logging, Freeze hotkeys, filter-position screenshots and HDR are not ported.
Native/model/pixel/runtime acceptance has not been executed.

## Preview movement snapping

The source-edge/canvas-edge/center movement policy and General controls follow
OBS Studio32.2.2, commit ba2f32bdf791005443988a4955e963663e16b1ed,
GPL-2.0-or-later. Exact source paths and the existing complete license are
referenced by preview-snapping/provenance.json. Rust uses the existing validated
geometry, gesture and Core command owners. No native OBS or Qt binary is linked;
single-item movement and handle resizing are the finite scope and native
acceptance is unexecuted.

- Browser local-HTML Missing Files relinking follows OBS32.2.2's pinned
  obs-browser3f0a2cdf378939ebe3c6f9ab36d4ea100c25aac2
  `obs-browser-plugin.cpp::browser_source_missingfiles/missing_file_callback` and
  `obs-browser-source.cpp::Update`, copyright2014 John R. Bradley and2023 Lain Bailey,
  GPL-2.0-or-later. Source hashes/URLs and the limited Rust metadata/reload adaptation
  are in `doc/migration/browser-missing-files-provenance.json` and the adjacent doc.
  No C++ runtime/glue is introduced; existing project GPL license applies.

- Craft Photo Filter adapts EffectCraft991904888a20b7da72b2a4880b21c7d912154736
  `color_fx.rs::PHOTO_FILTERS/photo_filter/mix`, `color::luminance` and
  `raster::Image::map_straight`, copyright2026 ArtCraft Team and the EffectCraft
  contributors, MIT selected from MIT OR Apache-2.0. The20 fixed palette colors
  and actual Custom color use the existing bounded Rust SDR CPU point chain;
  retained Sepia shares that selected formula without changing its saved shape.
  See `doc/migration/craft-photo-filter-provenance.json` for exact git-object
  hashes/URLs/defaults and the adjacent scope doc. The complete selected license
  remains `craft/effectcraft-MIT.txt`. No proprietary assets, donor engine or new
  GPU/native dependency is introduced.

- Craft Gaussian Blur adapts EffectCraft 991904888a20b7da72b2a4880b21c7d912154736
  misc.rs and raster/blur.rs, copyright 2026 ArtCraft Team and contributors,
  under the selected MIT license; see craft/effectcraft-MIT.txt and
  doc/migration/craft-gaussian-blur-provenance.json for source identity.
