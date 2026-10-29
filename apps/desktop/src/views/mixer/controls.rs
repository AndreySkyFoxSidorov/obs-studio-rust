use super::*;
use skylive_core::{MixerDisplayPreferences, MixerLayout, MixerPeakMode, MixerSourceAction};

pub(super) fn submit(
    intents: &mut Vec<Intent>,
    session: u64,
    model: &StudioSnapshot,
    command: Command,
) {
    intents.push(Intent::MixerEdit(MixerSubmission {
        session,
        revision: model.revision(),
        command,
    }));
}
pub(super) fn view_action_pending(intents: &[Intent]) -> bool {
    intents.iter().any(|intent| {
        matches!(
            intent,
            Intent::MixerEdit(MixerSubmission {
                command: Command::SetMixerDisplay { .. }
                    | Command::SetSourceMixerState { .. }
                    | Command::SetSourceAudioMonitoring { .. }
                    | Command::UnhideAllMixerSources,
                ..
            })
        )
    })
}
pub(super) fn source_menu(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: &Source,
    session: u64,
    editable: bool,
    intents: &mut Vec<Intent>,
) {
    ui.add_enabled_ui(editable,|ui|super::super::source_audio_hotkeys::open_button(ui,session,model,source.id(),intents));
    let flags = source.mixer_state();
    let global = model
        .global_audio()
        .iter()
        .any(|slot| slot.source() == source.id());
    if ui
        .add_enabled(
            editable,
            egui::Button::new(crate::i18n::tr_key(ui, "UnhideAll")),
        )
        .on_hover_text(crate::i18n::tr(
            ui,
            "Restore mixer controls without changing audio or scene visibility.",
        ))
        .clicked()
    {
        submit(intents, session, model, Command::UnhideAllMixerSources);
        ui.close_menu();
    }
    ui.separator();
    let pin = if flags.pinned() {
        "Basic.AudioMixer.Unpin"
    } else {
        "Basic.AudioMixer.Pin"
    };
    if ui
        .add_enabled(
            editable && !global,
            egui::Button::new(crate::i18n::tr_key(ui, pin)),
        )
        .on_hover_text(crate::i18n::tr(
            ui,
            "Keep this channel visible even when its scene is inactive.",
        ))
        .on_disabled_hover_text(crate::i18n::tr(
            ui,
            "Global audio channels already stay visible.",
        ))
        .clicked()
    {
        submit(
            intents,
            session,
            model,
            Command::SetSourceMixerState {
                source: source.id(),
                action: MixerSourceAction::SetPinned(!flags.pinned()),
            },
        );
        ui.close_menu();
    }
    let hide = if flags.hidden() && !global {
        "Basic.AudioMixer.Unhide"
    } else {
        "Basic.AudioMixer.Hide"
    };
    if ui
        .add_enabled(
            editable && !global && !flags.pinned(),
            egui::Button::new(crate::i18n::tr_key(ui, hide)),
        )
        .on_hover_text(crate::i18n::tr(
            ui,
            "Hide only the mixer control. The source keeps playing and its audio is unchanged.",
        ))
        .on_disabled_hover_text(crate::i18n::tr(
            ui,
            "Unpin this channel before hiding it. Global audio channels cannot be hidden.",
        ))
        .clicked()
    {
        submit(
            intents,
            session,
            model,
            Command::SetSourceMixerState {
                source: source.id(),
                action: MixerSourceAction::SetHidden(!flags.hidden()),
            },
        );
        ui.close_menu();
    }
    let mut locked = flags.volume_locked();
    if ui
        .add_enabled_ui(editable, |ui| {
            ui.checkbox(&mut locked, crate::i18n::tr_key(ui, "LockVolume"))
        })
        .inner
        .on_hover_text(crate::i18n::tr(
            ui,
            "Lock mixer volume controls while keeping mute, filters and Advanced Audio available.",
        ))
        .changed()
    {
        submit(
            intents,
            session,
            model,
            Command::SetSourceMixerState {
                source: source.id(),
                action: MixerSourceAction::SetVolumeLocked(locked),
            },
        );
    }
    ui.separator();
    for (action,key,hint) in [
        (crate::edit_clipboard::Action::CopyFilters,"Copy.Filters","actionCopyFilters"),
        (crate::edit_clipboard::Action::PasteFilters,"Paste.Filters","actionPasteFilters"),
    ] {
        let request=crate::edit_clipboard::mixer_request(ui.ctx(),model,session,source.id(),action);
        if ui.add_enabled(editable&&request.is_some(),egui::Button::new(crate::i18n::tr_key(ui,key)))
            .on_hover_text(crate::hints::text(ui,hint))
            .clicked() {
            if let Some(request)=request {intents.push(Intent::MixerFilterClipboard(request));}
            ui.close_menu();
        }
    }
    ui.separator();
    if ui
        .add_enabled(editable, egui::Button::new(crate::i18n::tr(ui, "Rename")))
        .on_hover_text(crate::hints::text(ui, "Rename"))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::RenameSource {
            source: source.id(),
            name: source.name().into(),
        }));
        ui.close_menu();
    }
}
pub(super) fn toolbar(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    session: u64,
    program: Option<&ProgramAudioView>,
    status: Option<&ProgramTransportStats>,
    intents: &mut Vec<Intent>,
) {
    let prefs = model.preferences().mixer_display();
    ui.horizontal_wrapped(|ui| {
        ui.menu_button(crate::i18n::tr_key(ui, "Basic.AudioMixer.Options"), |ui| {
            options_menu(ui,model,session,program,status,intents);
        })
        .response
        .on_hover_text(crate::i18n::tr(
            ui,
            "Configure mixer visibility, ordering and audio properties.",
        ));
        let vertical = prefs.layout() == MixerLayout::Vertical;
        let next = if vertical {
            MixerLayout::Horizontal
        } else {
            MixerLayout::Vertical
        };
        let key = if vertical {
            "Basic.AudioMixer.Layout.Horizontal"
        } else {
            "Basic.AudioMixer.Layout.Vertical"
        };
        if ui
            .small_button(crate::i18n::tr_key(ui, key))
            .on_hover_text(crate::i18n::tr(
                ui,
                "Switch the mixer layout without changing audio or losing channel settings.",
            ))
            .clicked()
        {
            submit(
                intents,
                session,
                model,
                Command::SetMixerDisplay {
                    preferences: MixerDisplayPreferences::new(
                        next,
                        prefs.show_inactive(),
                        prefs.show_hidden(),
                        prefs.keep_inactive_last(),
                        prefs.keep_hidden_last(),
                    ).with_peak_mode(prefs.peak_mode()),
                },
            );
        }
    });
}

