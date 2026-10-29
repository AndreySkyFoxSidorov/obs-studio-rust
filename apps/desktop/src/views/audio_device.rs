//! Friendly device selection is presentation metadata; bindings retain exact IDs.
use super::{
    AudioEdit, Intent,
    dialogs::{AudioDevicePicker, AudioTargetChoice, audio_target_label},
};
use eframe::egui;
use skylive_core::{AudioFlow, AudioTarget};

pub fn choice_allowed(
    platform: crate::platform::CapturePlatform,
    flow: AudioFlow,
    choice: AudioTargetChoice,
) -> bool {
    use crate::platform::CapturePlatform;
    match choice {
        AudioTargetChoice::Default => platform != CapturePlatform::Unavailable,
        AudioTargetChoice::WindowsProcess => platform == CapturePlatform::Windows && flow == AudioFlow::Output,
        AudioTargetChoice::WindowsEndpoint => platform == CapturePlatform::Windows,
        AudioTargetChoice::MacDevice => {
            platform == CapturePlatform::Macos && flow == AudioFlow::Input
        }
        AudioTargetChoice::MacSystem => {
            platform == CapturePlatform::Macos && flow == AudioFlow::Output
        }
    }
}

fn matches(choice: AudioTargetChoice, identity: &str, target: &AudioTarget) -> bool {
    match (choice, target) {
        (AudioTargetChoice::Default, AudioTarget::Default)
        | (AudioTargetChoice::MacSystem, AudioTarget::MacSystem) => true,
        (AudioTargetChoice::WindowsProcess,AudioTarget::WindowsWindow {selector}) => serde_json::from_str::<skylive_core::WindowsAudioWindowSelector>(identity).is_ok_and(|saved|saved.title()==selector.title()&&saved.class()==selector.class()&&saved.executable()==selector.executable()),
        (AudioTargetChoice::WindowsProcess,AudioTarget::WindowsProcess {identity:process}) => serde_json::from_str::<skylive_core::WindowsProcessIdentity>(identity).is_ok_and(|saved|&saved==process),
        (AudioTargetChoice::WindowsEndpoint, AudioTarget::WindowsEndpoint { id }) => identity == id,
        (AudioTargetChoice::MacDevice, AudioTarget::MacDevice { uid }) => identity == uid,
        _ => false,
    }
}
fn select(target: &AudioTarget, choice: &mut AudioTargetChoice, identity: &mut String) {
    match target {
        AudioTarget::Default => *choice = AudioTargetChoice::Default,
        AudioTarget::WindowsWindow {selector} => {*choice=AudioTargetChoice::WindowsProcess;*identity=serde_json::to_string(selector).unwrap_or_default();}
        AudioTarget::WindowsProcess {identity:process} => {*choice=AudioTargetChoice::WindowsProcess;*identity=serde_json::to_string(process).unwrap_or_default();}
        AudioTarget::WindowsEndpoint { id } => {
            *choice = AudioTargetChoice::WindowsEndpoint;
            identity.clone_from(id);
        }
        AudioTarget::MacDevice { uid } => {
            *choice = AudioTargetChoice::MacDevice;
            identity.clone_from(uid);
        }
        AudioTarget::MacSystem => *choice = AudioTargetChoice::MacSystem,
    }
}
pub fn selected_name(
    ui: &egui::Ui,
    flow: AudioFlow,
    choice: AudioTargetChoice,
    identity: &str,
    picker: Option<&AudioDevicePicker>,
) -> String {
    if choice == AudioTargetChoice::Default {
        let default = crate::i18n::tr(ui, "Default device");
        return picker
            .and_then(|picker| {
                picker.devices.iter().find(|device| {
                    device.is_default
                        && device.flow == flow
                        && picker.platform.audio(flow, &device.target)
                })
            })
            .map_or(default.clone(), |device| {
                format!("{default} · {}", device.name)
            });
    }
    if picker.is_some_and(|picker| picker.loading) {
        return crate::i18n::tr(ui, "Enumerating audio devices…");
    }
    picker
        .and_then(|picker| {
            picker.devices.iter().find(|device| {
                device.flow == flow
                    && picker.platform.audio(flow, &device.target)
                    && matches(choice, identity, &device.target)
            })
        })
        .map(|device| device.name.clone())
        .unwrap_or_else(|| crate::i18n::tr(ui, "Unavailable device"))
}
pub struct AudioDeviceFields<'a> {
    pub edit: &'a AudioEdit,
    pub name: &'a mut String,
    pub flow: AudioFlow,
    pub target: &'a mut AudioTargetChoice,
    pub identity: &'a mut String,
    pub use_device_timing: &'a mut bool,
    pub exclude_own_audio: &'a mut bool,
    pub picker: Option<&'a mut AudioDevicePicker>,
}
pub fn editor(ui: &mut egui::Ui, fields: AudioDeviceFields<'_>, intents: &mut Vec<Intent>) {
    let AudioDeviceFields {
        edit,
        name,
        flow,
        target,
        identity,
        use_device_timing,
        exclude_own_audio,
        picker,
    } = fields;
    if !matches!(edit, AudioEdit::Rebind { .. }) {
        let label = ui.label(crate::i18n::tr(ui, "Name"));
        ui.add(
            egui::TextEdit::singleline(name)
                .char_limit(256)
                .id_salt("audio-source-name"),
        )
        .labelled_by(label.id)
        .on_hover_text(crate::hints::text(ui, "audio-source-name"));
    }
    if *target == AudioTargetChoice::WindowsProcess {
        application_editor(ui,identity,use_device_timing,picker.as_deref(),intents);return;
    }
    let platform = picker
        .as_deref()
        .map_or(crate::platform::CapturePlatform::Unavailable, |picker| {
            picker.platform
        });
    if !choice_allowed(platform, flow, *target) {
        ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved audio device is unavailable for this operating system or capture type. Choose an available device or Default device."));
    }
    let label = ui.label(crate::i18n::tr(ui, "Device"));
    let selected = selected_name(ui, flow, *target, identity, picker.as_deref());
    let picker_response = egui::ComboBox::from_id_salt("native-audio-device")
        .selected_text(selected.clone())
        .width(ui.available_width().min(450.0))
        .truncate()
        .show_ui(ui, |ui| {
            ui.set_width(ui.available_width().min(450.0));
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            let default =
                selected_name(ui, flow, AudioTargetChoice::Default, "", picker.as_deref());
            if ui
                .selectable_label(*target == AudioTargetChoice::Default, &default)
                .on_hover_text(default)
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Follow the system's default audio device.",
                ))
                .clicked()
            {
                *target = AudioTargetChoice::Default;
                ui.close_menu();
            }
            if let Some(picker) = picker.as_deref() {
                for device in &picker.devices {
                    if device.flow != flow
                        || device.target == AudioTarget::Default
                        || !platform.audio(flow, &device.target)
                    {
                        continue;
                    }
                    ui.push_id(&device.target, |ui| {
                        if ui
                            .selectable_label(
                                matches(*target, identity, &device.target),
                                &device.name,
                            )
                            .on_hover_text(&device.name)
                            .on_hover_text(crate::hints::text(ui, "Audio device"))
                            .clicked()
                        {
                            select(&device.target, target, identity);
                            ui.close_menu();
                        }
                    });
                }
            }
        })
        .response
        .labelled_by(label.id)
        .on_hover_text(selected)
        .on_hover_text(crate::i18n::tr(
            ui,
            "Choose an available audio device, or follow the system default.",
        ));
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new("audio-native-picker-rect"),
            picker_response.rect,
        )
    });
    let _ = picker_response;
    if let Some(picker) = picker.as_deref() {
        if picker.loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr(ui, "Enumerating audio devices…"));
            });
        }
        if let Some(error) = &picker.error {
            ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
        }
        if ui
            .add_enabled(
                !picker.loading && picker.capabilities.device_enumeration,
                egui::Button::new(crate::i18n::tr(ui, "List Native Devices")),
            )
            .on_hover_text(crate::i18n::tr(
                ui,
                "Refresh the available device names without starting audio capture.",
            ))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Device listing is unavailable or already in progress.",
            ))
            .clicked()
        {
            intents.push(Intent::EnumerateAudioDevices);
        }
    }
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.remove::<egui::Rect>(egui::Id::new("audio-binding-type-rect"));
        data.remove::<egui::Id>(egui::Id::new("audio-binding-type-id"));
        data.remove::<egui::Rect>(egui::Id::new("audio-binding-popup-rect"));
    });
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Advanced")).id_salt("audio-device-advanced").show(ui,|ui|{
        let label=ui.label(crate::i18n::tr(ui,"Manual device binding"));
        let response=egui::ComboBox::from_id_salt("audio-target-kind").selected_text(crate::i18n::tr(ui,if choice_allowed(platform,flow,*target){audio_target_label(*target)}else{"Unavailable device"})).show_ui(ui,|ui|{#[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new("audio-binding-popup-rect"),ui.max_rect().intersect(ui.clip_rect())));for choice in [AudioTargetChoice::Default,AudioTargetChoice::WindowsEndpoint,AudioTargetChoice::MacDevice,AudioTargetChoice::MacSystem]{if !choice_allowed(platform,flow,choice){continue;}ui.selectable_value(target,choice,crate::i18n::tr(ui,audio_target_label(choice))).on_hover_text(crate::i18n::tr(ui,"Choose the platform for an exact saved device identifier."));}}).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Edit a device binding for this or another computer."));
        #[cfg(test)] if response.enabled()&&ui.clip_rect().contains_rect(response.rect){ui.ctx().data_mut(|data|{data.insert_temp(egui::Id::new("audio-binding-type-rect"),response.rect);data.insert_temp(egui::Id::new("audio-binding-type-id"),response.id);});}
        #[cfg(not(test))] let _=response;
        if choice_allowed(platform,flow,*target)&&matches!(target,AudioTargetChoice::WindowsEndpoint|AudioTargetChoice::MacDevice){let label=ui.label(crate::i18n::tr(ui,if *target==AudioTargetChoice::WindowsEndpoint{"Windows endpoint ID"}else{"macOS device UID"}));ui.add(egui::TextEdit::singleline(identity).char_limit(1024).desired_width(ui.available_width()).id_salt("audio-target-identity")).labelled_by(label.id).on_hover_text(crate::hints::text(ui,"audio-target-identity"));}
        ui.checkbox(use_device_timing,crate::i18n::tr(ui,"Use device timing")).on_hover_text(crate::hints::text(ui,"Use device timing"));
        let exclude_supported=flow==AudioFlow::Output&&picker.as_deref().is_some_and(|picker|picker.capabilities.exclude_own_audio)&&platform==crate::platform::CapturePlatform::Macos;
        if exclude_supported||*exclude_own_audio{ui.add_enabled(exclude_supported||*exclude_own_audio,egui::Checkbox::new(exclude_own_audio,crate::i18n::tr(ui,"Exclude this application's audio"))).on_hover_text(crate::hints::text(ui,"Exclude this application's audio"));if !exclude_supported{ui.label(crate::i18n::tr(ui,"This saved option is unavailable on this platform. Clear it to use the selected device."));}}
        ui.label(crate::i18n::tr(ui,"Exact identifiers are retained when a device is unavailable. Choosing a listed device replaces its binding."));
    });
    ui.label(crate::i18n::tr(ui,"Selecting a device does not start playback. Enable a source monitoring mode in Advanced Audio. Capturing and monitoring the same playback endpoint can feed audio back; choose a separate output when needed."));
}

