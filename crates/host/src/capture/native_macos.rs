//! macOS 14+ ScreenCaptureKit screenshots. There is no legacy capture fallback,
//! permission request, settings launch, picker, or file-based image interchange.
//! SDK objects stay on the thread executing each callback; only owned Rust
//! metadata / pixels cross the completion channel. The worker waits until the
//! actual completion, so a delayed SDK callback remains admitted active work.

use super::super::{CaptureBackend, CaptureCapabilities, CaptureError, CaptureLimits, DisplayInfo};
use block2::RcBlock;
use objc2::{AnyThread, rc::autoreleasepool};
use objc2_core_foundation::{CFRetained, CFString, CFUUID, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGMainDisplayID, CGPreflightScreenCaptureAccess, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSArray, NSError, NSProcessInfo};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
    SCStreamErrorCode, SCStreamErrorDomain, SCWindow,
};
use skylive_core::{DisplayCaptureBinding, DisplayTarget, MAX_DISPLAY_PIXELS};
use skylive_media::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RgbaFrame, RowOrder,
    ingest_packed_frame,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    sync::mpsc,
};

// These two identity APIs are exported by ColorSync, not CoreGraphics. They
// match Apple's CFUUIDRef / uint32_t C signatures and Create ownership rule.
#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C-unwind" {
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *mut CFUUID;
    fn CGDisplayGetDisplayIDFromUUID(uuid: &CFUUID) -> u32;
}

pub(super) struct NativeBackend;
impl NativeBackend {
    pub(super) fn new() -> Self {
        Self
    }
}
impl CaptureBackend for NativeBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "macos-screencapturekit-screenshot",
            cursor: true,
            hdr: false,
            note: Some(
                "Requires macOS 14+ and existing Screen Recording permission; SDR full-display screenshots, no audio",
            ),
        }
    }
    fn enumerate(&mut self, limits: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        preflight()?;
        autoreleasepool(|_| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let completion = RcBlock::new(
                move |content: *mut SCShareableContent, error: *mut NSError| {
                    let result = callback_result(|| {
                        let content = unsafe { checked_content(content, error) }?;
                        describe_displays(content, limits)
                    });
                    let _ = sender.send(result);
                },
            );
            unsafe {
                SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &completion);
            }
            // No timeout: releasing admission while an uncancellable SDK job
            // is still pending would permit unbounded outstanding OS work.
            receiver.recv().map_err(|_| CaptureError::Disconnected)?
        })
    }
    fn capture(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        let DisplayTarget::Macos { uuid } = binding.target() else {
            return Err(CaptureError::Unavailable(
                "This display target requires a different operating system",
            ));
        };
        preflight()?;
        let uuid = uuid.clone();
        let target = binding.target().clone();
        let cursor = binding.capture_cursor();
        autoreleasepool(|_| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let completion = RcBlock::new(
                move |content: *mut SCShareableContent, error: *mut NSError| {
                    let result = callback_result(|| {
                        let content = unsafe { checked_content(content, error) }?;
                        let display_id = id_from_uuid(&uuid)?;
                        if display_id == 0 {
                            return Err(CaptureError::TargetNotFound(target.clone()));
                        }
                        let displays = unsafe { content.displays() };
                        if displays.count() > limits.max_displays.min(64) {
                            return Err(CaptureError::BudgetExceeded("Display enumeration limit"));
                        }
                        let mut found = None;
                        for index in 0..displays.count() {
                            let display = displays.objectAtIndex(index);
                            if unsafe { display.displayID() } == display_id {
                                // The numeric ID can be reused after topology changes.
                                if !uuid_for_display(display_id)?.eq_ignore_ascii_case(&uuid) {
                                    return Err(CaptureError::TargetNotFound(target.clone()));
                                }
                                found = Some(display);
                                break;
                            }
                        }
                        let display =
                            found.ok_or_else(|| CaptureError::TargetNotFound(target.clone()))?;
                        let filter = display_filter(&display);
                        let (width, height) = filter_dimensions(&filter)?;
                        admitted_bytes(width, height, limits, None)?;
                        let config = unsafe { SCStreamConfiguration::new() };
                        unsafe {
                            config.setWidth(width as usize);
                            config.setHeight(height as usize);
                            config.setShowsCursor(cursor);
                            config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
                            config.setColorSpaceName(kCGColorSpaceSRGB);
                        }
                        // SDR is the macOS 14 default. The dynamic-range setter is
                        // macOS 15+, so it must not be sent to a macOS 14 object.
                        let image_sender = sender.clone();
                        let image_completion =
                            RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
                                let result = callback_result(|| {
                                    if let Some(error) = unsafe { error.as_ref() } {
                                        return Err(sdk_error(error));
                                    }
                                    let image = unsafe { image.as_ref() }.ok_or(
                                        CaptureError::InvalidFrame(
                                            "ScreenCaptureKit returned no image",
                                        ),
                                    )?;
                                    copy_image(image, width, height, limits)
                                });
                                let _ = image_sender.send(result);
                            });
                        unsafe {
                            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&filter, &config, Some(&image_completion));
                        }
                        Ok(())
                    });
                    if let Err(error) = result {
                        let _ = sender.send(Err(error));
                    }
                },
            );
            unsafe {
                SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &completion);
            }
            receiver.recv().map_err(|_| CaptureError::Disconnected)?
        })
    }
}

