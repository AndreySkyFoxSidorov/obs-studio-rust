//! AVAssetWriter MP4/H.264/AAC. Construct, append and retire only on the
//! recording owner. Completion holds no borrowed buffers or Objective-C owner.
use super::{PreparedVideo, RecordingConfig, RecordingError, RecordingWriter};
use block2::RcBlock;
use objc2::{
    AnyThread,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};
use objc2_av_foundation::*;
use objc2_avf_audio::{AVEncoderBitRateKey, AVFormatIDKey, AVNumberOfChannelsKey, AVSampleRateKey};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, kAudioFormatFlagIsPacked, kAudioFormatFlagIsSignedInteger,
    kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_core_media::*;
use objc2_core_video::*;
use objc2_foundation::{NSDictionary, NSError, NSNumber, NSString, NSURL};
use skylive_media::{RgbaFrame, recording_nv12};
use std::{
    path::Path,
    ptr::{self, NonNull},
    rc::Rc,
    sync::mpsc,
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

#[path = "native_macos/layout.rs"]
mod layout;

// Apple's VideoToolbox/VTVideoEncoderList.h exports these two CFStringRef
// specification keys. AVAssetWriter accepts them under its typed
// AVVideoEncoderSpecificationKey; no encoder implementation is vendored here.
#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C-unwind" {
    static kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: &'static CFString;
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: &'static CFString;
}
const READY_WAIT: Duration = Duration::from_secs(2);
const WAIT_STEP: Duration = Duration::from_millis(2);

