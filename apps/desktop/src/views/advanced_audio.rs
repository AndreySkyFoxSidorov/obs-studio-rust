//! A live source inspector. Scalar row drafts commit only on a completed user
//! gesture; routing, PCM, monitoring and track ownership stay outside the UI.
use super::{Intent, dialogs::Dialog};
use eframe::egui;
use skylive_core::{Id, Session, Source, SourceKind, StudioSnapshot};
use skylive_host::transition::{FrameComposition, FrameScene, OutputKey};
use skylive_host::{
    audio_capture::{AudioCaptureSnapshot, NativeAudioFormat},
    media_source::MediaSnapshot,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Db,
    Percent,
}
struct RowDraft {
    gain: f32,
    muted: bool,
    dirty: bool,
    force_mono: bool,
    balance: f32,
    properties_dirty: bool,
    sync_offset_ms: i32,
    sync_dirty: bool,
    audio_tracks:skylive_core::SourceAudioTrackMask,
}
#[derive(Clone, Copy)]
pub struct LiveAudioEdit {
    pub session: u64,
    pub revision: u64,
    pub source: Id,
    pub gain: f32,
    pub muted: bool,
}
#[derive(Clone, Copy)]
pub struct LiveAudioPropertyEdit {
    pub session: u64,
    pub revision: u64,
    pub source: Id,
    pub force_mono: bool,
    pub balance: f32,
    pub sync_offset_ms: i32,
    pub audio_tracks:skylive_core::SourceAudioTrackMask,
}
pub struct AdvancedAudioDraft {
    pub session: u64,
    pub error: Option<String>,
    revision: u64,
    unit: Unit,
    rows: HashMap<Id, RowDraft>,
    activity_key: Option<(u64, Option<OutputKey>, bool, Id)>,
    program: HashSet<Id>,
    preview: HashSet<Id>,
    formats: HashMap<Id, NativeAudioFormat>,
}
impl AdvancedAudioDraft {
    pub fn new(session: u64) -> Self {
        Self {
            session,
            error: None,
            revision: u64::MAX,
            unit: Unit::Db,
            rows: HashMap::new(),
            activity_key: None,
            program: HashSet::new(),
            preview: HashSet::new(),
            formats: HashMap::new(),
        }
    }
    pub fn refresh_formats(
        &mut self,
        session: &Session,
        capture: &AudioCaptureSnapshot,
        media: &MediaSnapshot,
    ) {
        self.formats.clear();
        if self.session != session.identity() {
            return;
        }
        let model = session.snapshot();
        for source in model.sources() {
            let format = match source.kind() {
                kind if kind.audio_capture_binding().is_some() => capture
                    .packet(source.id())
                    .filter(|packet| {
                        capture.session() == session.identity() && Some(&packet.binding) == kind.audio_capture_binding()
                    })
                    .map(|packet| packet.block.format),
                SourceKind::MediaSource { media: binding } => {
                    media.audio_format_for(session.identity(), source.id(), binding)
                }
                _ => None,
            };
            if let Some(format) = format {
                self.formats.insert(source.id(), format);
            }
        }
    }
    pub fn refresh_ndi_formats(&mut self,session:&Session,ndi:&skylive_host::ndi::NdiSnapshot){
        if self.session!=session.identity()||ndi.session()!=session.identity(){return;}
        let model=session.snapshot();if !model.preferences().plugin_enabled(skylive_core::NDI_PLUGIN){return;}
        for (source,format) in ndi.formats(){if matches!(model.source(source).map(|s|s.kind()),Some(SourceKind::NdiSource {ndi})if ndi.audio()){self.formats.insert(source,format);}}
    }
    pub fn refresh_browser_formats(&mut self,session:&Session,browser:&skylive_host::browser::BrowserSnapshot){
        if self.session!=session.identity()||browser.session()!=session.identity(){return;}
        let model=session.snapshot();for (source,binding,format)in browser.formats(){if matches!(model.source(source).map(|s|s.kind()),Some(SourceKind::Browser{browser})if browser==binding&&browser.control_audio()){self.formats.insert(source,format);}}
    }
    pub fn refresh_activity(
        &mut self,
        session: &Session,
        output: Option<&FrameComposition>,
        studio: bool,
    ) {
        if self.session != session.identity() {
            self.program.clear();
            self.preview.clear();
            return;
        }
        let model = session.snapshot();
        let key = (
            model.revision(),
            output.map(FrameComposition::identity),
            studio,
            session.preview(),
        );
        if self.activity_key == Some(key) {
            return;
        }
        self.activity_key = Some(key);
        self.program.clear();
        self.preview.clear();
        if let Some(output) = output {
            for scene in output.scenes() {
                if let Ok(sources) = scene.visible_sources(&model) {
                    self.program.extend(
                        sources
                            .into_iter()
                            .filter(|source| source.kind().has_audio())
                            .map(|source| source.id()),
                    );
                }
            }
            self.program
                .extend(model.global_audio().iter().map(|entry| entry.source()));
        }
        if studio {
            if let Ok(sources) = FrameScene::Live(session.preview()).visible_sources(&model) {
                self.preview.extend(
                    sources
                        .into_iter()
                        .filter(|source| source.kind().has_audio())
                        .map(|source| source.id()),
                );
            }
        }
    }
    fn activity(&self, source: Id) -> &'static str {
        match (
            self.program.contains(&source),
            self.preview.contains(&source),
        ) {
            (true, true) => "Program + Preview",
            (true, false) => "In Program",
            (false, true) => "In Preview",
            _ => "Inactive",
        }
    }
}