pub(super) fn options_menu(ui:&mut egui::Ui,model:&StudioSnapshot,session:u64,program:Option<&ProgramAudioView>,status:Option<&ProgramTransportStats>,intents:&mut Vec<Intent>) {
    let prefs=model.preferences().mixer_display();
let hidden = model
    .sources()
    .iter()
    .filter(|source| {
        source.kind().has_audio()
            && source.mixer_state().hidden()
            && !model
                .global_audio()
                .iter()
                .any(|slot| slot.source() == source.id())
    })
    .count();
if ui
    .add_enabled(
        hidden > 0,
        egui::Button::new(crate::i18n::tr_key(ui, "UnhideAll")),
    )
    .on_hover_text(crate::i18n::tr(
        ui,
        "Restore mixer controls without changing audio or scene visibility.",
    ))
    .clicked()
{
    submit(intents, session, model, Command::UnhideAllMixerSources);
    ui.close_menu();
}
ui.separator();
let mut show_hidden = prefs.show_hidden();
let mut show_inactive = prefs.show_inactive();
let mut hidden_last = prefs.keep_hidden_last();
let mut inactive_last = prefs.keep_inactive_last();
let mut changed = ui
    .checkbox(
        &mut show_hidden,
        crate::i18n::tr_key(ui, "Basic.AudioMixer.ShowHidden"),
    )
    .changed();
changed |= ui
    .checkbox(
        &mut show_inactive,
        crate::i18n::tr_key(ui, "Basic.AudioMixer.ShowInactive"),
    )
    .changed();
let vertical = prefs.layout() == MixerLayout::Vertical;
changed |= ui
    .checkbox(
        &mut hidden_last,
        crate::i18n::tr_key(
            ui,
            if vertical {
                "Basic.AudioMixer.KeepHiddenRight"
            } else {
                "Basic.AudioMixer.KeepHiddenBottom"
            },
        ),
    )
    .changed();
changed |= ui
    .checkbox(
        &mut inactive_last,
        crate::i18n::tr_key(
            ui,
            if vertical {
                "Basic.AudioMixer.KeepInactiveRight"
            } else {
                "Basic.AudioMixer.KeepInactiveBottom"
            },
        ),
    )
    .changed();
if changed {
    submit(
        intents,
        session,
        model,
        Command::SetMixerDisplay {
            preferences: MixerDisplayPreferences::new(
                prefs.layout(),
                show_inactive,
                show_hidden,
                inactive_last,
                hidden_last,
            ).with_peak_mode(prefs.peak_mode()),
        },
    );
}
ui.separator();
ui.menu_button(crate::i18n::tr_key(ui,"Basic.Settings.Audio.PeakMeterType"),|ui|{
    for (mode,key) in [(MixerPeakMode::Sample,"Basic.Settings.Audio.PeakMeterType.SamplePeak"),
        (MixerPeakMode::TruePeakEstimate,"Basic.Settings.Audio.PeakMeterType.TruePeak")] {
        if ui.selectable_label(prefs.peak_mode()==mode,crate::i18n::tr_key(ui,key))
            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Mixer.TruePeakHint",
                "True peak estimates intersample peaks from processed source audio. It uses more CPU and is not a limiter. While the Program meter is unavailable, levels wait rather than falling back to sample peak."))
            .clicked(){
            submit(intents,session,model,Command::SetMixerDisplay {preferences:prefs.with_peak_mode(mode)});
            ui.close_menu();
        }
    }
});
ui.separator();
if ui
    .button(crate::i18n::tr(ui, "Advanced Audio Properties"))
    .clicked()
{
    intents.push(Intent::OpenAdvancedAudio);
    ui.close_menu();
}
if ui
    .button(crate::i18n::tr(ui, "Global Audio Devices"))
    .clicked()
{
    intents.push(Intent::OpenGlobalAudioDevices);
    ui.close_menu();
}
ui.menu_button(crate::i18n::tr(ui, "Program Audio"), |ui| {
    program_audio_panel(ui, program, status, model.audio(), model.revision(), prefs.peak_mode())
});
ui.menu_button(crate::i18n::tr(ui, "Audio Sources"), |ui| {
    global_slots(ui, model, intents)
});
    ui.separator();
    let next=if prefs.layout()==MixerLayout::Vertical {MixerLayout::Horizontal}else{MixerLayout::Vertical};
    let key=if next==MixerLayout::Horizontal {"Basic.AudioMixer.Layout.Horizontal"}else{"Basic.AudioMixer.Layout.Vertical"};
    if ui.button(crate::i18n::tr_key(ui,key)).clicked(){submit(intents,session,model,Command::SetMixerDisplay{preferences:MixerDisplayPreferences::new(next,prefs.show_inactive(),prefs.show_hidden(),prefs.keep_inactive_last(),prefs.keep_hidden_last()).with_peak_mode(prefs.peak_mode())});ui.close_menu();}
}
