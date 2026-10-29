//! Global roots belong to the collection; profile format remains a staged page.
use super::{AudioEdit, Intent, dialogs::Dialog};
use eframe::egui;
use skylive_core::{AudioSlot, Id, SourceKind, StudioSnapshot};

pub struct GlobalAudioDraft {
    pub session: u64,
    pub platform: crate::platform::CapturePlatform,
    pub error: Option<String>,
}
pub struct LiveGlobalAudioEdit {
    pub session: u64,
    pub revision: u64,
    pub slot: AudioSlot,
    pub source: Option<Id>,
}
impl GlobalAudioDraft {
    pub fn new(session: u64, platform: crate::platform::CapturePlatform) -> Self {
        Self {
            session,
            platform,
            error: None,
        }
    }
}
const SLOTS: [(AudioSlot, &str); 6] = [
    (AudioSlot::Desktop1, "Desktop Audio 1"),
    (AudioSlot::Desktop2, "Desktop Audio 2"),
    (AudioSlot::Mic1, "Mic/Aux 1"),
    (AudioSlot::Mic2, "Mic/Aux 2"),
    (AudioSlot::Mic3, "Mic/Aux 3"),
    (AudioSlot::Mic4, "Mic/Aux 4"),
];
pub fn editor(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    draft: &mut GlobalAudioDraft,
    intents: &mut Vec<Intent>,
) {
    ui.label(crate::i18n::tr(ui,"Changes to global audio slots apply immediately and can be undone. Close keeps confirmed changes. Device properties use their own Apply and Cancel."));
    if let Some(error) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
    }
    egui::ScrollArea::horizontal().id_salt("global-audio-device-columns").show(ui,|ui| {
    ui.set_min_width(600.0);
    for (slot, label) in SLOTS {
        let current = model
            .global_audio()
            .iter()
            .find(|root| root.slot() == slot)
            .map(|root| root.source());
        let source = current.and_then(|id| model.source(id));
        ui.push_id(slot,|ui| {
            ui.horizontal(|ui| {
                ui.add_sized([124.0,26.0],egui::Label::new(crate::i18n::tr(ui,label)).truncate()).on_hover_text(crate::i18n::tr(ui,label));
                let caption=source.map_or_else(||crate::i18n::tr(ui,"Disabled"),|source|source.name().to_owned());
                let menu=egui::ComboBox::from_id_salt("global-audio-root").selected_text(&caption).width(230.0).truncate().show_ui(ui,|ui| {
                    ui.set_width(230.0);ui.style_mut().wrap_mode=Some(egui::TextWrapMode::Wrap);
                    for (value,name) in std::iter::once((None,crate::i18n::tr(ui,"Disabled"))).chain(model.sources().iter().filter(|source|matches!(source.kind(),SourceKind::AudioCapture{binding}if binding.flow()==slot.flow()&&draft.platform.audio(slot.flow(),binding.target()))).map(|source|(Some(source.id()),source.name().to_owned()))) {
                        let response=ui.selectable_label(current==value,&name).on_hover_text(&name).on_hover_text(crate::i18n::tr(ui,"Choose an existing audio source for this global slot, or disable the slot."));
                        #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new(("global-audio-option",slot,value)),response.rect));
                        if response.clicked() {
                            intents.push(Intent::LiveGlobalAudio(LiveGlobalAudioEdit{session:draft.session,revision:model.revision(),slot,source:value}));ui.close_menu();
                        }
                    }
                }).response.on_hover_text(caption).on_hover_text(crate::i18n::tr(ui,"Choose an existing audio source for this global slot, or disable the slot."));
                #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new(("global-audio-choice",slot)),menu.rect));
                let _=menu;
                let available=draft.platform!=crate::platform::CapturePlatform::Unavailable;
                let add=ui.add_enabled(available,egui::Button::new(crate::i18n::tr(ui,"Add New")).min_size(egui::vec2(78.0,24.0))).on_hover_text(crate::i18n::tr(ui,"Create and configure a new audio source for this global slot.")).on_disabled_hover_text(crate::i18n::tr(ui,"Audio capture is unavailable on this platform."));
                if add.clicked() {
                    let label=crate::i18n::tr(ui,label);
                    let name=if !model.sources().iter().any(|source|source.name()==label) {Some(label)} else {(2..=4097).map(|index|format!("{label} {index}")).find(|name|!model.sources().iter().any(|source|source.name()==name))};
                    match name {Some(name)=>intents.push(Intent::OpenDialog(Dialog::audio(AudioEdit::AddGlobal{slot},name,slot.flow()))),None=>intents.push(Intent::DialogError("No unique audio source name is available.".into()))}
                }
                let properties=ui.add_enabled(source.is_some(),egui::Button::new(crate::i18n::tr(ui,"Properties")).min_size(egui::vec2(90.0,24.0))).on_hover_text(crate::i18n::tr(ui,"Choose the device and capture options for this global audio source.")).on_disabled_hover_text(crate::i18n::tr(ui,"Choose or add an audio source first."));
                if properties.clicked() {if let Some(source)=source {if let SourceKind::AudioCapture{binding}=source.kind() {intents.push(Intent::OpenDialog(Dialog::audio_properties(source.id(),source.name().to_owned(),binding)));}}}
            });
            if source.is_some_and(|source|matches!(source.kind(),SourceKind::AudioCapture{binding}if !draft.platform.audio(slot.flow(),binding.target()))) {ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved audio device is unavailable for this operating system or capture type. Choose an available device or Default device."));}
        });
    }
    });
    ui.label(crate::i18n::tr(ui,"A shared source keeps the same volume, mute and filters in every slot and scene. Global slots stay active independently of scene selection."));
}
