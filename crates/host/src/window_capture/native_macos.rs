//! macOS 14+ ScreenCaptureKit window screenshot adapter, no permission prompt.
use super::super::{MAX_WINDOWS, WindowCaptureBackend, WindowCaptureError, WindowInfo};
use crate::capture::{CaptureCapabilities, CaptureLimits};
use block2::RcBlock;
use objc2::{AnyThread, rc::autoreleasepool};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGPreflightScreenCaptureAccess, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSError, NSProcessInfo};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
    SCStreamErrorCode, SCStreamErrorDomain, SCWindow,
};
use skylive_core::{MAX_DISPLAY_PIXELS, WindowCaptureBinding, WindowMatchPriority, WindowTarget};
use skylive_media::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RgbaFrame, RowOrder,
    ingest_packed_frame,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc,
};
pub(super) struct NativeBackend;
impl NativeBackend {
    pub(super) fn new() -> Self {
        Self
    }
}
impl WindowCaptureBackend for NativeBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "macos-screencapturekit-window",
            cursor: true,
            hdr: false,
            note: Some(
                "macOS 14+, existing Screen Recording permission; exact bundle/title selector; full desktop-independent window, no audio",
            ),
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<WindowInfo>, WindowCaptureError> {
        preflight()?;
        autoreleasepool(|_| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let completion = RcBlock::new(
                move |content: *mut SCShareableContent, error: *mut NSError| {
                    let result = callback_result(|| {
                        let content = unsafe { checked_content(content, error) }?;
                        let windows = unsafe { content.windows() };
                        if windows.count() > MAX_WINDOWS {
                            return Err(WindowCaptureError::BudgetExceeded(
                                "more than 1024 windows",
                            ));
                        }
                        let mut metadata_bytes = 0usize;
                        let mut out = Vec::new();
                        out.try_reserve_exact(windows.count())
                            .map_err(|_| WindowCaptureError::BudgetExceeded("window metadata"))?;
                        for i in 0..windows.count() {
                            let window = windows.objectAtIndex(i);
                            if !unsafe { window.isOnScreen() } {
                                continue;
                            }
                            if let Some(info) = describe_window(&window)? {
                                let cost = match &info.target {
                                    WindowTarget::Macos { title, bundle_id } => {
                                        title.len() + bundle_id.len() + info.name.len()
                                    }
                                    _ => 0,
                                };
                                metadata_bytes = metadata_bytes.checked_add(cost).ok_or(
                                    WindowCaptureError::BudgetExceeded("window metadata overflow"),
                                )?;
                                if metadata_bytes > 4 * 1024 * 1024 {
                                    return Err(WindowCaptureError::BudgetExceeded(
                                        "window metadata exceeds 4MiB",
                                    ));
                                }
                                out.push(info);
                            }
                        }
                        Ok(out)
                    });
                    let _ = sender.send(result);
                },
            );
            unsafe {
                SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true,false,&completion);
            }
            receiver
                .recv()
                .map_err(|_| WindowCaptureError::Disconnected)?
        })
    }
    fn capture(
        &mut self,
        binding: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, WindowCaptureError> {
        let WindowTarget::Macos { .. } = binding.target() else {
            return Err(WindowCaptureError::Unavailable(
                "window target requires another OS",
            ));
        };
        if binding.client_area() {
            return Err(WindowCaptureError::Unavailable(
                "ScreenCaptureKit captures complete windows; client-only crop is not implemented",
            ));
        }
        if binding.priority() != WindowMatchPriority::Title {
            return Err(WindowCaptureError::Unavailable(
                "macOS windows require exact title and bundle identity",
            ));
        }
        preflight()?;
        let target = binding.target().clone();
        let cursor = binding.capture_cursor();
        autoreleasepool(|_| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let completion = RcBlock::new(
                move |content: *mut SCShareableContent, error: *mut NSError| {
                    let result = callback_result(|| {
                        let content = unsafe { checked_content(content, error) }?;
                        let windows = unsafe { content.windows() };
                        if windows.count() > MAX_WINDOWS {
                            return Err(WindowCaptureError::BudgetExceeded(
                                "more than 1024 windows",
                            ));
                        }
                        let mut found = None;
                        for i in 0..windows.count() {
                            let window = windows.objectAtIndex(i);
                            if describe_window(&window)?.is_some_and(|info| info.target == target) {
                                if found.is_some() {
                                    return Err(WindowCaptureError::AmbiguousTarget);
                                }
                                found = Some(window);
                            }
                        }
                        let window = found.ok_or(WindowCaptureError::TargetNotFound)?;
                        if !unsafe { window.isOnScreen() } {
                            return Err(WindowCaptureError::TargetHidden);
                        }
                        let filter = unsafe {
                            SCContentFilter::initWithDesktopIndependentWindow(
                                SCContentFilter::alloc(),
                                &window,
                            )
                        };
                        let (width, height) = filter_dimensions(&filter)?;
                        admitted_bytes(width, height, limits, None)?;
                        let config = unsafe { SCStreamConfiguration::new() };
                        unsafe {
                            config.setWidth(width as usize);
                            config.setHeight(height as usize);
                            config.setShowsCursor(cursor);
                            config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
                            config.setColorSpaceName(kCGColorSpaceSRGB);
                            config.setIgnoreShadowsSingleWindow(true);
                        }
                        let image_sender = sender.clone();
                        let image_completion =
                            RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
                                let result = callback_result(|| {
                                    if let Some(error) = unsafe { error.as_ref() } {
                                        return Err(sdk_error(error));
                                    }
                                    let image = unsafe { image.as_ref() }.ok_or(
                                        WindowCaptureError::InvalidFrame("no screenshot image"),
                                    )?;
                                    copy_image(image, width, height, limits)
                                });
                                let _ = image_sender.send(result);
                            });
                        unsafe {
                            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&filter,&config,Some(&image_completion));
                        }
                        Ok(())
                    });
                    if let Err(error) = result {
                        let _ = sender.send(Err(error));
                    }
                },
            );
            unsafe {
                SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true,false,&completion);
            }
            receiver
                .recv()
                .map_err(|_| WindowCaptureError::Disconnected)?
        })
    }
}
fn describe_window(window: &SCWindow) -> Result<Option<WindowInfo>, WindowCaptureError> {
    let Some(app) = (unsafe { window.owningApplication() }) else {
        return Ok(None);
    };
    let Some(title) = (unsafe { window.title() }) else {
        return Ok(None);
    };
    let title = title.to_string();
    let bundle_id = unsafe { app.bundleIdentifier() }.to_string();
    if title.trim().is_empty() || bundle_id.trim().is_empty() {
        return Ok(None);
    }
    let target = WindowTarget::Macos {
        title: title.clone(),
        bundle_id: bundle_id.clone(),
    };
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
    };
    let (width, height) = filter_dimensions(&filter)?;
    WindowCaptureBinding::new(
        target.clone(),
        WindowMatchPriority::Title,
        true,
        false,
        width,
        height,
    )
    .map_err(|_| WindowCaptureError::InvalidFrame("native window identity"))?;
    Ok(Some(WindowInfo {
        full_window_size: None,
        target,
        name: format!("[{bundle_id}]: {title}"),
        width,
        height,
        minimized: false,
    }))
}
fn preflight() -> Result<(), WindowCaptureError> {
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        < 14
    {
        return Err(WindowCaptureError::Unavailable(
            "ScreenCaptureKit screenshots require macOS 14 or later",
        ));
    }
    if !CGPreflightScreenCaptureAccess() {
        return Err(WindowCaptureError::PermissionDenied);
    }
    Ok(())
}
fn callback_result<T>(
    operation: impl FnOnce() -> Result<T, WindowCaptureError>,
) -> Result<T, WindowCaptureError> {
    // Rust unwinding must not cross an Objective-C block ABI.
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or_else(|_| {
        Err(WindowCaptureError::Backend(
            "Rust panic in ScreenCaptureKit completion".into(),
        ))
    })
}
// Safety: pointers originate from the current SDK completion and the chosen
// lifetime cannot extend beyond that completion's invocation.
unsafe fn checked_content<'a>(
    content: *mut SCShareableContent,
    error: *mut NSError,
) -> Result<&'a SCShareableContent, WindowCaptureError> {
    if let Some(error) = unsafe { error.as_ref() } {
        return Err(sdk_error(error));
    }
    unsafe { content.as_ref() }.ok_or_else(|| {
        WindowCaptureError::Backend("ScreenCaptureKit returned no shareable content".into())
    })
}
fn sdk_error(error: &NSError) -> WindowCaptureError {
    if error.code() == SCStreamErrorCode::UserDeclined.0
        && &*error.domain() == unsafe { SCStreamErrorDomain }
    {
        WindowCaptureError::PermissionDenied
    } else {
        WindowCaptureError::Backend(format!(
            "ScreenCaptureKit {} ({}): {}",
            &*error.domain(),
            error.code(),
            &*error.localizedDescription()
        ))
    }
}
fn pixel_dimension(points: f64, scale: f32) -> Result<u32, WindowCaptureError> {
    let pixels = (points * f64::from(scale)).round();
    if !points.is_finite()
        || !scale.is_finite()
        || points <= 0.0
        || scale <= 0.0
        || !(1.0..=8192.0).contains(&pixels)
    {
        return Err(WindowCaptureError::InvalidFrame(
            "Invalid ScreenCaptureKit native pixel dimensions",
        ));
    }
    Ok(pixels as u32)
}
fn filter_dimensions(filter: &SCContentFilter) -> Result<(u32, u32), WindowCaptureError> {
    let rect = unsafe { filter.contentRect() };
    let scale = unsafe { filter.pointPixelScale() };
    Ok((
        pixel_dimension(rect.size.width, scale)?,
        pixel_dimension(rect.size.height, scale)?,
    ))
}
fn admitted_bytes(
    width: u32,
    height: u32,
    limits: CaptureLimits,
    sdk_stride: Option<usize>,
) -> Result<usize, WindowCaptureError> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > MAX_DISPLAY_PIXELS
    {
        return Err(WindowCaptureError::InvalidFrame(
            "Window dimensions exceed 8192-edge/32M-pixel limits",
        ));
    }
    let stride = (width as usize)
        .checked_mul(4)
        .ok_or(WindowCaptureError::BudgetExceeded("Display row overflow"))?;
    let bytes = stride
        .checked_mul(height as usize)
        .ok_or(WindowCaptureError::BudgetExceeded("Display frame overflow"))?;
    let native_stride = sdk_stride.unwrap_or(stride);
    if native_stride < stride {
        return Err(WindowCaptureError::InvalidFrame(
            "ScreenCaptureKit returned a short image stride",
        ));
    }
    // Keep all three simultaneously-live, cooperative buffers admitted: SDK
    // image, sRGB bitmap conversion, and the owned media-frame copy.
    let work = native_stride
        .checked_mul(height as usize)
        .and_then(|n| bytes.checked_mul(2).and_then(|b| n.checked_add(b)));
    if bytes > limits.max_frame_bytes || work.is_none_or(|n| n > limits.max_raw_bytes) {
        return Err(WindowCaptureError::BudgetExceeded(
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
) -> Result<RgbaFrame, WindowCaptureError> {
    let width = u32::try_from(CGImage::width(Some(image)))
        .map_err(|_| WindowCaptureError::InvalidFrame("Screenshot width overflow"))?;
    let height = u32::try_from(CGImage::height(Some(image)))
        .map_err(|_| WindowCaptureError::InvalidFrame("Screenshot height overflow"))?;
    if (width, height) != (expected_width, expected_height) {
        return Err(WindowCaptureError::InvalidFrame(
            "Window geometry changed during ScreenCaptureKit screenshot",
        ));
    }
    if CGImage::bits_per_component(Some(image)) != 8 || CGImage::bits_per_pixel(Some(image)) != 32 {
        return Err(WindowCaptureError::InvalidFrame(
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
        .map_err(|_| WindowCaptureError::BudgetExceeded("Screenshot conversion allocation"))?;
    rgba.resize(bytes, 0);
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or_else(|| {
        WindowCaptureError::Backend("Cannot create sRGB screenshot color space".into())
    })?;
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width as usize,
            height as usize,
            8,
            width as usize * 4,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0,
        )
    }
    .ok_or_else(|| {
        WindowCaptureError::Backend("Cannot create sRGB screenshot bitmap context".into())
    })?;
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
            alpha: AlphaMode::Premultiplied,
        },
        PackedFrameLimits {
            max_input_bytes: limits.max_raw_bytes,
            max_output_bytes: limits.max_frame_bytes,
            max_pixel_visits: (bytes / 4) as u64 * 2,
        },
    )
    .map_err(|_| WindowCaptureError::InvalidFrame("ScreenCaptureKit packed frame ingestion"))
}
