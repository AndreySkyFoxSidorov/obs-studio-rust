//! Opt-in controlled native acceptance. Captures only a uniquely named window
//! created by this test; never requests another application's pixels.
#[cfg(target_os = "windows")]
mod windows {
    use skylive_core::{Command, Session, WindowCaptureMethod, WindowMatchPriority};
    use skylive_host::{
        capture::{self, CaptureEvent, CaptureWorker},
        transition::FrameScene,
    };
    use skylive_media::MediaTime;
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, PAINTSTRUCT,
                UpdateWindow,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, SetThreadDpiAwarenessContext},
                WindowsAndMessaging::{
                    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
                    DispatchMessageW, GetClientRect, GetMessageW, HWND_TOPMOST, MSG,
                    PostQuitMessage, RegisterClassW, SW_HIDE, SW_MINIMIZE, SW_RESTORE,
                    SWP_NOACTIVATE, SWP_NOMOVE, SendMessageW, SetWindowPos, ShowWindow,
                    TranslateMessage, UnregisterClassW, WM_APP, WM_CLOSE, WM_DESTROY, WM_PAINT,
                    WNDCLASSW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
                },
            },
        },
        core::PCWSTR,
    };
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    const RESIZE: u32 = WM_APP + 1;
    const HIDE: u32 = WM_APP + 2;
    const RESTORE: u32 = WM_APP + 3;
    const MINIMIZE: u32 = WM_APP + 4;
    #[derive(Clone)]
    struct Identity {
        class: String,
        title: String,
    }
    struct Fixture {
        identity: Identity,
        hwnd: isize,
        done: mpsc::Receiver<()>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Fixture {
        fn new() -> Self {
            let id = SERIAL.fetch_add(1, Ordering::SeqCst);
            Self::with_identity(Identity {
                class: format!("SkyLiveOwnedCaptureFixture{}_{}", std::process::id(), id),
                title: format!("SkyLive controlled window {} {}", std::process::id(), id),
            })
        }
        fn with_identity(identity: Identity) -> Self {
            let (ready, received) = mpsc::sync_channel(1);
            let (done, finished) = mpsc::sync_channel(1);
            let data = identity.clone();
            let handle = thread::spawn(move || {
                // Fixed physical pixel dimensions independent of desktop scaling.
                let previous = unsafe {
                    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE)
                };
                let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap();
                let instance = HINSTANCE(module.0);
                let class: Vec<u16> = data.class.encode_utf16().chain(Some(0)).collect();
                let title: Vec<u16> = data.title.encode_utf16().chain(Some(0)).collect();
                let atom = unsafe {
                    RegisterClassW(&WNDCLASSW {
                        style: CS_HREDRAW | CS_VREDRAW,
                        lpfnWndProc: Some(procedure),
                        hInstance: instance,
                        lpszClassName: PCWSTR(class.as_ptr()),
                        ..Default::default()
                    })
                };
                assert_ne!(atom, 0);
                let hwnd = unsafe {
                    CreateWindowExW(
                        WS_EX_TOPMOST,
                        PCWSTR(class.as_ptr()),
                        PCWSTR(title.as_ptr()),
                        WS_POPUP | WS_VISIBLE,
                        64,
                        64,
                        320,
                        240,
                        None,
                        None,
                        instance,
                        None,
                    )
                }
                .unwrap();
                unsafe {
                    let _ = UpdateWindow(hwnd);
                }
                ready.send(hwnd.0 as isize).unwrap();
                let mut message = MSG::default();
                loop {
                    let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
                    if result <= 0 {
                        break;
                    }
                    unsafe {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                unsafe {
                    let _ = UnregisterClassW(PCWSTR(class.as_ptr()), instance);
                    let _ = SetThreadDpiAwarenessContext(previous);
                }
                let _ = done.send(());
            });
            let hwnd = received
                .recv_timeout(Duration::from_secs(5))
                .expect("owned fixture window ready");
            Self {
                identity,
                hwnd,
                done: finished,
                thread: Some(handle),
            }
        }
        fn command(&self, message: u32, parameter: isize) {
            unsafe {
                SendMessageW(
                    HWND(self.hwnd as *mut _),
                    message,
                    WPARAM(0),
                    LPARAM(parameter),
                );
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.command(WM_CLOSE, 0);
            self.done
                .recv_timeout(Duration::from_secs(5))
                .expect("owned fixture close acknowledged");
            if let Some(handle) = self.thread.take() {
                handle.join().unwrap();
            }
        }
    }
    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = unsafe { BeginPaint(hwnd, &mut paint) };
                let mut area = RECT::default();
                if unsafe { GetClientRect(hwnd, &mut area) }.is_ok() {
                    let midx = area.right / 2;
                    let midy = area.bottom / 2;
                    for (rect, color) in [
                        (
                            RECT {
                                left: 0,
                                top: 0,
                                right: midx,
                                bottom: midy,
                            },
                            0x000000ff,
                        ),
                        (
                            RECT {
                                left: midx,
                                top: 0,
                                right: area.right,
                                bottom: midy,
                            },
                            0x0000ff00,
                        ),
                        (
                            RECT {
                                left: 0,
                                top: midy,
                                right: midx,
                                bottom: area.bottom,
                            },
                            0x00ff0000,
                        ),
                        (
                            RECT {
                                left: midx,
                                top: midy,
                                right: area.right,
                                bottom: area.bottom,
                            },
                            0x00ffffff,
                        ),
                    ] {
                        let brush = unsafe { CreateSolidBrush(COLORREF(color)) };
                        if !brush.is_invalid() {
                            unsafe {
                                let _ = FillRect(dc, &rect, brush);
                                let _ = DeleteObject(brush);
                            }
                        }
                    }
                }
                unsafe {
                    let _ = EndPaint(hwnd, &paint);
                }
                LRESULT(0)
            }
            RESIZE => {
                let width = (lparam.0 & 0xffff) as i32;
                let height = ((lparam.0 >> 16) & 0xffff) as i32;
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        0,
                        0,
                        width,
                        height,
                        SWP_NOMOVE | SWP_NOACTIVATE,
                    );
                    let _ = UpdateWindow(hwnd);
                }
                LRESULT(0)
            }
            HIDE => {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                LRESULT(0)
            }
            RESTORE => {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = UpdateWindow(hwnd);
                }
                LRESULT(0)
            }
            MINIMIZE => {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }
    fn capture(worker: &mut CaptureWorker, session: &Session, time: u64) {
        assert!(worker.request_frame(session, MediaTime(time)).unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.is_busy() {
            let _ = worker.poll(session, MediaTime(time)).unwrap();
            assert!(Instant::now() < deadline, "owned window capture completion");
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn assert_quadrants(worker: &CaptureWorker, source: skylive_core::Id, width: u32, height: u32) {
        let snapshot = worker.snapshot();
        assert!(snapshot.problems().is_empty(), "{:?}", snapshot.problems());
        let frame = &snapshot.frame(source).unwrap().image;
        assert_eq!((frame.width(), frame.height()), (width, height));
        for (x, y, expected) in [
            (width / 4, height / 4, [255, 0, 0, 255]),
            (width * 3 / 4, height / 4, [0, 255, 0, 255]),
            (width / 4, height * 3 / 4, [0, 0, 255, 255]),
            (width * 3 / 4, height * 3 / 4, [255, 255, 255, 255]),
        ] {
            let offset = ((y * width + x) * 4) as usize;
            assert_eq!(&frame.pixels()[offset..offset + 4], &expected);
        }
    }
    fn ready(
        worker: &mut CaptureWorker,
        session: &Session,
        source: skylive_core::Id,
        size: (u32, u32),
        clock: &mut u64,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            capture(worker, session, *clock);
            *clock = clock.checked_add(1_000_000_000).unwrap();
            let snapshot = worker.snapshot();
            if snapshot.problems().is_empty()
                && snapshot
                    .frame(source)
                    .is_some_and(|f| (f.image.width(), f.image.height()) == size)
            {
                assert_eq!(
                    snapshot.frame(source).unwrap().window.unwrap().backend,
                    skylive_host::window_capture::WindowCaptureBackendKind::Wgc
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "actual WGC frame deadline: {:?}",
                snapshot.problems()
            );
            drop(snapshot);
            thread::sleep(Duration::from_millis(15));
        }
    }
    #[test]
    #[ignore = "explicit owned-window WGC acceptance; no other application's pixels"]
    fn real_wgc_owned_pixels_pending_resize_hide_close_recreate_and_method_switch() {
        let fixture = Fixture::new();
        let identity = fixture.identity.clone();
        let mut session = Session::new();
        let scene = session.preview();
        let mut worker = CaptureWorker::new(capture::native::backend()).unwrap();
        let key = worker.request_windows(&session).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let windows = loop {
            if let Some(CaptureEvent::Windows {
                key: actual,
                result,
            }) = worker.poll(&session, MediaTime(0)).unwrap()
            {
                assert_eq!(actual, key);
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        let own=windows.into_iter().find(|w|matches!(&w.target,skylive_core::WindowTarget::Windows{title,class,..}if title==&identity.title&&class==&identity.class)).expect("owned fixture only");
        let binding = own
            .binding(WindowMatchPriority::Title, false, true)
            .unwrap()
            .with_method(WindowCaptureMethod::Wgc)
            .unwrap();
        session
            .apply(Command::AddWindowCaptureSource {
                scene,
                name: "Owned WGC fixture".into(),
                binding: binding.clone(),
            })
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        worker
            .synchronize(&session, &[FrameScene::Live(scene)])
            .unwrap();
        let mut clock = 0;
        ready(&mut worker, &session, source, (320, 240), &mut clock);
        assert_quadrants(&worker, source, 320, 240);
        let first_generation = worker
            .snapshot()
            .frame(source)
            .unwrap()
            .window
            .unwrap()
            .generation;
        fixture.command(RESIZE, 400 | (300 << 16));
        ready(&mut worker, &session, source, (400, 300), &mut clock);
        assert_quadrants(&worker, source, 400, 300);
        assert_ne!(
            worker
                .snapshot()
                .frame(source)
                .unwrap()
                .window
                .unwrap()
                .generation,
            first_generation
        );
        // Popup fixture has no nonclient border: full-window and client geometry
        // are exactly equal, so their independent pixel oracle remains literal.
        // Decorated/translucent windows are a separate acceptance gate.
        session
            .apply(Command::SetWindowCaptureSource {
                source,
                binding: skylive_core::WindowCaptureBinding::new(
                    binding.target().clone(),
                    binding.priority(),
                    false,
                    false,
                    320,
                    240,
                )
                .unwrap()
                .with_method(WindowCaptureMethod::Wgc)
                .unwrap(),
            })
            .unwrap();
        worker
            .synchronize(&session, &[FrameScene::Live(scene)])
            .unwrap();
        ready(&mut worker, &session, source, (400, 300), &mut clock);
        assert_quadrants(&worker, source, 400, 300);
        fixture.command(HIDE, 0);
        capture(&mut worker, &session, clock);
        clock += 1_000_000_000;
        assert!(worker.snapshot().frame(source).is_none());
        assert_eq!(
            worker.snapshot().problem(source).unwrap().code,
            "window_capture_target_hidden"
        );
        fixture.command(RESTORE, 0);
        ready(&mut worker, &session, source, (400, 300), &mut clock);
        fixture.command(MINIMIZE, 0);
        capture(&mut worker, &session, clock);
        clock += 1_000_000_000;
        assert!(worker.snapshot().frame(source).is_none());
        assert_eq!(
            worker.snapshot().problem(source).unwrap().code,
            "window_capture_target_minimized"
        );
        fixture.command(RESTORE, 0);
        ready(&mut worker, &session, source, (400, 300), &mut clock);
        let old_generation = worker
            .snapshot()
            .frame(source)
            .unwrap()
            .window
            .unwrap()
            .generation;
        drop(fixture);
        capture(&mut worker, &session, clock);
        clock += 1_000_000_000;
        assert!(worker.snapshot().frame(source).is_none());
        let replacement = Fixture::with_identity(identity);
        ready(&mut worker, &session, source, (320, 240), &mut clock);
        assert_quadrants(&worker, source, 320, 240);
        assert_ne!(
            worker
                .snapshot()
                .frame(source)
                .unwrap()
                .window
                .unwrap()
                .generation,
            old_generation
        );
        session
            .apply(Command::SetWindowCaptureSource {
                source,
                binding: binding
                    .clone()
                    .with_method(WindowCaptureMethod::Gdi)
                    .unwrap(),
            })
            .unwrap();
        worker
            .synchronize(&session, &[FrameScene::Live(scene)])
            .unwrap();
        capture(&mut worker, &session, clock);
        assert_quadrants(&worker, source, 320, 240);
        assert_eq!(
            worker
                .snapshot()
                .frame(source)
                .unwrap()
                .window
                .unwrap()
                .backend,
            skylive_host::window_capture::WindowCaptureBackendKind::Gdi
        );
        worker.join().unwrap();
        drop(replacement);
        println!(
            "Real WGC owned opaque popup pixel/resize/hide/minimize/close/recreate/full-client/method-switch passed; no other application pixels requested; decorated/translucent/color/HDR not measured by this fixture"
        );
    }
}
