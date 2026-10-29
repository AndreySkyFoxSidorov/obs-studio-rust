//! Opt-in controlled native acceptance. Captures only a uniquely named window
//! created by this test; never requests another application's pixels.
#[cfg(target_os = "windows")]
mod windows {
    use skylive_core::{Command, Session, WindowMatchPriority};
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
    #[test]
    #[ignore = "explicit controlled native Windows capture; creates only its own quadrant test window"]
    fn owned_window_pixels_resize_hidden_minimized_closed_and_recreated_are_exact() {
        let fixture = Fixture::new();
        let identity = fixture.identity.clone();
        let mut session = Session::new();
        let scene = session.preview();
        let mut worker = CaptureWorker::new(capture::native::backend()).unwrap();
        assert!(worker.window_capabilities().available);
        let key = worker.request_windows(&session).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let windows = loop {
            if let Some(CaptureEvent::Windows {
                key: actual,
                result,
            }) = worker.poll(&session, MediaTime(0)).unwrap()
            {
                assert_eq!(key, actual);
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        let own=windows.into_iter().find(|w|matches!(&w.target,skylive_core::WindowTarget::Windows{title,class,..}if title==&identity.title&&class==&identity.class)).expect("only the owned fixture target");
        let binding = own
            .binding(WindowMatchPriority::Title, false, true)
            .unwrap();
        let item = session
            .apply(Command::AddWindowCaptureSource {
                scene,
                name: "Owned native fixture".into(),
                binding,
            })
            .unwrap()
            .created
            .unwrap();
        let source = session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .find(|i| i.id() == item)
            .unwrap()
            .source();
        worker
            .synchronize(&session, &[FrameScene::Live(scene)])
            .unwrap();
        capture(&mut worker, &session, 0);
        assert_quadrants(&worker, source, 320, 240);
        fixture.command(RESIZE, 400 | (300 << 16));
        capture(&mut worker, &session, 1_000_000_000);
        assert_quadrants(&worker, source, 400, 300);
        fixture.command(HIDE, 0);
        capture(&mut worker, &session, 2_000_000_000);
        assert_eq!(
            worker.snapshot().problem(source).unwrap().code,
            "window_capture_target_hidden"
        );
        fixture.command(RESTORE, 0);
        capture(&mut worker, &session, 3_000_000_000);
        assert_quadrants(&worker, source, 400, 300);
        fixture.command(MINIMIZE, 0);
        capture(&mut worker, &session, 4_000_000_000);
        assert_eq!(
            worker.snapshot().problem(source).unwrap().code,
            "window_capture_target_minimized"
        );
        fixture.command(RESTORE, 0);
        capture(&mut worker, &session, 5_000_000_000);
        assert_quadrants(&worker, source, 400, 300);
        drop(fixture);
        capture(&mut worker, &session, 6_000_000_000);
        assert_eq!(
            worker.snapshot().problem(source).unwrap().code,
            "window_capture_target_not_found"
        );
        let replacement = Fixture::with_identity(identity);
        capture(&mut worker, &session, 7_000_000_000);
        assert_quadrants(&worker, source, 320, 240);
        worker.join().unwrap();
        drop(replacement);
        println!(
            "Controlled GDI window quadrant pixels, resize, hidden/minimized, close/recreate and owner release passed; no other application pixels requested or retained"
        );
    }
}