struct NativeObjects {
    writer: Retained<AVAssetWriter>,
    video: Retained<AVAssetWriterInput>,
    adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    audio: Option<Retained<AVAssetWriterInput>>,
    audio_format: Option<CFRetained<CMAudioFormatDescription>>,
    pool_aux: Retained<NSDictionary<NSString, AnyObject>>,
}
pub struct Writer {
    native: Option<NativeObjects>,
    config: RecordingConfig,
    last_video: Option<u64>,
    audio_end: Option<u64>,
    end: (i64, i32),
    finished: Option<Result<(), RecordingError>>,
    owner: ThreadId,
    _not_send: Rc<()>,
}
impl Writer {
    fn sdk(&self) -> &NativeObjects {
        self.native
            .as_ref()
            .expect("native objects remain retained until owner Drop")
    }
    pub fn new(config: &RecordingConfig, path: &Path) -> Result<Self, RecordingError> {
        autoreleasepool(|_| Self::new_native(config, path))
    }
    fn new_native(config: &RecordingConfig, path: &Path) -> Result<Self, RecordingError> {
        config.validate()?;
        layout::video_time(0, config.video.fps_num(), config.video.fps_den())?;
        let path = path
            .to_str()
            .ok_or_else(|| RecordingError::invalid("AVAssetWriter output path is not UTF-8"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let writer = unsafe {
            AVAssetWriter::initWithURL_fileType_error(
                AVAssetWriter::alloc(),
                &url,
                key(AVFileTypeMPEG4)?,
            )
        }
        .map_err(|error| sdk_error("create AVAssetWriter", &error))?;
        let settings = video_settings(config)?;
        let video_type = unsafe { key(AVMediaTypeVideo)? };
        if !unsafe { writer.canApplyOutputSettings_forMediaType(Some(&settings), video_type) } {
            return Err(RecordingError::invalid(
                "AVAssetWriter does not support the requested H264 settings",
            ));
        }
        let video = unsafe {
            AVAssetWriterInput::initWithMediaType_outputSettings(
                AVAssetWriterInput::alloc(),
                video_type,
                Some(&settings),
            )
        };
        unsafe {
            video.setExpectsMediaDataInRealTime(true);
            video.setMediaTimeScale(
                i32::try_from(config.video.fps_num())
                    .map_err(|_| RecordingError::invalid("recording video timescale overflow"))?,
            );
        }
        let width = NSNumber::new_u32(config.video.width());
        let height = NSNumber::new_u32(config.video.height());
        let pixel_format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        let format_key = ns_cf(unsafe { kCVPixelBufferPixelFormatTypeKey });
        let width_key = ns_cf(unsafe { kCVPixelBufferWidthKey });
        let height_key = ns_cf(unsafe { kCVPixelBufferHeightKey });
        let attributes = dict(
            &[&format_key, &width_key, &height_key],
            &[&pixel_format, &width, &height],
        );
        let adaptor = unsafe {
            AVAssetWriterInputPixelBufferAdaptor::initWithAssetWriterInput_sourcePixelBufferAttributes(AVAssetWriterInputPixelBufferAdaptor::alloc(),&video,Some(&attributes))
        };
        if !unsafe { writer.canAddInput(&video) } {
            return Err(RecordingError::invalid(
                "AVAssetWriter cannot add its video input",
            ));
        }
        unsafe {
            writer.addInput(&video);
        }
        let (audio, audio_format) = if let Some(format) = config.audio {
            let settings = audio_settings(format)?;
            let audio_type = unsafe { key(AVMediaTypeAudio)? };
            if !unsafe { writer.canApplyOutputSettings_forMediaType(Some(&settings), audio_type) } {
                return Err(RecordingError::invalid(
                    "AVAssetWriter does not support the requested AAC settings",
                ));
            }
            let description = pcm_format(format.sample_rate, format.channels)?;
            let input = unsafe {
                AVAssetWriterInput::initWithMediaType_outputSettings_sourceFormatHint(
                    AVAssetWriterInput::alloc(),
                    audio_type,
                    Some(&settings),
                    Some(&description),
                )
            };
            unsafe {
                input.setExpectsMediaDataInRealTime(true);
                input.setMediaTimeScale(format.sample_rate as i32);
            }
            if !unsafe { writer.canAddInput(&input) } {
                return Err(RecordingError::invalid(
                    "AVAssetWriter cannot add its audio input",
                ));
            }
            unsafe {
                writer.addInput(&input);
            }
            (Some(input), Some(description))
        } else {
            (None, None)
        };
        let threshold_key = ns_cf(unsafe { kCVPixelBufferPoolAllocationThresholdKey });
        let threshold = NSNumber::new_u32(3);
        let pool_aux = dict(&[&threshold_key], &[&threshold]);
        let mut result = Self {
            native: Some(NativeObjects {
                writer,
                video,
                adaptor,
                audio,
                audio_format,
                pool_aux,
            }),
            config: config.clone(),
            last_video: None,
            audio_end: None,
            end: (0, 1),
            finished: None,
            owner: thread::current().id(),
            _not_send: Rc::new(()),
        };
        if !unsafe { result.sdk().writer.startWriting() } {
            let error = result.failure("start AVAssetWriter");
            result.finished = Some(Err(error.clone()));
            return Err(error);
        }
        unsafe {
            result.sdk().writer.startSessionAtSourceTime(time((0, 1)));
        }
        Ok(result)
    }
    fn check_owner(&self) -> Result<(), RecordingError> {
        if self.owner != thread::current().id() {
            Err(RecordingError::invalid(
                "AVAssetWriter used outside its recording owner",
            ))
        } else if self.finished.is_some() {
            Err(RecordingError::invalid(
                "AVAssetWriter recording already finalized",
            ))
        } else {
            Ok(())
        }
    }
    fn failure(&self, operation: &str) -> RecordingError {
        if let Some(error) = unsafe { self.sdk().writer.error() } {
            sdk_error(operation, &error)
        } else {
            RecordingError::native(
                operation,
                format!(
                    "AVAssetWriter status {}",
                    unsafe { self.sdk().writer.status() }.0
                ),
            )
        }
    }
    fn ready(&self, input: &AVAssetWriterInput, start: Instant) -> Result<(), RecordingError> {
        loop {
            if unsafe { self.sdk().writer.status() } != AVAssetWriterStatus::Writing {
                return Err(self.failure("AVAssetWriter stopped accepting samples"));
            }
            if unsafe { input.isReadyForMoreMediaData() } {
                return Ok(());
            }
            if start.elapsed() >= READY_WAIT {
                return Err(RecordingError::native(
                    "AVAssetWriter input readiness",
                    "two second worker deadline exceeded",
                ));
            }
            thread::sleep(WAIT_STEP);
        }
    }
    fn buffer(&self, start: Instant) -> Result<CFRetained<CVPixelBuffer>, RecordingError> {
        let pool = unsafe { self.sdk().adaptor.pixelBufferPool() }
            .ok_or_else(|| self.failure("AVAssetWriter pixel buffer pool unavailable"))?;
        loop {
            let mut raw = ptr::null_mut();
            // NSDictionary<NSString, NSNumber> is toll-free bridged to an
            // immutable CFDictionary of CFString/CFNumber allocation settings.
            let auxiliary = unsafe {
                &*((&*self.sdk().pool_aux as *const NSDictionary<NSString, AnyObject>)
                    .cast::<CFDictionary>())
            };
            let status = unsafe {
                CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
                    None,
                    &pool,
                    Some(auxiliary),
                    NonNull::from(&mut raw),
                )
            };
            if status == kCVReturnSuccess {
                return NonNull::new(raw)
                    .map(|raw| unsafe { CFRetained::from_raw(raw) })
                    .ok_or_else(|| {
                        RecordingError::invalid("CoreVideo returned a null recording buffer")
                    });
            }
            if status != kCVReturnWouldExceedAllocationThreshold {
                return Err(RecordingError::native(
                    "allocate bounded recording pixel buffer",
                    status,
                ));
            }
            if start.elapsed() >= READY_WAIT {
                return Err(RecordingError::native(
                    "AVAssetWriter three-buffer pool readiness",
                    "two second worker deadline exceeded",
                ));
            }
            if unsafe { self.sdk().writer.status() } != AVAssetWriterStatus::Writing {
                return Err(self.failure("AVAssetWriter stopped during pixel buffer admission"));
            }
            thread::sleep(WAIT_STEP);
        }
    }
    fn finish_native(&mut self) -> Result<(), RecordingError> {
        if self.owner != thread::current().id() {
            return Err(RecordingError::invalid(
                "AVAssetWriter finalized outside its owner",
            ));
        }
        let status = unsafe { self.sdk().writer.status() };
        if status == AVAssetWriterStatus::Completed {
            return Ok(());
        }
        if status != AVAssetWriterStatus::Writing {
            return Err(self.failure("finalize AVAssetWriter"));
        }
        unsafe {
            self.sdk().video.markAsFinished();
            if let Some(audio) = &self.sdk().audio {
                audio.markAsFinished();
            }
            if self.end.0 > 0 {
                self.sdk().writer.endSessionAtSourceTime(time(self.end));
            }
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let completion = RcBlock::new(move || {
            let _ = sender.try_send(());
        });
        // Actual completion is the release fence. A hung SDK keeps this admitted
        // worker/owner alive; no timeout is mistaken for native quiescence.
        unsafe {
            self.sdk()
                .writer
                .finishWritingWithCompletionHandler(&completion);
        }
        receiver.recv().map_err(|error| {
            RecordingError::native("AVAssetWriter finalization completion", error)
        })?;
        if unsafe { self.sdk().writer.status() } == AVAssetWriterStatus::Completed {
            Ok(())
        } else {
            Err(self.failure("finish AVAssetWriter"))
        }
    }
}
impl Writer {
    fn validate_video(&self, frame: &RgbaFrame, index: u64) -> Result<(), RecordingError> {
        self.check_owner()?;
        if (frame.width(), frame.height())
            != (self.config.video.width(), self.config.video.height())
        {
            return Err(RecordingError::invalid(
                "recording frame dimensions changed",
            ));
        }
        if self.last_video.is_some_and(|last| index <= last) {
            return Err(RecordingError::invalid(
                "recording video timestamp is not increasing",
            ));
        }
        Ok(())
    }
    fn video_native(&mut self, frame: &RgbaFrame, index: u64) -> Result<(), RecordingError> {
        self.validate_video(frame, index)?;
        let packed = recording_nv12(frame, 32 * 1024 * 1024, 18_000_000)
            .map_err(|error| RecordingError::native("Program NV12 preparation", error))?;
        self.video_native_prepared(frame, &packed, index)
    }
    fn video_native_prepared(
        &mut self,
        frame: &RgbaFrame,
        packed: &[u8],
        index: u64,
    ) -> Result<(), RecordingError> {
        self.validate_video(frame, index)?;
        let pts = layout::video_time(
            index,
            self.config.video.fps_num(),
            self.config.video.fps_den(),
        )?;
        let end = layout::video_time(
            index
                .checked_add(1)
                .ok_or_else(|| RecordingError::invalid("recording frame index overflow"))?,
            self.config.video.fps_num(),
            self.config.video.fps_den(),
        )?;
        let start = Instant::now();
        self.ready(&self.sdk().video, start)?;
        let buffer = self.buffer(start)?;
        copy_video(
            &buffer,
            packed,
            frame.width() as usize,
            frame.height() as usize,
        )?;
        if !unsafe {
            self.sdk()
                .adaptor
                .appendPixelBuffer_withPresentationTime(&buffer, time(pts))
        } {
            return Err(self.failure("append recording video"));
        }
        self.last_video = Some(index);
        self.end = layout::later(self.end, end);
        Ok(())
    }
    fn audio_native(&mut self, samples: &[i16], first_frame: u64) -> Result<(), RecordingError> {
        self.check_owner()?;
        let format = self
            .config
            .audio
            .ok_or_else(|| RecordingError::invalid("audio is disabled for this recording"))?;
        if samples.is_empty()
            || samples.len() % usize::from(format.channels) != 0
            || samples.len() / usize::from(format.channels) > 4096
        {
            return Err(RecordingError::invalid("recording PCM block shape"));
        }
        if self.audio_end.is_some_and(|end| first_frame < end) {
            return Err(RecordingError::invalid(
                "recording PCM timestamp overlaps previous input",
            ));
        }
        let frames = samples.len() / usize::from(format.channels);
        let end = first_frame
            .checked_add(frames as u64)
            .ok_or_else(|| RecordingError::invalid("recording PCM frame index overflow"))?;
        let pts = layout::audio_time(first_frame, format.sample_rate)?;
        let finish = layout::audio_time(end, format.sample_rate)?;
        let input = self
            .sdk()
            .audio
            .as_ref()
            .ok_or_else(|| RecordingError::invalid("AAC input is unavailable"))?;
        let description = self
            .sdk()
            .audio_format
            .as_ref()
            .ok_or_else(|| RecordingError::invalid("PCM format is unavailable"))?;
        self.ready(input, Instant::now())?;
        let length = samples.len() * 2;
        let mut raw = ptr::null_mut();
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                length,
                None,
                ptr::null(),
                0,
                length,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut raw),
            )
        };
        os_status("create owned PCM block", status)?;
        let block = NonNull::new(raw)
            .map(|raw| unsafe { CFRetained::from_raw(raw) })
            .ok_or_else(|| RecordingError::invalid("CoreMedia returned a null PCM block"))?;
        // Both supported macOS architectures are little-endian. The ASBD below
        // describes signed, packed native-endian PCM; CoreMedia copies it now.
        let pointer = NonNull::new(samples.as_ptr().cast_mut().cast())
            .ok_or_else(|| RecordingError::invalid("PCM pointer is null"))?;
        os_status("copy recording PCM", unsafe {
            CMBlockBuffer::replace_data_bytes(pointer, &block, 0, length)
        })?;
        let mut raw = ptr::null_mut();
        os_status("create recording PCM sample", unsafe {
            CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                None,
                &block,
                description,
                frames as isize,
                time(pts),
                ptr::null(),
                NonNull::from(&mut raw),
            )
        })?;
        let sample = NonNull::new(raw)
            .map(|raw| unsafe { CFRetained::from_raw(raw) })
            .ok_or_else(|| RecordingError::invalid("CoreMedia returned a null PCM sample"))?;
        if !unsafe { input.appendSampleBuffer(&sample) } {
            return Err(self.failure("append recording PCM"));
        }
        self.audio_end = Some(end);
        self.end = layout::later(self.end, finish);
        Ok(())
    }
    fn finalize_native(&mut self) -> Result<(), RecordingError> {
        if let Some(result) = &self.finished {
            return result.clone();
        }
        let result = self.finish_native();
        self.finished = Some(result.clone());
        result
    }
}
impl RecordingWriter for Writer {
    fn video(&mut self, frame: &RgbaFrame, index: u64) -> Result<(), RecordingError> {
        autoreleasepool(|_| self.video_native(frame, index))
    }
    fn video_prepared(&mut self, frame: &PreparedVideo, index: u64) -> Result<(), RecordingError> {
        // buffer() returns a pool-owned CVPixelBuffer, copied afresh and retained
        // independently by AVAssetWriter; the cached CPU slice is never lent.
        autoreleasepool(|_| self.video_native_prepared(frame.image(), frame.nv12(), index))
    }
    fn audio(&mut self, samples: &[i16], first_frame: u64) -> Result<(), RecordingError> {
        autoreleasepool(|_| self.audio_native(samples, first_frame))
    }
    fn finalize(&mut self) -> Result<(), RecordingError> {
        autoreleasepool(|_| self.finalize_native())
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.finalize();
        autoreleasepool(|_| drop(self.native.take()));
    }
}