fn application_editor(ui:&mut egui::Ui,identity:&mut String,use_device_timing:&mut bool,picker:Option<&AudioDevicePicker>,intents:&mut Vec<Intent>) {
    let selected=serde_json::from_str::<skylive_core::WindowsAudioWindowSelector>(identity).ok();
    let label_text=if let Some(value)=&selected{format!("{} · [{}] {}",crate::i18n::tr_key_or(ui,"SkyLive.ApplicationAudio.PersistentWindow","Persistent window"),value.executable(),value.title())}else{picker.and_then(|p|p.devices.iter().find(|d|matches(AudioTargetChoice::WindowsProcess,identity,&d.target))).map(|d|format!("{} · {}",crate::i18n::tr(ui,"Application process"),d.name)).unwrap_or_else(||crate::i18n::tr(ui,"Choose a running application"))};
    let label=ui.label(crate::i18n::tr(ui,"Application"));
    egui::ComboBox::from_id_salt("application-audio-process").width(ui.available_width().min(500.0)).truncate().selected_text(label_text).show_ui(ui,|ui|{
        if let Some(picker)=picker{for device in &picker.devices{
            if !matches!(&device.target,AudioTarget::WindowsWindow{..}|AudioTarget::WindowsProcess{..}){continue;}
            let (kind,hint)=match &device.target {
                AudioTarget::WindowsWindow{..}=>(crate::i18n::tr_key_or(ui,"SkyLive.ApplicationAudio.PersistentWindow","Persistent window"),crate::i18n::tr_key_or(ui,"SkyLive.ApplicationAudio.WindowReconnectHint","Search for the window again after it closes or the application restarts.")),
                _=>(crate::i18n::tr(ui,"Application process"),crate::i18n::tr(ui,"Capture stops when this process exits. Restarted applications must be selected again; system audio is never substituted.")),
            };
            if ui.selectable_label(matches(AudioTargetChoice::WindowsProcess,identity,&device.target),format!("{kind} · {}",device.name)).on_hover_text(hint).clicked(){let mut choice=AudioTargetChoice::WindowsProcess;select(&device.target,&mut choice,identity);ui.close_menu();}
        }}
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Capture the selected application's process tree, including its child processes."));
    if let Some(selector)=&selected {
        use skylive_core::WindowsAudioWindowPriority as P;
        ui.label(crate::i18n::tr(ui,"Window Match Priority"));
        for (priority,label) in [(P::Title,"Window title must match"),(P::Class,"Match title, otherwise find window of same type"),(P::Executable,"Match title, otherwise find window of same executable")] {
            if ui.selectable_label(selector.priority()==priority,crate::i18n::tr(ui,label)).clicked(){
                if let Ok(updated)=skylive_core::WindowsAudioWindowSelector::new(selector.title().into(),selector.class().into(),selector.executable().into(),priority){*identity=serde_json::to_string(&updated).unwrap_or_default();}
            }
        }
    }
    if let Some(picker)=picker {
        if picker.loading {ui.horizontal(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,"Listing running applications…"));});}
        if let Some(error)=&picker.error {let text=if let Some((code,detail))=error.split_once(": "){format!("{code}: {}",crate::i18n::tr(ui,detail))}else{crate::i18n::tr(ui,error)};ui.colored_label(ui.visuals().error_fg_color,text);}
        if !picker.loading&&picker.devices.is_empty()&&picker.error.is_none(){ui.label(crate::i18n::tr(ui,"No accessible application windows were found."));}
        if ui.add_enabled(!picker.loading&&picker.platform==crate::platform::CapturePlatform::Windows,egui::Button::new(crate::i18n::tr(ui,"Refresh Applications"))).on_hover_text(crate::i18n::tr(ui,"Refresh actual application windows and process identities without starting capture.")).clicked(){intents.push(Intent::EnumerateAudioDevices);}
    }
    if selected.is_some(){ui.label(crate::i18n::tr_key_or(ui,"SkyLive.ApplicationAudio.WindowReconnectHint","Search for the window again after it closes or the application restarts."));}else{ui.label(crate::i18n::tr(ui,"Capture stops when this process exits. Restarted applications must be selected again; system audio is never substituted."));}
    ui.checkbox(use_device_timing,crate::i18n::tr(ui,"Use device timing")).on_hover_text(crate::hints::text(ui,"Use device timing"));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn friendly_picker_keeps_exact_authored_identity_and_hides_platform_fields() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut picker = AudioDevicePicker {
            platform: crate::platform::CapturePlatform::Windows,
            token: 1,
            session: 1,
            revision: 0,
            capabilities: skylive_host::audio_capture::AudioCapabilities {
                backend: "fake",
                input: true,
                output: true,
                device_enumeration: true,
                exclude_own_audio: false,
                note: None,
            },
            devices: vec![skylive_host::audio_capture::AudioDeviceInfo {
                target: AudioTarget::WindowsEndpoint {
                    id: "endpoint-exact-id".into(),
                },
                name: "Studio Microphone".into(),
                flow: AudioFlow::Input,
                is_default: true,
            }],
            loading: false,
            error: None,
        };
        let mut name = "Voice".to_owned();
        let mut choice = AudioTargetChoice::WindowsEndpoint;
        let mut identity = "endpoint-exact-id".to_owned();
        let mut timing = false;
        let mut exclude = false;
        let mut intents = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert_eq!(
                        selected_name(ui, AudioFlow::Input, choice, &identity, Some(&picker)),
                        "Studio Microphone"
                    );
                    assert_eq!(
                        selected_name(
                            ui,
                            AudioFlow::Input,
                            AudioTargetChoice::Default,
                            "",
                            Some(&picker)
                        ),
                        "Default device · Studio Microphone"
                    );
                    assert_eq!(
                        selected_name(
                            ui,
                            AudioFlow::Input,
                            AudioTargetChoice::MacDevice,
                            "foreign-exact-uid",
                            Some(&picker)
                        ),
                        "Unavailable device"
                    );
                    editor(
                        ui,
                        AudioDeviceFields {
                            edit: &AudioEdit::Rebind {
                                source: skylive_core::Id::new(1),
                            },
                            name: &mut name,
                            flow: AudioFlow::Input,
                            target: &mut choice,
                            identity: &mut identity,
                            use_device_timing: &mut timing,
                            exclude_own_audio: &mut exclude,
                            picker: Some(&mut picker),
                        },
                        &mut intents,
                    );
                });
            },
        );
        fn contains(shape: &egui::epaint::Shape, label: &str) -> bool {
            match shape {
                egui::epaint::Shape::Text(text) => text.galley.text() == label,
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().any(|shape| contains(shape, label))
                }
                _ => false,
            }
        }
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| contains(&shape.shape, "Studio Microphone"))
        );
        for raw in [
            "Windows endpoint",
            "macOS device",
            "Windows endpoint ID",
            "endpoint-exact-id",
        ] {
            assert!(
                !output
                    .shapes
                    .iter()
                    .any(|shape| contains(&shape.shape, raw)),
                "advanced binding field {raw} must not crowd the primary picker"
            );
        }
        assert_eq!(identity, "endpoint-exact-id");
        assert!(intents.is_empty());
    }
    #[test]
    fn current_platform_and_flow_scope_both_device_selectors_for_all_edit_modes() {
        use crate::platform::CapturePlatform;
        fn find(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| find(shape, label))
                }
                _ => None,
            }
        }
        for platform in [CapturePlatform::Windows, CapturePlatform::Macos] {
            for flow in [AudioFlow::Input, AudioFlow::Output] {
                for edit_index in 0..3 {
                    let context = egui::Context::default();
                    crate::theme::install(&context);
                    crate::i18n::install(
                        &context,
                        crate::i18n::english().unwrap(),
                        std::path::Path::new("headless-translations.csv"),
                    );
                    let session = skylive_core::Session::new();
                    let edit = match edit_index {
                        0 => AudioEdit::Add {
                            scene: session.preview(),
                        },
                        1 => AudioEdit::AddGlobal {
                            slot: if flow == AudioFlow::Input {
                                skylive_core::AudioSlot::Mic1
                            } else {
                                skylive_core::AudioSlot::Desktop1
                            },
                        },
                        _ => AudioEdit::Rebind {
                            source: skylive_core::Id::new(1),
                        },
                    };
                    let mut picker = AudioDevicePicker {
                        platform,
                        token: 1,
                        session: session.identity(),
                        revision: 0,
                        capabilities: skylive_host::audio_capture::AudioCapabilities {
                            backend: "fake",
                            input: true,
                            output: true,
                            device_enumeration: true,
                            exclude_own_audio: true,
                            note: None,
                        },
                        devices: vec![
                            skylive_host::audio_capture::AudioDeviceInfo {
                                target: AudioTarget::WindowsEndpoint {
                                    id: "windows-exact-id".into(),
                                },
                                name: "Windows device".into(),
                                flow,
                                is_default: platform == CapturePlatform::Windows,
                            },
                            skylive_host::audio_capture::AudioDeviceInfo {
                                target: AudioTarget::MacDevice {
                                    uid: "mac-input-exact-id".into(),
                                },
                                name: "Mac input device".into(),
                                flow: AudioFlow::Input,
                                is_default: platform == CapturePlatform::Macos
                                    && flow == AudioFlow::Input,
                            },
                            skylive_host::audio_capture::AudioDeviceInfo {
                                target: AudioTarget::MacSystem,
                                name: "Mac system device".into(),
                                flow: AudioFlow::Output,
                                is_default: platform == CapturePlatform::Macos
                                    && flow == AudioFlow::Output,
                            },
                        ],
                        loading: false,
                        error: None,
                    };
                    let (mut name, mut target, mut identity, mut timing, mut exclude) = (
                        "Fixture".to_owned(),
                        AudioTargetChoice::Default,
                        String::new(),
                        false,
                        false,
                    );
                    let mut frame = |events| {
                        context.run(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(800.0, 600.0),
                                )),
                                events,
                                ..Default::default()
                            },
                            |ctx| {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    editor(
                                        ui,
                                        AudioDeviceFields {
                                            edit: &edit,
                                            name: &mut name,
                                            flow,
                                            target: &mut target,
                                            identity: &mut identity,
                                            use_device_timing: &mut timing,
                                            exclude_own_audio: &mut exclude,
                                            picker: Some(&mut picker),
                                        },
                                        &mut vec![],
                                    )
                                });
                            },
                        )
                    };
                    let click = |point, pressed| {
                        vec![
                            egui::Event::PointerMoved(point),
                            egui::Event::PointerButton {
                                pos: point,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ]
                    };
                    let _ = frame(vec![]);
                    let _ = frame(vec![]);
                    let rect = context
                        .data_mut(|data| {
                            data.get_temp::<egui::Rect>(egui::Id::new("audio-native-picker-rect"))
                        })
                        .unwrap();
                    let _ = frame(click(rect.center(), true));
                    let _ = frame(click(rect.center(), false));
                    let _ = frame(vec![]);
                    let output = frame(vec![]);
                    for forbidden in if platform == CapturePlatform::Windows {
                        vec!["Mac input device", "Mac system device"]
                    } else {
                        vec!["Windows device"]
                    } {
                        assert!(
                            !output
                                .shapes
                                .iter()
                                .any(|shape| find(&shape.shape, forbidden).is_some()),
                            "foreign device {forbidden} appeared for {platform:?}/{flow:?}/{edit_index}"
                        );
                    }
                    let expected = if platform == CapturePlatform::Windows {
                        "Windows device"
                    } else if flow == AudioFlow::Input {
                        "Mac input device"
                    } else {
                        "Mac system device"
                    };
                    assert!(
                        output
                            .shapes
                            .iter()
                            .any(|shape| find(&shape.shape, expected).is_some())
                    );
                    let _ = frame(vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]);
                    let output = frame(vec![]);
                    let advanced = output
                        .shapes
                        .iter()
                        .find_map(|shape| find(&shape.shape, "Advanced"))
                        .unwrap();
                    let _ = frame(click(advanced, true));
                    let _ = frame(click(advanced, false));
                    for _ in 0..24 {
                        let _ = frame(vec![]);
                    }
                    let rect = context
                        .data_mut(|data| {
                            data.get_temp::<egui::Rect>(egui::Id::new("audio-binding-type-rect"))
                        })
                        .unwrap();
                    let _ = frame(click(rect.center(), true));
                    let _ = frame(click(rect.center(), false));
                    let _ = frame(vec![]);
                    let output = frame(vec![]);
                    let options: Vec<_> = [
                        AudioTargetChoice::Default,
                        AudioTargetChoice::WindowsEndpoint,
                        AudioTargetChoice::MacDevice,
                        AudioTargetChoice::MacSystem,
                    ]
                    .into_iter()
                    .filter(|choice| choice_allowed(platform, flow, *choice))
                    .collect();
                    assert_eq!(options.len(), 2);
                    for forbidden in if platform == CapturePlatform::Windows {
                        vec!["macOS device", "macOS system audio", "macOS device UID"]
                    } else {
                        vec!["Windows endpoint", "Windows endpoint ID"]
                    } {
                        assert!(
                            !output
                                .shapes
                                .iter()
                                .any(|shape| find(&shape.shape, forbidden).is_some()),
                            "foreign advanced option {forbidden} appeared for {platform:?}/{flow:?}/{edit_index}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn foreign_authored_binding_is_retained_until_current_platform_repair_and_capabilities_gate_apply()
     {
        use super::super::dialogs::Dialog;
        use crate::platform::CapturePlatform;
        use skylive_core::{AudioCaptureBinding, Command, SourceKind};
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let original = AudioCaptureBinding::new(
            AudioFlow::Input,
            AudioTarget::MacDevice {
                uid: "saved-mac-uid".into(),
            },
            true,
            false,
        )
        .unwrap();
        session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Portable microphone".into(),
                binding: original.clone(),
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let mut dialog = Dialog::audio_properties(source, "Portable microphone".into(), &original);
        if let Dialog::AudioCapture { device_picker, .. } = &mut dialog {
            *device_picker = Some(AudioDevicePicker {
                platform: CapturePlatform::Windows,
                token: 1,
                session: session.identity(),
                revision: session.snapshot().revision(),
                capabilities: skylive_host::audio_capture::AudioCapabilities {
                    backend: "fake",
                    input: true,
                    output: false,
                    device_enumeration: true,
                    exclude_own_audio: false,
                    note: None,
                },
                devices: vec![],
                loading: false,
                error: None,
            });
        }
        let before = session.snapshot();
        assert!(dialog.audio_intent().is_err());
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),SourceKind::AudioCapture{binding} if binding==&original)
        );
        if let Dialog::AudioCapture { target, .. } = &mut dialog {
            *target = AudioTargetChoice::Default;
        }
        let Intent::ApplyAudioBinding {
            revision, command, ..
        } = dialog.audio_intent().unwrap().unwrap()
        else {
            panic!("typed audio repair")
        };
        session.apply_at(Some(revision), command).unwrap();
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),SourceKind::AudioCapture{binding} if binding.target()==&AudioTarget::Default)
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),SourceKind::AudioCapture{binding} if binding==&original)
        );
        if let Dialog::AudioCapture { flow, .. } = &mut dialog {
            *flow = AudioFlow::Output;
        }
        assert!(
            dialog.audio_intent().is_err(),
            "unsupported flow cannot claim a working capture binding"
        );
    }
}