fn preflight() -> Result<(), CaptureError> {
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        < 14
    {
        return Err(CaptureError::Unavailable(
            "ScreenCaptureKit screenshots require macOS 14 or later",
        ));
    }
    if !CGPreflightScreenCaptureAccess() {
        return Err(CaptureError::PermissionDenied);
    }
    Ok(())
}
fn callback_result<T>(
    operation: impl FnOnce() -> Result<T, CaptureError>,
) -> Result<T, CaptureError> {
    // Rust unwinding must not cross an Objective-C block ABI.
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or_else(|_| {
        Err(CaptureError::Backend(
            "Rust panic in ScreenCaptureKit completion".into(),
        ))
    })
}
// Safety: pointers originate from the current SDK completion and the chosen
// lifetime cannot extend beyond that completion's invocation.
unsafe fn checked_content<'a>(
    content: *mut SCShareableContent,
    error: *mut NSError,
) -> Result<&'a SCShareableContent, CaptureError> {
    if let Some(error) = unsafe { error.as_ref() } {
        return Err(sdk_error(error));
    }
    unsafe { content.as_ref() }.ok_or_else(|| {
        CaptureError::Backend("ScreenCaptureKit returned no shareable content".into())
    })
}
fn sdk_error(error: &NSError) -> CaptureError {
    if error.code() == SCStreamErrorCode::UserDeclined.0
        && &*error.domain() == unsafe { SCStreamErrorDomain }
    {
        CaptureError::PermissionDenied
    } else {
        CaptureError::Backend(format!(
            "ScreenCaptureKit {} ({}): {}",
            &*error.domain(),
            error.code(),
            &*error.localizedDescription()
        ))
    }
}
fn uuid_for_display(display: u32) -> Result<String, CaptureError> {
    let raw = NonNull::new(unsafe { CGDisplayCreateUUIDFromDisplayID(display) })
        .ok_or_else(|| CaptureError::Backend("ColorSync returned no display UUID".into()))?;
    let uuid = unsafe { CFRetained::from_raw(raw) };
    CFUUID::new_string(None, Some(&uuid))
        .map(|s| s.to_string())
        .ok_or_else(|| CaptureError::Backend("Cannot format display UUID".into()))
}
fn id_from_uuid(uuid: &str) -> Result<u32, CaptureError> {
    let value = CFString::from_str(uuid);
    let uuid = CFUUID::from_string(None, Some(&value))
        .ok_or(CaptureError::InvalidFrame("Invalid display UUID"))?;
    Ok(unsafe { CGDisplayGetDisplayIDFromUUID(&uuid) })
}
fn display_filter(display: &SCDisplay) -> objc2::rc::Retained<SCContentFilter> {
    let excluded = NSArray::<SCWindow>::from_slice(&[]);
    unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            display,
            &excluded,
        )
    }
}
fn pixel_dimension(points: f64, scale: f32) -> Result<u32, CaptureError> {
    let pixels = (points * f64::from(scale)).round();
    if !points.is_finite()
        || !scale.is_finite()
        || points <= 0.0
        || scale <= 0.0
        || !(1.0..=8192.0).contains(&pixels)
    {
        return Err(CaptureError::InvalidFrame(
            "Invalid ScreenCaptureKit native pixel dimensions",
        ));
    }
    Ok(pixels as u32)
}
fn filter_dimensions(filter: &SCContentFilter) -> Result<(u32, u32), CaptureError> {
    let rect = unsafe { filter.contentRect() };
    let scale = unsafe { filter.pointPixelScale() };
    Ok((
        pixel_dimension(rect.size.width, scale)?,
        pixel_dimension(rect.size.height, scale)?,
    ))
}
fn describe_displays(
    content: &SCShareableContent,
    limits: CaptureLimits,
) -> Result<Vec<DisplayInfo>, CaptureError> {
    let displays = unsafe { content.displays() };
    if displays.count() > limits.max_displays.min(64) {
        return Err(CaptureError::BudgetExceeded("Display enumeration limit"));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(displays.count())
        .map_err(|_| CaptureError::BudgetExceeded("Display metadata allocation"))?;
    let primary = CGMainDisplayID();
    for index in 0..displays.count() {
        let display = displays.objectAtIndex(index);
        let id = unsafe { display.displayID() };
        let uuid = uuid_for_display(id)?;
        let filter = display_filter(&display);
        let (width, height) = filter_dimensions(&filter)?;
        // SCK supplies no display friendly-name API. Do not invent an AppKit
        // main-thread dependency just to label an otherwise background query.
        let name = format!("Display {width} × {height} ({uuid})");
        output.push(DisplayInfo {
            target: DisplayTarget::Macos { uuid },
            name,
            width,
            height,
            primary: id == primary,
        });
    }
    Ok(output)
}
fn admitted_bytes(
    width: u32,
    height: u32,
    limits: CaptureLimits,
    sdk_stride: Option<usize>,
) -> Result<usize, CaptureError> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > MAX_DISPLAY_PIXELS
    {
        return Err(CaptureError::InvalidFrame(
            "Display dimensions exceed 8192-edge/32M-pixel limits",
        ));
    }
    let stride = (width as usize)
        .checked_mul(4)
        .ok_or(CaptureError::BudgetExceeded("Display row overflow"))?;
    let bytes = stride
        .checked_mul(height as usize)
        .ok_or(CaptureError::BudgetExceeded("Display frame overflow"))?;
    let native_stride = sdk_stride.unwrap_or(stride);
    if native_stride < stride {
        return Err(CaptureError::InvalidFrame(
            "ScreenCaptureKit returned a short image stride",
        ));
    }
    // Keep all three simultaneously-live, cooperative buffers admitted: SDK
    // image, sRGB bitmap conversion, and the owned media-frame copy.
    let work = native_stride
        .checked_mul(height as usize)
        .and_then(|n| bytes.checked_mul(2).and_then(|b| n.checked_add(b)));
    if bytes > limits.max_frame_bytes || work.is_none_or(|n| n > limits.max_raw_bytes) {
        return Err(CaptureError::BudgetExceeded(
            "ScreenCaptureKit image, color conversion and RGBA buffers",
        ));
    }
    Ok(bytes)
}
fn copy_image(
    image: &CGImage,
    expected_width: u32,
    expected_height: u32,
    limits: CaptureLimits,
) -> Result<RgbaFrame, CaptureError> {
    let width = u32::try_from(CGImage::width(Some(image)))
        .map_err(|_| CaptureError::InvalidFrame("Screenshot width overflow"))?;
    let height = u32::try_from(CGImage::height(Some(image)))
        .map_err(|_| CaptureError::InvalidFrame("Screenshot height overflow"))?;
    if (width, height) != (expected_width, expected_height) {
        return Err(CaptureError::InvalidFrame(
            "Display topology changed during ScreenCaptureKit screenshot",
        ));
    }
    if CGImage::bits_per_component(Some(image)) != 8 || CGImage::bits_per_pixel(Some(image)) != 32 {
        return Err(CaptureError::InvalidFrame(
            "Expected an SDR 8-bit ScreenCaptureKit screenshot",
        ));
    }
    let bytes = admitted_bytes(
        width,
        height,
        limits,
        Some(CGImage::bytes_per_row(Some(image))),
    )?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes)
        .map_err(|_| CaptureError::BudgetExceeded("Screenshot conversion allocation"))?;
    rgba.resize(bytes, 0);
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| CaptureError::Backend("Cannot create sRGB screenshot color space".into()))?;
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width as usize,
            height as usize,
            8,
            width as usize * 4,
            Some(&space),
            CGImageAlphaInfo::NoneSkipLast.0 | CGImageByteOrderInfo::Order32Big.0,
        )
    }
    .ok_or_else(|| CaptureError::Backend("Cannot create sRGB screenshot bitmap context".into()))?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(f64::from(width), f64::from(height)),
        ),
        Some(image),
    );
    CGContext::flush(Some(&context));
    drop(context); // CoreGraphics must stop borrowing the Vec before ingestion.
    ingest_packed_frame(
        &rgba,
        PackedFrameLayout {
            width,
            height,
            stride_bytes: width as usize * 4,
            format: PackedPixelFormat::Rgba8,
            row_order: RowOrder::TopDown,
            alpha: AlphaMode::Opaque,
        },
        PackedFrameLimits {
            max_input_bytes: limits.max_raw_bytes,
            max_output_bytes: limits.max_frame_bytes,
            max_pixel_visits: (bytes / 4) as u64 * 2,
        },
    )
    .map_err(|_| CaptureError::InvalidFrame("ScreenCaptureKit packed frame ingestion"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pixel_dimensions_use_capture_points_times_retina_scale() {
        assert_eq!(pixel_dimension(1512.0, 2.0).unwrap(), 3024);
        for (p, s) in [
            (0.0, 2.0),
            (1.0, 0.0),
            (f64::NAN, 1.0),
            (1.0, f32::INFINITY),
            (8193.0, 1.0),
        ] {
            assert!(pixel_dimension(p, s).is_err());
        }
    }
    #[test]
    fn screenshot_admission_counts_sdk_stride_and_two_owned_buffers() {
        let mut limits = CaptureLimits {
            max_frame_bytes: 64,
            max_raw_bytes: 192,
            ..CaptureLimits::default()
        };
        assert_eq!(admitted_bytes(4, 4, limits, None).unwrap(), 64);
        assert!(admitted_bytes(4, 4, limits, Some(20)).is_err());
        limits.max_raw_bytes = 208;
        assert_eq!(admitted_bytes(4, 4, limits, Some(20)).unwrap(), 64);
        assert!(admitted_bytes(4, 4, limits, Some(15)).is_err());
    }
    #[test]
    fn native_5k_6k_and_8k_screenshots_fit_three_buffer_profile_without_allocation() {
        let limits = CaptureLimits::default();
        for (width, height, bytes) in [
            (5120, 2880, 58_982_400),
            (6016, 3384, 81_432_576),
            (7680, 4320, 132_710_400),
            (8192, 4096, 134_217_728),
        ] {
            let stride = width as usize * 4;
            assert_eq!(
                admitted_bytes(width, height, limits, Some(stride)).unwrap(),
                bytes
            );
            let tight = CaptureLimits {
                max_raw_bytes: bytes * 3 - 1,
                ..limits
            };
            assert!(matches!(
                admitted_bytes(width, height, tight, Some(stride)),
                Err(CaptureError::BudgetExceeded(_))
            ));
        }
        assert!(matches!(
            admitted_bytes(8192, 4097, limits, None),
            Err(CaptureError::InvalidFrame(_))
        ));
        assert!(matches!(
            admitted_bytes(8193, 1, limits, None),
            Err(CaptureError::InvalidFrame(_))
        ));
    }
}