fn time(value: (i64, i32)) -> CMTime {
    CMTime {
        value: value.0,
        timescale: value.1,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}
fn key(value: Option<&'static NSString>) -> Result<&'static NSString, RecordingError> {
    value.ok_or_else(|| {
        RecordingError::invalid("required AVFoundation recording constant is unavailable")
    })
}
fn ns_cf(value: &CFString) -> Retained<NSString> {
    NSString::from_str(&value.to_string())
}
fn dict(keys: &[&NSString], values: &[&AnyObject]) -> Retained<NSDictionary<NSString, AnyObject>> {
    NSDictionary::from_slices(keys, values)
}
fn os_status(operation: &str, status: i32) -> Result<(), RecordingError> {
    if status == 0 {
        Ok(())
    } else {
        Err(RecordingError::native(operation, status))
    }
}
fn sdk_error(operation: &str, error: &NSError) -> RecordingError {
    RecordingError::native(
        operation,
        format!(
            "{} ({}): {}",
            &*error.domain(),
            error.code(),
            &*error.localizedDescription()
        ),
    )
}

fn video_settings(
    config: &RecordingConfig,
) -> Result<Retained<NSDictionary<NSString, AnyObject>>, RecordingError> {
    let bitrate = NSNumber::new_u32(config.video_bitrate_kbps * 1000);
    let rate =
        NSNumber::new_f64(f64::from(config.video.fps_num()) / f64::from(config.video.fps_den()));
    let reorder = NSNumber::new_bool(false);
    let compression = unsafe {
        dict(
            &[
                key(AVVideoAverageBitRateKey)?,
                key(AVVideoExpectedSourceFrameRateKey)?,
                key(AVVideoAllowFrameReorderingKey)?,
                key(AVVideoProfileLevelKey)?,
            ],
            &[
                &bitrate,
                &rate,
                &reorder,
                key(AVVideoProfileLevelH264HighAutoLevel)?,
            ],
        )
    };
    let color = unsafe {
        dict(
            &[
                key(AVVideoColorPrimariesKey)?,
                key(AVVideoTransferFunctionKey)?,
                key(AVVideoYCbCrMatrixKey)?,
            ],
            &[
                key(AVVideoColorPrimaries_ITU_R_709_2)?,
                key(AVVideoTransferFunction_ITU_R_709_2)?,
                key(AVVideoYCbCrMatrix_ITU_R_709_2)?,
            ],
        )
    };
    let enabled = NSNumber::new_bool(config.prefer_hardware);
    let required = NSNumber::new_bool(false);
    let enabled_key =
        ns_cf(unsafe { kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder });
    let required_key =
        ns_cf(unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder });
    let encoder = dict(&[&enabled_key, &required_key], &[&enabled, &required]);
    let width = NSNumber::new_u32(config.video.width());
    let height = NSNumber::new_u32(config.video.height());
    unsafe {
        Ok(dict(
            &[
                key(AVVideoCodecKey)?,
                key(AVVideoWidthKey)?,
                key(AVVideoHeightKey)?,
                key(AVVideoCompressionPropertiesKey)?,
                key(AVVideoColorPropertiesKey)?,
                key(AVVideoEncoderSpecificationKey)?,
            ],
            &[
                key(AVVideoCodecTypeH264)?,
                &width,
                &height,
                &compression,
                &color,
                &encoder,
            ],
        ))
    }
}
fn audio_settings(
    format: super::RecordingAudio,
) -> Result<Retained<NSDictionary<NSString, AnyObject>>, RecordingError> {
    let codec = NSNumber::new_u32(kAudioFormatMPEG4AAC);
    let rate = NSNumber::new_u32(format.sample_rate);
    let channels = NSNumber::new_u8(format.channels);
    let bitrate = NSNumber::new_u32(format.bitrate_kbps * 1000);
    unsafe {
        Ok(dict(
            &[
                key(AVFormatIDKey)?,
                key(AVSampleRateKey)?,
                key(AVNumberOfChannelsKey)?,
                key(AVEncoderBitRateKey)?,
            ],
            &[&codec, &rate, &channels, &bitrate],
        ))
    }
}
fn pcm_format(
    rate: u32,
    channels: u8,
) -> Result<CFRetained<CMAudioFormatDescription>, RecordingError> {
    let bytes = u32::from(channels) * 2;
    let mut description = AudioStreamBasicDescription {
        mSampleRate: f64::from(rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsPacked | kAudioFormatFlagIsSignedInteger,
        mBytesPerPacket: bytes,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes,
        mChannelsPerFrame: u32::from(channels),
        mBitsPerChannel: 16,
        mReserved: 0,
    };
    let mut raw = ptr::null();
    os_status("create recording PCM format", unsafe {
        CMAudioFormatDescriptionCreate(
            None,
            NonNull::from(&mut description),
            0,
            ptr::null(),
            0,
            ptr::null(),
            None,
            NonNull::from(&mut raw),
        )
    })?;
    NonNull::new(raw.cast_mut())
        .map(|raw| unsafe { CFRetained::from_raw(raw) })
        .ok_or_else(|| RecordingError::invalid("CoreMedia returned a null PCM format"))
}
struct Locked<'a>(&'a CVPixelBuffer);
impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let _ = unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::empty()) };
    }
}
fn copy_video(
    buffer: &CVPixelBuffer,
    packed: &[u8],
    width: usize,
    height: usize,
) -> Result<(), RecordingError> {
    if CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        || CVPixelBufferGetPlaneCount(buffer) != 2
        || CVPixelBufferGetWidth(buffer) != width
        || CVPixelBufferGetHeight(buffer) != height
        || CVPixelBufferGetWidthOfPlane(buffer, 0) != width
        || CVPixelBufferGetHeightOfPlane(buffer, 0) != height
        || CVPixelBufferGetWidthOfPlane(buffer, 1) != width / 2
        || CVPixelBufferGetHeightOfPlane(buffer, 1) != height / 2
    {
        return Err(RecordingError::invalid(
            "CoreVideo recording NV12 layout changed",
        ));
    }
    let layout = layout::plane_layout(
        width,
        height,
        CVPixelBufferGetBytesPerRowOfPlane(buffer, 0),
        CVPixelBufferGetBytesPerRowOfPlane(buffer, 1),
        CVPixelBufferGetDataSize(buffer),
    )?;
    os_status("lock recording pixel buffer", unsafe {
        CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::empty())
    })?;
    let _locked = Locked(buffer);
    let y = CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>();
    let uv = CVPixelBufferGetBaseAddressOfPlane(buffer, 1).cast::<u8>();
    if y.is_null() || uv.is_null() {
        return Err(RecordingError::invalid(
            "CoreVideo recording plane pointer is null",
        ));
    }
    let y_end = (y as usize)
        .checked_add(layout.y_bytes)
        .ok_or_else(|| RecordingError::invalid("recording luma pointer extent overflow"))?;
    let uv_end = (uv as usize)
        .checked_add(layout.uv_bytes)
        .ok_or_else(|| RecordingError::invalid("recording chroma pointer extent overflow"))?;
    if y_end > uv as usize && uv_end > y as usize {
        return Err(RecordingError::invalid(
            "CoreVideo recording planes overlap",
        ));
    }
    // CoreVideo owns these two disjoint writable planes for this successful
    // lock. Dimensions, row extents and the total native allocation are checked.
    layout::copy_planes(
        packed,
        unsafe { std::slice::from_raw_parts_mut(y, layout.y_bytes) },
        unsafe { std::slice::from_raw_parts_mut(uv, layout.uv_bytes) },
        layout,
    )?;
    unsafe {
        buffer.set_attachment(
            kCVImageBufferColorPrimariesKey,
            kCVImageBufferColorPrimaries_ITU_R_709_2.as_ref(),
            CVAttachmentMode::ShouldPropagate,
        );
        buffer.set_attachment(
            kCVImageBufferTransferFunctionKey,
            kCVImageBufferTransferFunction_ITU_R_709_2.as_ref(),
            CVAttachmentMode::ShouldPropagate,
        );
        buffer.set_attachment(
            kCVImageBufferYCbCrMatrixKey,
            kCVImageBufferYCbCrMatrix_ITU_R_709_2.as_ref(),
            CVAttachmentMode::ShouldPropagate,
        );
    }
    Ok(())
}

