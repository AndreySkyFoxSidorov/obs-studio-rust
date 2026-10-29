//! A staged window selector. Native handles and acquisition never enter a view.
use super::{Intent, WindowEdit};
use eframe::egui;
use skylive_core::{WindowCaptureBinding, WindowCaptureMethod, WindowMatchPriority, WindowTarget};
use skylive_host::{
    capture::{CaptureCapabilities, CaptureRequestKey},
    window_capture::WindowInfo,
};

pub struct WindowDraft {
    pub platform: crate::platform::CapturePlatform,
    pub token: u64,
    pub session: u64,
    pub revision: u64,
    pub edit: WindowEdit,
    pub name: String,
    pub selected: Option<WindowTarget>,
    pub priority: WindowMatchPriority,
    pub method: WindowCaptureMethod,
    pub capture_cursor: bool,
    pub client_area: bool,
    pub capture_audio: bool,
    pub windows: Vec<WindowInfo>,
    pub capabilities: CaptureCapabilities,
    pub loading: bool,
    pub error: Option<String>,
}
impl WindowDraft {
    pub fn on_platform(mut self, platform: crate::platform::CapturePlatform) -> Self {
        self.platform = platform;
        if self.selected.is_none() {
            self.client_area = platform == crate::platform::CapturePlatform::Windows;
            self.method = WindowCaptureMethod::Auto;
        }
        self
    }
    pub fn new(
        token: u64,
        session: u64,
        revision: u64,
        edit: WindowEdit,
        name: String,
        binding: Option<WindowCaptureBinding>,
        capabilities: CaptureCapabilities,
    ) -> Self {
        let mut draft = Self {
            platform: crate::platform::CapturePlatform::Unavailable,
            token,
            session,
            revision,
            edit,
            name,
            selected: None,
            priority: WindowMatchPriority::Class,
            method: WindowCaptureMethod::Auto,
            capture_cursor: capabilities.cursor,
            client_area: false,
            capture_audio: false,
            windows: vec![],
            capabilities,
            loading: true,
            error: None,
        };
        if let Some(binding) = binding {
            draft.selected = Some(binding.target().clone());
            draft.priority = binding.priority();
            draft.method = binding.method();
            draft.capture_cursor = binding.capture_cursor();
            draft.client_area = binding.client_area();
            draft.capture_audio = binding.capture_audio();
        }
        draft
    }
    pub fn accept(
        &mut self,
        pending: Option<(u64, CaptureRequestKey)>,
        key: CaptureRequestKey,
        current_session: u64,
        current_revision: u64,
        result: Result<Vec<WindowInfo>, skylive_host::capture::CaptureError>,
    ) {
        if pending != Some((self.token, key)) {
            return;
        }
        self.loading = false;
        if key.session != self.session
            || key.revision != self.revision
            || key.session != current_session
            || key.revision != current_revision
        {
            self.error = Some("The document changed. Reopen this dialog to bind a window.".into());
            return;
        }
        match result {
            Ok(windows) => {
                let windows: Vec<_> = windows
                    .into_iter()
                    .filter(|window| self.platform.window(&window.target))
                    .collect();
                if self.selected.is_none() {
                    self.selected = windows.first().map(|w| w.target.clone());
                    self.platform_defaults();
                }
                self.windows = windows;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn platform_defaults(&mut self) {
        if let Some(target) = &self.selected {
            let windows = matches!(target, WindowTarget::Windows { .. });
            self.client_area = windows;
            if !windows {
                self.method = WindowCaptureMethod::Auto;
                self.capture_audio = false;
            }
            self.priority = if windows {
                WindowMatchPriority::Class
            } else {
                WindowMatchPriority::Title
            };
        }
    }
    pub fn intent(&self) -> Result<Intent, String> {
        if self.loading {
            return Err("Wait for window enumeration before applying".into());
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if !self.capabilities.available {
            return Err("Window capture is unavailable on this host".into());
        }
        let target = self.selected.as_ref().ok_or("Select an available window")?;
        let window = skylive_host::window_capture::matching_window(&self.windows,target,self.priority)
            .ok_or("Select an available window")?;
        if !self.platform.window(&window.target) {
            return Err(
                "This saved window belongs to another platform. Choose an available window.".into(),
            );
        }
        if window.minimized {
            return Err("Restore the selected window before applying".into());
        }
        let windows = matches!(window.target, WindowTarget::Windows { .. });
        if !windows
            && (self.client_area
                || self.priority != WindowMatchPriority::Title
                || self.method != WindowCaptureMethod::Auto)
        {
            return Err("macOS requires complete-window capture and exact title matching".into());
        }
        let binding = window
            .binding_for_target(
                target,
                self.priority,
                self.capture_cursor,
                self.client_area,
                self.method,
            )
            .map_err(|e| e.to_string())?
            .with_capture_audio(self.capture_audio)
            .map_err(|e| e.to_string())?;
        Ok(Intent::CommitWindow {
            session: self.session,
            revision: self.revision,
            edit: self.edit.clone(),
            name: self.name.clone(),
            binding,
        })
    }
}
pub fn editor(ui: &mut egui::Ui, draft: &mut WindowDraft, intents: &mut Vec<Intent>) {
    if matches!(draft.edit, WindowEdit::Add { .. }) {
        let label = ui.label(crate::i18n::tr(ui, "Name"));
        ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .char_limit(256)
                .id_salt("window-source-name"),
        )
        .labelled_by(label.id)
        .on_hover_text(crate::i18n::tr(ui, "Name of this window capture source"));
    }
    ui.label(crate::i18n::tf(
        ui,
        "Backend: {} · SDR",
        &[("{}", draft.capabilities.backend.to_string())],
    ));
    if draft
        .selected
        .as_ref()
        .is_some_and(|target| !draft.platform.window(target))
    {
        ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved capture device belongs to another platform. Choose an available device to repair the binding."));
    }
    if let Some(note) = draft.capabilities.note {
        ui.label(crate::i18n::tr(ui, note));
    }
    if draft.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(crate::i18n::tr(ui, "Enumerating windows…"));
        });
    }
    if let Some(error) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
    }
    let selected_label = draft.selected.as_ref().and_then(|target|
        skylive_host::window_capture::matching_window(&draft.windows,target,draft.priority))
        .filter(|window|draft.platform.window(&window.target))
        .map_or_else(|| crate::i18n::tr(ui, "Select window"), |w| w.name.clone());
    let old = draft.selected.clone();
    ui.add_enabled_ui(!draft.loading, |ui| {
        egui::ComboBox::from_id_salt("window-target")
            .selected_text(selected_label.clone())
            .width(ui.available_width().min(450.0))
            .truncate()
            .show_ui(ui, |ui| {
                ui.set_width(ui.available_width().min(450.0));
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                for window in draft
                    .windows
                    .iter()
                    .filter(|window| draft.platform.window(&window.target))
                {
                    let label = format!(
                        "{} · {}×{}{}",
                        window.name,
                        window.width,
                        window.height,
                        if window.minimized {
                            crate::i18n::tr(ui, " · Minimized")
                        } else {
                            String::new()
                        }
                    );
                    ui.selectable_value(&mut draft.selected, Some(window.target.clone()), label)
                        .on_hover_text(&window.name)
                        .on_hover_text(crate::i18n::tr(
                            ui,
                            "Choose the application window to capture",
                        ));
                }
            })
            .response
            .on_hover_text(selected_label)
            .on_hover_text(crate::i18n::tr(ui, "Window"));
    });
    if old != draft.selected {
        draft.platform_defaults();
    }
    if !draft.loading && draft.windows.is_empty() && draft.error.is_none() {
        ui.label(crate::i18n::tr(ui, "No windows are available."));
    }
    let windows = draft.platform == crate::platform::CapturePlatform::Windows;
    ui.label(crate::i18n::tr(ui, "Window Match Priority"));
    for (value, label) in [
        (WindowMatchPriority::Title, "Window title must match"),
        (
            WindowMatchPriority::Class,
            "Match title, otherwise find window of same type",
        ),
        (
            WindowMatchPriority::Executable,
            "Match title, otherwise find window of same executable",
        ),
    ] {
        if !windows && value != WindowMatchPriority::Title {
            continue;
        }
        ui.add_enabled_ui(windows || value == WindowMatchPriority::Title, |ui| {
            ui.radio_value(&mut draft.priority, value, crate::i18n::tr(ui, label))
                .on_hover_text(crate::i18n::tr(ui, label))
                .on_disabled_hover_text(crate::i18n::tr(
                    ui,
                    "macOS requires exact title and bundle identity",
                ));
        });
    }
    if windows {
        ui.label(crate::i18n::tr(ui, "Capture Method"));
        for (method, label) in [
            (
                WindowCaptureMethod::Auto,
                "Automatic (OBS window-class policy)",
            ),
            (WindowCaptureMethod::Gdi, "BitBlt (GDI compatibility)"),
            (WindowCaptureMethod::Wgc, "Windows Graphics Capture"),
        ] {
            method_option(ui, draft, method, label);
        }
    }
    ui.add_enabled(
        draft.capabilities.cursor,
        egui::Checkbox::new(
            &mut draft.capture_cursor,
            crate::i18n::tr(ui, "Capture cursor"),
        ),
    )
    .on_hover_text(crate::i18n::tr(ui, "Capture cursor"))
    .on_disabled_hover_text(crate::i18n::tr(
        ui,
        "This backend does not support cursor composition.",
    ));
    ui.add_enabled(
        windows && draft.method!=WindowCaptureMethod::Gdi,
        egui::Checkbox::new(&mut draft.client_area, crate::i18n::tr(ui, "Client Area")),
    )
    .on_hover_text(crate::i18n::tr(
        ui,
        "GDI captures the client area. WGC can capture the complete window or its client area. macOS uses ScreenCaptureKit.",
    ))
    .on_disabled_hover_text(crate::i18n::tr(
        ui,
        "GDI captures the client area. WGC can capture the complete window or its client area. macOS uses ScreenCaptureKit.",
    ));
    if windows {
        ui.checkbox(&mut draft.capture_audio, crate::i18n::tr_key_or(ui,"SkyLive.WindowCapture.CaptureAudio","Capture audio"))
            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.WindowCapture.CaptureAudioHint",
                "Capture audio from the application owning the captured window. Changing the window changes the audio target."));
    }
    if ui
        .add_enabled(
            !draft.loading,
            egui::Button::new(crate::i18n::tr(ui, "Refresh Windows")),
        )
        .on_hover_text(crate::i18n::tr(
            ui,
            "Refresh the window list without changing the source",
        ))
        .clicked()
    {
        intents.push(Intent::RefreshWindows);
    }
    ui.label(crate::i18n::tr(ui,"Apply stores this binding without starting capture. WGC starts on the capture worker after an explicit request. Cancel changes no source."));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn caps() -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "fake",
            cursor: true,
            hdr: false,
            note: None,
        }
    }
    fn window() -> WindowInfo {
        WindowInfo {
            full_window_size: None,
            target: WindowTarget::Windows {
                title: "Editor".into(),
                class: "EditorClass".into(),
                executable: "editor.exe".into(),
            },
            name: "Editor".into(),
            width: 640,
            height: 480,
            minimized: false,
        }
    }
    #[test]
    fn picker_freshness_and_staged_binding_are_independent_of_display_results() {
        let mut draft = WindowDraft::new(
            7,
            11,
            13,
            WindowEdit::Add {
                scene: skylive_core::Session::new().preview(),
            },
            "Window".into(),
            None,
            caps(),
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        let key = CaptureRequestKey {
            session: 11,
            revision: 13,
            serial: 17,
        };
        draft.accept(Some((6, key)), key, 11, 13, Ok(vec![window()]));
        assert!(draft.loading);
        assert!(draft.windows.is_empty());
        draft.accept(Some((7, key)), key, 11, 13, Ok(vec![window()]));
        assert!(!draft.loading);
        assert!(
            matches!(draft.intent().unwrap(),Intent::CommitWindow{binding,..}if binding.client_area()&&binding.priority()==WindowMatchPriority::Class)
        );
    }
    #[test]
    fn stale_catalog_and_unsupported_options_never_produce_a_command() {
        let mut draft = WindowDraft::new(
            1,
            2,
            3,
            WindowEdit::Add {
                scene: skylive_core::Session::new().preview(),
            },
            "Window".into(),
            None,
            caps(),
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        let key = CaptureRequestKey {
            session: 2,
            revision: 3,
            serial: 4,
        };
        draft.accept(Some((1, key)), key, 2, 4, Ok(vec![window()]));
        assert!(draft.intent().is_err());
        assert!(draft.windows.is_empty());
        draft.error = None;
        draft.windows = vec![window()];
        draft.selected = Some(window().target);
        draft.client_area = false;
        assert!(draft.intent().is_err());
        draft.client_area = true;
        draft.windows[0].minimized = true;
        assert!(draft.intent().is_err());
    }
    #[test]
    fn portable_foreign_window_stays_unavailable_and_catalog_filters_foreign_entries() {
        let foreign = WindowTarget::Macos {
            title: "Saved title".into(),
            bundle_id: "saved.bundle".into(),
        };
        let binding = WindowCaptureBinding::new(
            foreign.clone(),
            WindowMatchPriority::Title,
            true,
            false,
            640,
            480,
        )
        .unwrap();
        let mut draft = WindowDraft::new(
            1,
            2,
            3,
            WindowEdit::Rebind {
                source: skylive_core::Id::new(1),
            },
            "Portable".into(),
            Some(binding),
            caps(),
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        let key = CaptureRequestKey {
            session: 2,
            revision: 3,
            serial: 1,
        };
        let mut other = window();
        other.target = foreign.clone();
        draft.accept(Some((1, key)), key, 2, 3, Ok(vec![other, window()]));
        assert_eq!(draft.selected, Some(foreign));
        assert_eq!(draft.windows.len(), 1);
        assert!(draft.intent().is_err());
        draft.selected = Some(window().target);
        draft.platform_defaults();
        assert!(draft.intent().is_ok());
    }
}

fn method_option(
    ui: &mut egui::Ui,
    draft: &mut WindowDraft,
    method: WindowCaptureMethod,
    label: &str,
) -> egui::Response {
    let response=ui.radio_value(&mut draft.method,method,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"Choose a real capture backend. WGC keeps the Windows border; protected content and HDR are not supported."));
    response
}
#[cfg(test)]
mod wgc_tests {
    use super::*;
    fn draft() -> WindowDraft {
        let mut draft = WindowDraft::new(
            1,
            2,
            3,
            WindowEdit::Add {
                scene: skylive_core::Session::new().preview(),
            },
            "WGC".into(),
            None,
            CaptureCapabilities {
                available: true,
                backend: "fake",
                cursor: true,
                hdr: false,
                note: None,
            },
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        let info = WindowInfo {
            full_window_size: Some((656, 520)),
            target: WindowTarget::Windows {
                title: "Fixture".into(),
                class: "FixtureClass".into(),
                executable: "fixture.exe".into(),
            },
            name: "Fixture".into(),
            width: 640,
            height: 480,
            minimized: false,
        };
        draft.selected = Some(info.target.clone());
        draft.windows = vec![info];
        draft.loading = false;
        draft
    }
    #[test]
    fn full_window_command_requires_wgc_and_preserves_its_revision_fence() {
        let mut draft = draft();
        assert_eq!(draft.method, WindowCaptureMethod::Auto);
        draft.method = WindowCaptureMethod::Gdi;
        draft.client_area = false;
        assert!(draft.intent().is_err());
        draft.method = WindowCaptureMethod::Wgc;
        assert!(
            matches!(draft.intent().unwrap(),Intent::CommitWindow{binding,..}if binding.method()==WindowCaptureMethod::Wgc&&!binding.client_area()&&binding.width()==656&&binding.height()==520)
        );
        draft.revision = 9;
        assert!(matches!(
            draft.intent().unwrap(),
            Intent::CommitWindow { revision: 9, .. }
        ));
    }
    #[test]
    fn new_windows_draft_defaults_to_auto_while_existing_gdi_binding_is_preserved() {
        let draft = draft();
        assert_eq!(draft.method, WindowCaptureMethod::Auto);
        let legacy = WindowCaptureBinding::new(
            draft.selected.clone().unwrap(),
            draft.priority,
            draft.capture_cursor,
            true,
            640,
            480,
        )
        .unwrap();
        let existing = WindowDraft::new(
            draft.token,
            draft.session,
            draft.revision,
            draft.edit.clone(),
            draft.name.clone(),
            Some(legacy),
            draft.capabilities,
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        assert_eq!(existing.method, WindowCaptureMethod::Gdi);
    }
    #[test]
    fn real_pointer_switches_backend_and_explicit_gdi_selection_restores_client_area() {
        let mut draft = draft();
        let ctx = egui::Context::default();
        let mut point = egui::Pos2::ZERO;
        let draw = |ctx: &egui::Context, draft: &mut WindowDraft, point: &mut egui::Pos2| {
            egui::CentralPanel::default().show(ctx, |ui| {
                *point = method_option(
                    ui,
                    draft,
                    WindowCaptureMethod::Wgc,
                    "Windows Graphics Capture",
                )
                .rect
                .center();
            });
        };
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            draw(ctx, &mut draft, &mut point)
        });
        for pressed in [true, false] {
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ctx| draw(ctx, &mut draft, &mut point),
            );
        }
        assert_eq!(draft.method, WindowCaptureMethod::Wgc);
        draft.client_area = false;
        let mut point = egui::Pos2::ZERO;
        let draw = |ctx: &egui::Context, draft: &mut WindowDraft, point: &mut egui::Pos2| {
            egui::CentralPanel::default().show(ctx, |ui| {
                *point = method_option(
                    ui,
                    draft,
                    WindowCaptureMethod::Gdi,
                    "BitBlt (GDI compatibility)",
                )
                .rect
                .center();
            });
        };
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            draw(ctx, &mut draft, &mut point)
        });
        for pressed in [true, false] {
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ctx| draw(ctx, &mut draft, &mut point),
            );
        }
        assert_eq!(draft.method, WindowCaptureMethod::Gdi);
        assert!(draft.client_area);
    }
}