pub fn editor(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    draft: &mut AdvancedAudioDraft,
    intents: &mut Vec<Intent>,
) {
    if draft.revision != model.revision() {
        draft.rows.clear();
        draft.revision = model.revision();
    }
    super::audio_monitoring::device(ui,model,draft.session,true,intents);
    ui.separator();
    ui.label(crate::i18n::tr(ui,"Volume and mute changes apply immediately when confirmed. Press Enter to confirm typed volume; Close discards an unconfirmed value."));
    ui.label(crate::i18n::tr(ui,"Filters are applied before sync offset. With a positive offset, filter changes are heard after that delay. Volume and mute follow output time."));
    ui.horizontal_wrapped(|ui| {
        ui.label(crate::i18n::tr(ui, "Volume units"));
        for (unit, caption, help) in [
            (Unit::Db, "dB", "Display volume in decibels."),
            (
                Unit::Percent,
                "%",
                "Display volume as a percentage, from 0 to 2000%.",
            ),
        ] {
            if ui
                .selectable_value(&mut draft.unit, unit, caption)
                .on_hover_text(crate::i18n::tr(ui, help))
                .on_disabled_hover_text(crate::i18n::tr(ui, help))
                .changed()
            {
                draft.rows.clear();
            }
        }
    });
    if let Some(error) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
    }
    let sources: Vec<_> = model
        .sources()
        .iter()
        .filter(|source| source.kind().has_audio())
        .collect();
    if sources.is_empty() {
        ui.label(crate::i18n::tr(
            ui,
            "No audio sources. Add an audio capture or a media source with audio.",
        ));
    }
    egui::ScrollArea::horizontal().id_salt("advanced-audio-columns").show(ui,|ui|{
        ui.set_min_width(1240.0);
        header(ui);
        egui::ScrollArea::vertical().id_salt("advanced-audio-sources").max_height(320.0).auto_shrink([false,true]).show_rows(ui,28.0,sources.len(),|ui,range|{
            for index in range{let source=sources[index];let activity=draft.activity(source.id());let format=draft.formats.get(&source.id()).copied();let row=draft.rows.entry(source.id()).or_insert(RowDraft{gain:source.gain(),muted:source.muted(),dirty:false,force_mono:source.audio_properties().force_mono(),balance:source.audio_properties().balance(),properties_dirty:false,sync_offset_ms:source.audio_properties().sync_offset_ms(),sync_dirty:false,audio_tracks:source.audio_properties().audio_tracks()});ui.push_id(source.id().get(),|ui|{
                ui.horizontal(|ui|{
                    let name=ui.add_sized([160.0,26.0],egui::Label::new(source.name()).truncate()).on_hover_text(source.name());
                    ui.add_sized([112.0,26.0],egui::Label::new(crate::i18n::tr(ui,activity)).truncate()).on_hover_text(crate::i18n::tr(ui,"Where this source is included. Device availability and signal levels are shown in the mixer."));
                    volume(ui,source,row,RowContext{session:draft.session,revision:draft.revision,unit:draft.unit,name:name.id},intents);
                    properties(ui,model,source,row,PropertyContext{session:draft.session,revision:draft.revision,project_mono:model.audio().speakers().channels()==1,format},intents);
                    super::audio_monitoring::mode(ui,draft.session,draft.revision,source.id(),source.audio_monitoring(),intents);
                    if ui.add_sized([104.0,24.0],egui::Button::new(crate::i18n::tr(ui,"Properties")).truncate()).on_hover_text(crate::hints::text(ui,"Source Properties")).on_disabled_hover_text(crate::hints::text(ui,"Source Properties")).clicked(){if let Some(intent)=super::docks::source_properties_intent(source){intents.push(intent);}}
                    let enabled=model.preferences().plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)||model.preferences().plugin_enabled(skylive_core::AUDIO_PAN_PLUGIN)||model.preferences().plugin_enabled(skylive_core::CRAFT_AUDIO_PLUGIN)||!source.audio_filters().is_empty();
                    if ui.add_enabled(enabled,egui::Button::new(crate::i18n::tr(ui,"Filters")).min_size(egui::vec2(80.0,24.0))).on_hover_text(crate::hints::text(ui,"Audio Filters")).on_disabled_hover_text(crate::i18n::tr(ui,"Enable an audio filter module in Tools > Plugins." )).clicked(){intents.push(Intent::OpenDialog(Dialog::audio_filters(source)));}
                });
            });}
        });
    });
    ui.label(crate::i18n::tr(ui,"Monitoring uses its own playback device and ignores track assignments. Source sync offsets use the common Program clock; track assignments do not restart capture or source filters."));
}
fn header(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        for (label, width) in [
            ("Name", 160.0),
            ("Activity", 112.0),
            ("Volume", 115.0),
            ("Mute", 80.0),
            ("Mono", 54.0),
            ("Balance", 130.0),
            ("Sync Offset (ms)", 120.0),
            ("Audio Tracks", 204.0),
            ("Audio Monitoring",210.0),
            ("Properties", 104.0),
            ("Filters", 80.0),
        ] {
            ui.add_sized(
                [width, 24.0],
                egui::Label::new(crate::i18n::tr(ui, label)).truncate(),
            );
        }
    });
    ui.separator();
}
struct RowContext {
    session: u64,
    revision: u64,
    unit: Unit,
    name: egui::Id,
}
fn volume(
    ui: &mut egui::Ui,
    source: &Source,
    row: &mut RowDraft,
    context: RowContext,
    intents: &mut Vec<Intent>,
) {
    let RowContext {
        session,
        revision,
        unit,
        name,
    } = context;
    let mut value = match unit {
        Unit::Db => super::mixer::gain_db(row.gain),
        Unit::Percent => f64::from(row.gain) * 100.0,
    };
    let widget = match unit {
        Unit::Db => egui::DragValue::new(&mut value)
            .range(-1000.0..=super::mixer::gain_db(20.0))
            .clamp_existing_to_range(false)
            .speed(0.1)
            .max_decimals(6)
            .suffix(" dB")
            .custom_formatter(|value, _| super::mixer::format_db(value))
            .custom_parser(super::mixer::parse_db),
        Unit::Percent => egui::DragValue::new(&mut value)
            .range(0.0..=2000.0)
            .clamp_existing_to_range(false)
            .speed(1.0)
            .max_decimals(6)
            .suffix("%"),
    };
    let response=ui.add_sized([115.0,24.0],widget).labelled_by(name).on_hover_text(crate::i18n::tr(ui,"Drag to adjust volume, or type a value and press Enter. Escape cancels the unconfirmed value.")).on_disabled_hover_text(crate::i18n::tr(ui,"Drag to adjust volume, or type a value and press Enter. Escape cancels the unconfirmed value."));
    let mut info = egui::WidgetInfo::drag_value(ui.is_enabled(), value);
    info.label = Some(format!(
        "{} — {}",
        crate::i18n::tr(ui, "Volume"),
        source.name()
    ));
    response.widget_info(|| info.clone());
    if response.changed() {
        row.gain = match unit {
            Unit::Db => super::mixer::db_gain(value),
            Unit::Percent => (value / 100.0) as f32,
        };
        row.dirty = true;
    }
    let focused = response.has_focus() || response.lost_focus();
    if focused && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        row.gain = source.gain();
        row.dirty = false;
        response.surrender_focus();
    }
    let commit = row.dirty
        && (response.drag_stopped()
            || focused && ui.input(|input| input.key_pressed(egui::Key::Enter)));
    let mute = ui
        .allocate_ui(egui::vec2(80.0, 24.0), |ui| {
            super::mixer::mute_button(ui, &mut row.muted)
        })
        .inner;
    let info = egui::WidgetInfo::selected(
        egui::WidgetType::Checkbox,
        ui.is_enabled(),
        row.muted,
        format!("{} — {}", crate::i18n::tr(ui, "Mute"), source.name()),
    );
    mute.widget_info(|| info.clone());
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("advanced-audio-volume", source.id().get())),
            response.rect,
        );
        data.insert_temp(
            egui::Id::new(("advanced-audio-mute", source.id().get())),
            mute.rect,
        );
    });
    if commit || mute.changed() {
        intents.push(Intent::LiveAudio(LiveAudioEdit {
            session,
            revision,
            source: source.id(),
            gain: if commit { row.gain } else { source.gain() },
            muted: row.muted,
        }));
        row.dirty = false;
    }
}
struct PropertyContext {
    session: u64,
    revision: u64,
    project_mono: bool,
    format: Option<NativeAudioFormat>,
}
fn balance_available(project_mono: bool, format: Option<NativeAudioFormat>) -> bool {
    !project_mono
        && format.is_some_and(|format| format.channels == 2 && matches!(format.channel_mask, 0 | 3))
}
fn properties(
    ui: &mut egui::Ui,
    model:&StudioSnapshot,
    source: &Source,
    row: &mut RowDraft,
    context: PropertyContext,
    intents: &mut Vec<Intent>,
) {
    row.audio_tracks=source.audio_properties().audio_tracks();
    let mono=ui.add_sized([54.0,24.0],egui::Checkbox::new(&mut row.force_mono,crate::i18n::tr(ui,"Mono"))).on_hover_text(crate::i18n::tr(ui,"Mix this source to mono. The setting is retained when the project already uses mono output.")).on_disabled_hover_text(crate::i18n::tr(ui,"Mix this source to mono. The setting is retained when the project already uses mono output."));
    let info = egui::WidgetInfo::selected(
        egui::WidgetType::Checkbox,
        ui.is_enabled(),
        row.force_mono,
        format!("{} — {}", crate::i18n::tr(ui, "Mono"), source.name()),
    );
    mono.widget_info(|| info.clone());
    let available = balance_available(context.project_mono, context.format);
    if !available {
        row.balance = source.audio_properties().balance();
        row.properties_dirty = false;
    }
    let mut percentage = row.balance * 100.0;
    let balance=ui.add_enabled_ui(available,|ui|ui.add_sized([130.0,24.0],egui::DragValue::new(&mut percentage).range(0.0..=100.0).clamp_existing_to_range(false).speed(1.0).max_decimals(4).suffix("%"))).inner
        .on_hover_text(crate::i18n::tr(ui,"Stereo balance: 0% left, 50% center, 100% right. Values between 40% and 60% snap to center. Confirm a typed value with Enter."))
        .on_disabled_hover_text(crate::i18n::tr(ui,if context.project_mono{"Balance is unavailable when the project output is mono."}else if context.format.is_none(){"Balance requires a confirmed original stereo format. Start this source to identify its format."}else{"Balance is available only for an original stereo source."}));
    let mut info = egui::WidgetInfo::drag_value(balance.enabled(), f64::from(percentage));
    info.label = Some(format!(
        "{} — {}",
        crate::i18n::tr(ui, "Balance"),
        source.name()
    ));
    balance.widget_info(|| info.clone());
    if available && balance.changed() {
        row.balance = if percentage > 40.0 && percentage < 60.0 {
            0.5
        } else {
            percentage / 100.0
        };
        row.properties_dirty = true;
    }
    let focused = balance.has_focus() || balance.lost_focus();
    if focused && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        row.balance = source.audio_properties().balance();
        row.properties_dirty = false;
        balance.surrender_focus();
    }
    let commit = available
        && row.properties_dirty
        && (balance.drag_stopped()
            || focused && ui.input(|input| input.key_pressed(egui::Key::Enter)));
    let sync=ui.add_sized([120.0,24.0],egui::DragValue::new(&mut row.sync_offset_ms)
        .range(skylive_core::MIN_AUDIO_SYNC_OFFSET_MS..=skylive_core::MAX_AUDIO_SYNC_OFFSET_MS)
        .clamp_existing_to_range(false).speed(1.0).suffix(" ms"))
        .on_hover_text(crate::i18n::tr(ui,"Positive offset delays this source; negative offset advances it using buffered future audio. Range: -950 to 20000 ms. Native capture timestamps are unchanged. Press Enter to confirm; Escape cancels."));
    let mut sync_info=egui::WidgetInfo::drag_value(sync.enabled(),f64::from(row.sync_offset_ms));
    sync_info.label=Some(format!("{} — {}",crate::i18n::tr(ui,"Sync Offset (ms)"),source.name()));
    sync.widget_info(||sync_info.clone());
    if sync.changed(){row.sync_dirty=true;}
    let sync_focused=sync.has_focus()||sync.lost_focus();
    if sync_focused&&ui.input(|input|input.key_pressed(egui::Key::Escape)) {
        row.sync_offset_ms=source.audio_properties().sync_offset_ms();row.sync_dirty=false;sync.surrender_focus();
    }
    let sync_commit=row.sync_dirty&&(sync.drag_stopped()||sync_focused&&ui.input(|input|input.key_pressed(egui::Key::Enter)));
    let mut tracks_changed=false;
    ui.allocate_ui_with_layout(egui::vec2(204.0,24.0),egui::Layout::left_to_right(egui::Align::Center),|ui|{
        ui.spacing_mut().item_spacing.x=2.0;
        for number in 1u8..=6{
            let track=skylive_core::AudioTrackId::new(number).expect("bounded audio track");
            let mut selected=row.audio_tracks.contains(track);
            let authored_name=model.profiles().selected().recording().audio_tracks().entry(track).name();
            let title=if authored_name.is_empty(){crate::i18n::tf(ui,"Track {track}",&[("{track}",number.to_string())])}else{authored_name.to_owned()};
            let response=ui.add_sized([31.0,24.0],egui::Checkbox::new(&mut selected,number.to_string()))
                .on_hover_text(format!("{}\n{}",title,crate::i18n::tr(ui,"Assign this source to audio mixes 1 to 6. Recording saves its selected mixes; Streaming, Replay and NDI each use one chosen mix. Clear all boxes for monitoring, meters and sidechain only.")));
            response.widget_info(||egui::WidgetInfo::selected(egui::WidgetType::Checkbox,response.enabled(),selected,format!("{} — {}",title,source.name())));
            if response.changed(){row.audio_tracks=row.audio_tracks.with(track,selected);tracks_changed=true;}
        }
    });
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("advanced-audio-mono", source.id().get())),
            mono.rect,
        );
        data.insert_temp(
            egui::Id::new(("advanced-audio-balance", source.id().get())),
            (balance.rect, available),
        );
    });
    if mono.changed() || commit || sync_commit || tracks_changed {
        intents.push(Intent::LiveAudioProperties(LiveAudioPropertyEdit {
            session: context.session,
            revision: context.revision,
            source: source.id(),
            force_mono: row.force_mono,
            audio_tracks:row.audio_tracks,
            sync_offset_ms: if sync_commit {row.sync_offset_ms}else{source.audio_properties().sync_offset_ms()},
            balance: if commit {
                row.balance
            } else {
                source.audio_properties().balance()
            },
        }));
        row.properties_dirty = false;
        row.sync_dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(
        context: &egui::Context,
        session: &Session,
        draft: &mut AdvancedAudioDraft,
        events: Vec<egui::Event>,
    ) -> Vec<Intent> {
        let mut intents = vec![];
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor(ui, &session.snapshot(), draft, &mut intents)
                });
            },
        );
        intents
    }
    fn balance(
        context: &egui::Context,
        session: &Session,
        draft: &mut AdvancedAudioDraft,
        source: Id,
        text: &str,
    ) -> Vec<Intent> {
        frame(context, session, draft, vec![]);
        frame(context, session, draft, vec![]);
        let (pos, enabled) = context
            .data_mut(|data| {
                data.get_temp::<(egui::Rect, bool)>(egui::Id::new((
                    "advanced-audio-balance",
                    source.get(),
                )))
            })
            .unwrap();
        assert!(enabled);
        let pos = pos.center();
        for pressed in [true, false] {
            frame(
                context,
                session,
                draft,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        frame(
            context,
            session,
            draft,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Text(text.into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
    }
    #[test]
    fn balance_gate_uses_original_format_and_project_layout_not_converted_channel_count() {
        for (mask, expected) in [(0, true), (3, true), (12, false)] {
            assert_eq!(
                balance_available(
                    false,
                    Some(NativeAudioFormat {
                        sample_rate: 48000,
                        channels: 2,
                        channel_mask: mask
                    })
                ),
                expected
            );
        }
        assert!(!balance_available(false, None));
        assert!(!balance_available(
            true,
            Some(NativeAudioFormat {
                sample_rate: 48000,
                channels: 2,
                channel_mask: 3
            })
        ));
        assert!(!balance_available(
            false,
            Some(NativeAudioFormat {
                sample_rate: 48000,
                channels: 1,
                channel_mask: 4
            })
        ));
    }
    #[test]
    fn balance_widget_preserves_authored_unsnapped_value_then_snaps_confirmed_edit_as_one_undo() {
        let mut session = Session::new();
        session
            .apply(skylive_core::Command::AddAudioCaptureSource {
                scene: session.preview(),
                name: "Stereo".into(),
                binding: skylive_core::AudioCaptureBinding::new(
                    skylive_core::AudioFlow::Input,
                    skylive_core::AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(skylive_core::Command::SetSourceAudioProperties {
                source,
                properties: skylive_core::SourceAudioProperties::new(false, 0.49).unwrap(),
            })
            .unwrap();
        let before = session.snapshot();
        let context = egui::Context::default();
        context.enable_accesskit();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let mut draft = AdvancedAudioDraft::new(session.identity());
        assert!(frame(&context, &session, &mut draft, vec![]).is_empty());
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let (_, available) = context
            .data_mut(|data| {
                data.get_temp::<(egui::Rect, bool)>(egui::Id::new((
                    "advanced-audio-balance",
                    source.get(),
                )))
            })
            .unwrap();
        assert!(!available);
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_properties()
                .balance(),
            0.49
        );
        // Injected metadata represents a fake original stereo port. No native
        // stream or Program-channel meter is used to infer its eligibility.
        draft.formats.insert(
            source,
            NativeAudioFormat {
                sample_rate: 48000,
                channels: 2,
                channel_mask: 3,
            },
        );
        assert!(frame(&context, &session, &mut draft, vec![]).is_empty());
        assert_eq!(
            draft.rows[&source].balance, 0.49,
            "painting must not snap a retained authored value"
        );
        let intents = balance(&context, &session, &mut draft, source, "48");
        let [Intent::LiveAudioProperties(edit)] = intents.as_slice() else {
            panic!("confirmed balance must emit one typed source-property edit")
        };
        assert_eq!(edit.balance, 0.5);
        assert!(!edit.force_mono);
        session
            .apply(skylive_core::Command::SetSourceAudioProperties {
                source,
                properties: skylive_core::SourceAudioProperties::new(edit.force_mono, edit.balance)
                    .unwrap(),
            })
            .unwrap();
        session.apply(skylive_core::Command::Undo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_properties()
                .balance(),
            0.49
        );
    }
}