#[cfg(test)]
mod native_acceptance {
    use super::*;
    use std::io::Write;
    #[test]
    #[ignore = "Opt-in macOS codec execution: set SKYLIVE_MAC_RECORDING_ACCEPTANCE_DIRECTORY; retains two synthetic MP4 fixtures and JSON evidence"]
    fn real_h264_aac_files_pass_independent_rust_decoder_oracles() {
        let base = std::env::var_os("SKYLIVE_MAC_RECORDING_ACCEPTANCE_DIRECTORY")
            .expect("explicit acceptance output directory is required");
        let base = std::path::absolute(base).unwrap();
        std::fs::create_dir_all(&base).unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = base.join(format!("macos-recording-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let mut receipts = vec![];
        for prefer_hardware in [false, true] {
            let path = directory.join(if prefer_hardware {
                "prefer-hardware.mp4"
            } else {
                "software.mp4"
            });
            let config = RecordingConfig {
                path: path.clone(),
                video: skylive_core::VideoSettings::new(64, 32, 30, 1).unwrap(),
                video_bitrate_kbps: 6000,
                audio: Some(super::super::RecordingAudio {
                    sample_rate: 48000,
                    channels: 2,
                    bitrate_kbps: 128,
                }),
                prefer_hardware,
            };
            let mut writer = Writer::new(&config, &path).unwrap();
            for index in 0..30u64 {
                let mut pixels = Vec::with_capacity(64 * 32 * 4);
                for _y in 0..32 {
                    for x in 0..64 {
                        pixels.extend_from_slice(&match x / 16 {
                            0 => [255, 0, 0, 255],
                            1 => [0, 255, 0, 255],
                            2 => [0, 0, 255, 255],
                            _ => [128, 128, 128, 255],
                        });
                    }
                }
                writer
                    .video(&RgbaFrame::new(64, 32, pixels).unwrap(), index)
                    .unwrap();
                let mut pcm = Vec::with_capacity(1600 * 2);
                for frame in index * 1600..(index + 1) * 1600 {
                    let sample = (6553.0
                        * (std::f64::consts::TAU * 440.0 * frame as f64 / 48000.0).sin())
                    .round() as i16;
                    pcm.extend_from_slice(&[sample, sample]);
                }
                writer.audio(&pcm, index * 1600).unwrap();
            }
            writer.finalize().unwrap();
            writer.finalize().unwrap();
            drop(writer);
            let bytes = std::fs::metadata(&path).unwrap().len();
            assert!((1..=16 * 1024 * 1024).contains(&bytes));
            let mut reader =
                crate::mediafile::MediaFileReader::open(&path, Default::default()).unwrap();
            let info = reader.info().clone();
            let video = info.video.as_ref().unwrap();
            assert_eq!((video.width, video.height, video.samples), (64, 32, 30));
            assert!((999_000_000..=1_001_000_000).contains(&video.duration_ns));
            let audio = info.audio.as_ref().unwrap();
            assert_eq!((audio.sample_rate, audio.channels), (48000, 2));
            let mut frames = 0;
            while let Some(picture) = reader.next_video().unwrap() {
                assert!(frames < 30);
                assert_eq!((picture.width, picture.height), (64, 32));
                assert!(
                    (i128::from(picture.pts) - i128::from(frames) * 1_000_000_000 / 30).abs() <= 1
                );
                // Independent literal BT.709/video-range luma values at the
                // centers of red, green, blue and128gray16px bands. Quantization
                // is permitted; the expected values do not call our converter.
                for (x, expected) in [(8, 63i16), (24, 173), (40, 32), (56, 126)] {
                    assert!(
                        (i16::from(picture.y[16 * picture.y_stride + x]) - expected).abs() <= 8
                    );
                }
                frames += 1;
            }
            assert_eq!(frames, 30);
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
            assert!((46_000..=52_000).contains(&mono.len()));
            let tail = &mono[8192..];
            let amplitude = |hz: f64| {
                let (mut sin, mut cos) = (0.0, 0.0);
                for (index, value) in tail.iter().enumerate() {
                    let angle = std::f64::consts::TAU * hz * index as f64 / 48000.0;
                    sin += f64::from(*value) * angle.sin();
                    cos += f64::from(*value) * angle.cos();
                }
                2.0 * sin.hypot(cos) / tail.len() as f64
            };
            let tone = amplitude(440.0);
            assert!((0.15..=0.25).contains(&tone));
            assert!(amplitude(1000.0) < 0.01);
            receipts.push(serde_json::json!({"path":path,"bytes":bytes,"requested_prefer_hardware":prefer_hardware,"actual_encoder_identity":"not queried","decoded_video_frames":frames,"decoded_audio_frames":mono.len(),"decoded_440hz_amplitude":tone,"decoder":"independent pure Rust H264/AAC","synthetic_input":true,"native_capture":false,"audio_playback":false,"media_info":info}));
        }
        let receipt = directory.join("acceptance.json");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt)
            .unwrap();
        serde_json::to_writer_pretty(&mut file, &receipts).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
        eprintln!(
            "macOS native recording acceptance evidence: {}",
            receipt.display()
        );
    }
}
