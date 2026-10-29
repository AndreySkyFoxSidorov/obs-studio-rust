//! Compact native Audio Mixer. Immutable snapshots in; typed intentions out.
//! No device access, PCM retention or engine-side processing.
use super::{AudioEdit, Intent, ProgramAudioView, dialogs::Dialog};
use eframe::egui;
use skylive_core::{AudioSlot, Command, Id, Source, SourceKind, StudioSnapshot};
use skylive_host::{
    audio::ChannelMeter,
    audio_capture::{AudioCaptureSnapshot, ProgramTransportStats},
    transition::FrameScene,
};
use std::collections::HashSet;
mod controls;
mod projection;
mod vertical;

pub struct MixerSubmission {
    pub session: u64,
    pub revision: u64,
    pub command: Command,
}

const SILENCE_DB: f64 = -1000.0;
const MIN_METER_DB: f32 = -60.0;
const WARNING_DB: f32 = -20.0;
const ERROR_DB: f32 = -9.0;
const FRESH_SECONDS: f64 = 0.5;
const PEAK_HOLD_SECONDS: f64 = 20.0;
const DECAY_DB_PER_SECOND: f32 = 11.76;
const NOMINAL: egui::Color32 = egui::Color32::from_rgb(0x4c, 0xff, 0x4c);
const WARNING: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0x4c);
const ERROR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x4c, 0x4c);

const LIVE_GAIN_PREVIEW: &str = "audio-mixer-live-gain-preview";
/// Cleared each frame so a hidden or cancelled mixer cannot retain a transient gain.
pub(crate) fn clear_gain_preview(context:&egui::Context){
    context.data_mut(|data|data.remove::<skylive_host::audio::MixerGainPreview>(egui::Id::new(LIVE_GAIN_PREVIEW)));
}
pub(crate) fn gain_preview(context:&egui::Context)->Option<skylive_host::audio::MixerGainPreview>{
    context.data(|data|data.get_temp(egui::Id::new(LIVE_GAIN_PREVIEW)))
}

fn meter_segments(ui:&egui::Ui)->[(f32,f32,egui::Color32,egui::Color32);3] {
    use skylive_core::InterfaceColor::*;
    let settings=crate::theme::accessibility(ui.ctx());
    let roles=[(MeterGreenActive,MeterGreen),(MeterYellowActive,MeterYellow),(MeterRedActive,MeterRed)];
    let spans=[(MIN_METER_DB,WARNING_DB),(WARNING_DB,ERROR_DB),(ERROR_DB,0.0)];
    let defaults=[NOMINAL,WARNING,ERROR];
    std::array::from_fn(|index| {
        let (active,inactive)=roles[index];let (low,high)=spans[index];
        (low,high,if settings.override_colors(){crate::theme::interface_color(ui,active)}else{defaults[index]},crate::theme::interface_color(ui,inactive))
    })
}

pub(super) struct MixerViewState<'a> {
    pub(super) session: u64,
    pub(super) audio: &'a AudioCaptureSnapshot,
    pub(super) program_views: &'a [FrameScene],
    pub(super) preview: Option<Id>,
    pub(super) program_audio: Option<&'a ProgramAudioView>,
    pub(super) program_status: Option<&'a ProgramTransportStats>,
}

#[derive(Clone, Copy)]
struct VolumeDraft {
    session: u64,
    revision: u64,
    canonical: f32,
    canonical_muted: bool,
    value: f32,
    dirty: bool,
}
impl VolumeDraft {
    fn new(session: u64, revision: u64, source: &Source) -> Self {
        Self {
            session,
            revision,
            canonical: source.gain(),
            canonical_muted: source.muted(),
            value: source.gain(),
            dirty: false,
        }
    }
    fn matches(&self, session: u64, revision: u64, source: &Source) -> bool {
        self.session == session
            && self.revision == revision
            && self.canonical == source.gain()
            && self.canonical_muted == source.muted()
    }
}

#[derive(Clone)]
struct MeterState {
    peak_mode: skylive_core::MixerPeakMode,
    token: Option<(u64, u64, u64)>,
    last_seen: f64,
    last_painted: f64,
    channels: usize,
    display: [f32; 8],
    rms: [f32; 8],
    held: [f32; 8],
    hold_until: [f64; 8],
    clip_until: f64,
}
impl Default for MeterState {
    fn default() -> Self {
        Self {
            peak_mode: skylive_core::MixerPeakMode::Sample,
            token: None,
            last_seen: 0.0,
            last_painted: 0.0,
            channels: 0,
            display: [MIN_METER_DB; 8],
            rms: [MIN_METER_DB; 8],
            held: [MIN_METER_DB; 8],
            hold_until: [0.0; 8],
            clip_until: 0.0,
        }
    }
}
#[derive(Clone)]
struct SourcePresentation {
    source: Id,
    volume: VolumeDraft,
    meter: MeterState,
}
#[derive(Clone, Default)]
struct MixerPresentation {
    session: u64,
    rows: Vec<SourcePresentation>,
}

struct Levels<'a> {
    meters: &'a [ChannelMeter],
    scale: f64,
    token: (u64, u64, u64),
    post_filter: bool,
}

fn source_levels<'a>(
    source: &Source,
    session: u64,
    model: &StudioSnapshot,
    capture: &'a AudioCaptureSnapshot,
    program: Option<&'a ProgramAudioView>,
) -> Option<Levels<'a>> {
    if !capture.source_runtime_active(source.id()){return None;}
    if let Some(program) = program.filter(|program| {
        program.key.session == session
            && program.key.settings == model.audio()
            && program.source_meter_revision == Some(model.revision())
    }) {
        if let Some(meter) = program
            .source_meters
            .iter()
            .find(|meter| meter.source == source.id())
        {
            return Some(Levels {
                meters: &meter.channels,
                scale: 1.0,
                token: (1, program.key.generation, program.start_frame),
                post_filter: true,
            });
        }
    }
    if model.preferences().mixer_display().peak_mode() == skylive_core::MixerPeakMode::TruePeakEstimate {
        return None;
    }
    let binding = source.kind().audio_capture_binding()?;
    // Peak/RMS scalars cannot reconstruct channel mixing or cancellation.
    // Wait for the actual Program source meters after channel properties change.
    if !source.audio_properties().is_default() {
        return None;
    }
    capture
        .packet(source.id())
        .filter(|packet| capture.session() == session && &packet.binding == binding)
        .map(|packet| Levels {
            meters: &packet.meters,
            scale: if source.muted() {
                0.0
            } else {
                f64::from(source.gain())
            },
            token: (0, packet.stream_generation, packet.sequence),
            post_filter: false,
        })
}

pub(super) fn gain_db(gain: f32) -> f64 {
    if gain <= 0.0 {
        SILENCE_DB
    } else {
        20.0 * f64::from(gain).log10()
    }
}
pub(super) fn db_gain(db: f64) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        (10.0f64.powf(db / 20.0) as f32).clamp(0.0, 20.0)
    }
}
// Exact supplied OBS_FADER_LOG attenuation law, not a generic logarithmic UI.
fn gain_deflection(gain: f32) -> f32 {
    if gain <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * gain.log10();
    if db >= 0.0 {
        return 1.0;
    }
    if db <= -96.0 {
        return 0.0;
    }
    (-(-db + 6.0).log10() - (-2.008_600_2_f32)) / (-0.778_151_3_f32 - (-2.008_600_2_f32))
}
fn deflection_gain(deflection: f32) -> f32 {
    if deflection <= 0.0 {
        0.0
    } else if deflection >= 1.0 {
        1.0
    } else {
        10.0f32.powf((-(96.0 + 6.0) * ((96.0 + 6.0) / 6.0f32).powf(-deflection) + 6.0) / 20.0)
    }
}
pub(super) fn format_db(value: f64) -> String {
    if value <= SILENCE_DB {
        "−∞".into()
    } else {
        format!("{value:+.2}")
    }
}
pub(super) fn parse_db(value: &str) -> Option<f64> {
    let value = value.trim();
    if matches!(value, "−∞" | "-∞" | "-inf" | "-Infinity") {
        Some(SILENCE_DB)
    } else {
        value.parse::<f64>().ok().filter(|value| value.is_finite())
    }
}
fn level_db(level: f64) -> f32 {
    if level > 0.0 && level.is_finite() {
        (20.0 * level.log10()) as f32
    } else {
        MIN_METER_DB
    }
}
fn meter_position(db: f32) -> f32 {
    ((db - MIN_METER_DB) / -MIN_METER_DB).clamp(0.0, 1.0)
}

pub(super) fn show(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    _scene: Id,
    state: MixerViewState<'_>,
    intents: &mut Vec<Intent>,
) {
    let MixerViewState {
        session,
        audio,
        program_views,
        preview,
        program_audio,
        program_status,
    } = state;
    ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,"Audio Mixer")).strong()).sense(egui::Sense::click())).context_menu(|ui|controls::options_menu(ui,model,session,program_audio,program_status,intents));
    ui.separator();
    controls::toolbar(ui, model, session, program_audio, program_status, intents);
    ui.separator();
    let mut rows = projection::rows(model, session, program_views, preview);
    for row in &mut rows {if !audio.source_runtime_active(row.source.id()){row.active=false;row.preview=false;}}
    rows.retain(|row|row.active||row.preview||row.global||row.pinned||model.preferences().mixer_display().show_inactive());
    let memory_id = ui.id().with("mixer-presentation");
    let mut presentation = ui
        .data_mut(|data| data.get_temp::<MixerPresentation>(memory_id))
        .unwrap_or_default();
    if presentation.session != session {
        presentation = MixerPresentation {
            session,
            rows: Vec::new(),
        };
    }
    presentation
        .rows
        .retain(|entry| rows.iter().any(|row| row.source.id() == entry.source));
    let now = ui.input(|input| input.time);
    let vertical =
        model.preferences().mixer_display().layout() == skylive_core::MixerLayout::Vertical;
    let viewport_height = ui.available_height().max(1.0);
    let scroll = if vertical {
        egui::ScrollArea::both()
    } else {
        egui::ScrollArea::vertical()
    };
    let scroll = scroll
        .id_salt(("audio-mixer-sources", vertical))
        .auto_shrink([false, false])
        .max_height(viewport_height)
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.label(crate::i18n::tr(
                    ui,
                    "No active audio sources in the selected scene or global slots.",
                ));
            }
            let mut draw = |ui: &mut egui::Ui| {
                for projected in &rows {
                    let source = projected.source;
                    if !presentation
                        .rows
                        .iter()
                        .any(|entry| entry.source == source.id())
                    {
                        presentation.rows.push(SourcePresentation {
                            source: source.id(),
                            volume: VolumeDraft::new(session, model.revision(), source),
                            meter: MeterState::default(),
                        });
                    }
                    let entry = presentation
                        .rows
                        .iter_mut()
                        .find(|entry| entry.source == source.id())
                        .expect("inserted source presentation");
                    if !entry.volume.matches(session, model.revision(), source) {
                        entry.volume = VolumeDraft::new(session, model.revision(), source);
                    }
                    if !audio.source_runtime_active(source.id()){entry.meter=MeterState::default();}
                    let peak_mode = model.preferences().mixer_display().peak_mode();
                    if entry.meter.peak_mode != peak_mode {
                        entry.meter = MeterState::default();
                        entry.meter.peak_mode = peak_mode;
                    }
                    let levels = source_levels(source, session, model, audio, program_audio);
                    let channel_count = levels
                        .as_ref()
                        .map_or(entry.meter.channels.clamp(1, 8), |levels| {
                            levels.meters.len().clamp(1, 8)
                        });
                    let height = if vertical {
                        viewport_height.max(130.0)
                    } else {
                        84.0 + channel_count as f32 * 6.0
                            + if levels.is_none() { 32.0 } else { 0.0 }
                    };
                    let width = if vertical {
                        136.0
                    } else {
                        ui.available_width().max(1.0)
                    };
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
                    if !ui.is_rect_visible(rect) {
                        entry.volume = VolumeDraft::new(session, model.revision(), source);
                        continue;
                    }
                    let mut row = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("audio-source", source.id().get()))
                            .max_rect(rect.shrink(3.0))
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    row.set_clip_rect(rect.intersect(ui.clip_rect()));
                    row.style_mut().spacing.item_spacing = egui::vec2(4.0, 2.0);
                    let category = projected.category();
                    if category.starts_with("Basic.") {
                        row.small(crate::i18n::tr_key(&row, category));
                    } else {
                        row.small(crate::i18n::tr(&row, category));
                    }
                    source_strip(
                        &mut row,
                        model,
                        source,
                        session,
                        projected.editable,
                        audio,
                        levels.as_ref(),
                        &mut entry.volume,
                        &mut entry.meter,
                        now,
                        intents,
                    );
                    if vertical {
                        ui.painter().vline(
                            rect.right(),
                            rect.y_range(),
                            egui::Stroke::new(1.0, crate::theme::border(ui)),
                        );
                    } else {
                        ui.painter().hline(
                            rect.x_range(),
                            rect.bottom(),
                            egui::Stroke::new(1.0, crate::theme::border(ui)),
                        );
                    }
                }
            };
            if vertical {
                ui.horizontal(|ui| draw(ui));
            } else {
                draw(ui);
            }
        });
    #[cfg(test)]
    ui.data_mut(|data| data.insert_temp(egui::Id::new("audio-mixer-viewport"), scroll.inner_rect));
    #[cfg(not(test))]
    let _ = scroll;
    ui.data_mut(|data| data.insert_temp(memory_id, presentation));
}

#[allow(clippy::too_many_arguments)]
fn source_strip(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: &Source,
    session: u64,
    editable: bool,
    capture: &AudioCaptureSnapshot,
    levels: Option<&Levels<'_>>,
    draft: &mut VolumeDraft,
    meter: &mut MeterState,
    now: f64,
    intents: &mut Vec<Intent>,
) {
    if model.preferences().mixer_display().layout() == skylive_core::MixerLayout::Vertical {
        return vertical::strip(
            ui, model, source, session, editable, capture, levels, draft, meter, now, intents,
        );
    }
    let locked = source.mixer_state().volume_locked();
    let mut db = gain_db(draft.value);
    let mut numeric = None;
    ui.horizontal(|ui| {
        let name_width = (ui.available_width() - 124.0).max(12.0);
        let name = egui::menu::menu_custom_button(
            ui,
            egui::Button::new(egui::RichText::new(source.name()).strong())
                .frame(false).truncate().min_size(egui::vec2(name_width, 22.0)),
            |ui| source_options(ui, model, source, session, capture, levels, editable, intents),
        ).response;
        name.clone()
            .on_hover_ui(|ui| source_details(ui, source, capture, levels, editable));
        name.clone().context_menu(|ui|source_options(ui,model,source,session,capture,levels,editable,intents));
        let response = ui
            .add_enabled_ui(editable && !locked, |ui| {
                // Canonical input changes replace the edit widget identity, so an
                // old focused text buffer cannot republish a stale gain on Enter.
                ui.push_id(
                    (
                        session,
                        model.revision(),
                        source.gain().to_bits(),
                        source.muted(),
                    ),
                    |ui| {
                        ui.add(
                            egui::DragValue::new(&mut db)
                                .range(SILENCE_DB..=gain_db(20.0))
                                .clamp_existing_to_range(false)
                                .speed(0.1)
                                .max_decimals(6)
                                .suffix(" dB")
                                .custom_formatter(|value, _| format_db(value))
                                .custom_parser(parse_db),
                        )
                        .labelled_by(name.id)
                        .on_hover_text(crate::hints::text(ui, "Volume"))
                    },
                )
                .inner
            })
            .inner;
        numeric = Some(response);
        let source_options_info = egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            crate::i18n::tr(ui, "Source Properties"),
        );
        ui.menu_button("...", |ui| {
            source_options(
                ui, model, source, session, capture, levels, editable, intents,
            )
        })
        .response
        .on_hover_text(crate::hints::text(ui, "Source Properties"))
        .widget_info(|| source_options_info.clone());
    });
    let numeric = numeric.expect("numeric volume response");
    let mut numeric_info = egui::WidgetInfo::drag_value(numeric.enabled(), db);
    numeric_info.label = Some(format!("{} - {}", crate::i18n::tr(ui, "Volume"), source.name()));
    numeric.widget_info(|| numeric_info.clone());
    if numeric.changed() {
        draft.value = db_gain(db);
        draft.dirty = true;
    }
    let mut slider = None;
    let mut mute_changed = false;
    let mut muted = source.muted();
    ui.add_enabled_ui(editable, |ui| {
        ui.horizontal(|ui| {
            let mute = mute_button(ui, &mut muted);
            mute_changed = mute.changed();
            monitor_button(ui,model,source,session,editable,intents);
            let mut deflection = gain_deflection(draft.value);
            let width = (ui.available_width() - 2.0).max(16.0);
            let response = ui
                .add_enabled_ui(!locked, |ui| {
                    ui.add_sized(
                        [width, 22.0],
                        egui::Slider::new(&mut deflection, 0.0..=1.0).show_value(false),
                    )
                    .on_hover_text(crate::hints::text(ui, "Volume"))
                })
                .inner;
            let widget_info = {
                egui::WidgetInfo::slider(
                    ui.is_enabled() && !locked,
                    gain_db(draft.value),
                    format!("{} · {}", source.name(), crate::i18n::tr(ui, "Volume")),
                )
            };
            response.widget_info(|| widget_info.clone());
            if response.changed() {
                draft.value = deflection_gain(deflection);
                draft.dirty = true;
            }
            slider = Some(response);
        });
    });
    let slider = slider.expect("volume slider response");
    #[cfg(test)]
    ui.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("audio-fader", source.id().get())),
            slider.rect,
        );
        data.insert_temp(
            egui::Id::new(("audio-fader-widget", source.id().get())),
            slider.id,
        );
        data.insert_temp(
            egui::Id::new(("audio-gain", source.id().get())),
            numeric.rect,
        );
    });
    finish_edit(
        ui,
        model,
        source,
        session,
        editable,
        draft,
        &slider,
        &numeric,
        muted,
        mute_changed,
        intents,
    );
    let available = update_meter(meter, levels, now);
    #[cfg(test)]
    if let Some(levels) = levels.filter(|_| available) {
        ui.data_mut(|data| {
            for (channel, value) in levels.meters.iter().enumerate() {
                data.insert_temp(
                    egui::Id::new(("audio-meter", source.id().get(), channel)),
                    (value.peak * levels.scale, value.rms * levels.scale),
                );
            }
        });
    }
    paint_meters(ui, source.id(), meter, available, source.muted());
    if !available {
        ui.add(egui::Label::new(crate::i18n::tr(ui, "Waiting for audio levels")).wrap());
    }
    if !editable {
        ui.label(crate::i18n::tr(
            ui,
            "Retired Program source · restore its authored reference to edit",
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_edit(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: &Source,
    session: u64,
    editable: bool,
    draft: &mut VolumeDraft,
    slider: &egui::Response,
    numeric: &egui::Response,
    muted: bool,
    mute_changed: bool,
    intents: &mut Vec<Intent>,
) {
    let focused =
        slider.has_focus() || slider.lost_focus() || numeric.has_focus() || numeric.lost_focus();
    if (focused && ui.input(|input| input.key_pressed(egui::Key::Escape)))
        || (draft.dirty && !ui.input(|input| input.focused)) {
        *draft = VolumeDraft::new(session, model.revision(), source);
        slider.surrender_focus();
        numeric.surrender_focus();
    }
    if draft.dirty && !focused && !slider.dragged() && !numeric.dragged()
        && !slider.drag_stopped() && !numeric.drag_stopped()
        && !slider.changed() && !numeric.changed() {
        *draft = VolumeDraft::new(session, model.revision(), source);
    }
    let commit = draft.dirty
        && (slider.drag_stopped()
            || numeric.drag_stopped()
            || (slider.changed() && !slider.is_pointer_button_down_on())
            || slider.lost_focus()
            || numeric.lost_focus()
            || (focused && ui.input(|input| input.key_pressed(egui::Key::Enter))));
    if controls::view_action_pending(intents) || source.mixer_state().volume_locked() {
        draft.value = source.gain();
        draft.dirty = false;
    }
    if editable && draft.dirty && (slider.dragged() || numeric.dragged()) {
        let preview=skylive_host::audio::MixerGainPreview {session,revision:model.revision(),source:source.id(),gain:draft.value};
        ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new(LIVE_GAIN_PREVIEW),preview));
    }
    let commit = commit && draft.dirty;
    if editable && (commit || mute_changed) {
        controls::submit(
            intents,
            session,
            model,
            Command::SetMixerAudio {
                source: source.id(),
                gain: draft.value,
                muted: mute_changed.then_some(muted),
            },
        );
        draft.dirty = false;
        draft.canonical = draft.value;
        draft.canonical_muted = if mute_changed {muted} else {source.muted()};
    }
}

/// OBS monitor toggle: disabling MonitorOnly/MonitorAndOutput returns to Off;
/// enabling from Off selects MonitorAndOutput without altering mute or gain.
pub(super) fn monitor_button(ui:&mut egui::Ui,model:&StudioSnapshot,source:&Source,session:u64,editable:bool,intents:&mut Vec<Intent>){
    use skylive_core::AudioMonitoringMode;
    let active=!source.audio_monitoring().is_off();
    let available=super::audio_monitoring::available(ui,session);
    let action=if active{"Basic.AudioMixer.Monitoring.Disable"}else{"Basic.AudioMixer.Monitoring.Enable"};
    let label=crate::i18n::tr_key(ui,action);
    let accessible=format!("{} · {}",source.name(),label);
    let response=ui.add_enabled_ui(editable&&(active||available),|ui|{
        let (rect,response)=ui.allocate_exact_size(egui::vec2(25.0,22.0),egui::Sense::click());
        let visuals=ui.style().interact_selectable(&response,active);
        ui.painter().rect(rect,3.0,visuals.bg_fill,visuals.bg_stroke,egui::StrokeKind::Inside);
        let color=visuals.fg_stroke.color;
        let c=rect.center();
        let points=vec![c+egui::vec2(-6.0,2.0),c+egui::vec2(-6.0,-2.0),c+egui::vec2(-4.0,-5.0),c+egui::vec2(0.0,-6.0),c+egui::vec2(4.0,-5.0),c+egui::vec2(6.0,-2.0),c+egui::vec2(6.0,2.0)];
        ui.painter().add(egui::Shape::line(points,egui::Stroke::new(1.5,color)));
        ui.painter().rect_filled(egui::Rect::from_center_size(c+egui::vec2(-6.0,3.0),egui::vec2(3.0,6.0)),1.0,color);
        ui.painter().rect_filled(egui::Rect::from_center_size(c+egui::vec2(6.0,3.0),egui::vec2(3.0,6.0)),1.0,color);
        response.widget_info(||egui::WidgetInfo::selected(egui::WidgetType::Checkbox,ui.is_enabled(),active,&accessible));
        response
    }).inner.on_hover_text(label).on_disabled_hover_text(crate::i18n::tr(ui,"Physical audio monitoring requires the Windows WASAPI output backend"));
    if response.clicked(){
        controls::submit(intents,session,model,Command::SetSourceAudioMonitoring{source:source.id(),mode:if active{AudioMonitoringMode::Off}else{AudioMonitoringMode::MonitorAndOutput}});
    }
}
pub(super) fn mute_button(ui: &mut egui::Ui, muted: &mut bool) -> egui::Response {
    let label = crate::i18n::tr(ui, if *muted {"Unmute"} else {"Mute"});
    let font = egui::FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.clone(), font, ui.visuals().text_color());
    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(25.0 + galley.size().x, 22.0),
        egui::Sense::click(),
    );
    if response.clicked() {
        *muted = !*muted;
        response.mark_changed();
    }
    let widget_info =
        { egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *muted, &label) };
    response.widget_info(|| widget_info.clone());
    let visuals = ui.style().interact_selectable(&response, *muted);
    ui.painter().rect(
        rect,
        3.0,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let color = visuals.fg_stroke.color;
    let center = egui::pos2(rect.left() + 10.0, rect.center().y);
    let speaker = vec![
        center + egui::vec2(-5.0, -3.0),
        center + egui::vec2(-2.0, -3.0),
        center + egui::vec2(2.0, -6.0),
        center + egui::vec2(2.0, 6.0),
        center + egui::vec2(-2.0, 3.0),
        center + egui::vec2(-5.0, 3.0),
    ];
    ui.painter().add(egui::Shape::convex_polygon(
        speaker,
        color,
        egui::Stroke::NONE,
    ));
    if *muted {
        ui.painter().line_segment(
            [
                center + egui::vec2(4.0, -4.0),
                center + egui::vec2(9.0, 4.0),
            ],
            egui::Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(4.0, 4.0),
                center + egui::vec2(9.0, -4.0),
            ],
            egui::Stroke::new(1.5, color),
        );
    } else {
        ui.painter().line_segment(
            [
                center + egui::vec2(5.0, -3.0),
                center + egui::vec2(7.0, 0.0),
            ],
            egui::Stroke::new(1.2, color),
        );
        ui.painter().line_segment(
            [center + egui::vec2(7.0, 0.0), center + egui::vec2(5.0, 3.0)],
            egui::Stroke::new(1.2, color),
        );
    }
    ui.painter().galley(
        egui::pos2(rect.left() + 23.0, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    response
        .on_hover_text(label.clone())
        .on_disabled_hover_text(label)
}

fn update_meter(state: &mut MeterState, levels: Option<&Levels<'_>>, now: f64) -> bool {
    let dt = ((now - state.last_painted).max(0.0) as f32).min(5.0);
    state.last_painted = now;
    let valid = levels.filter(|levels| {
        levels.meters.len() <= 8
            && !levels.meters.is_empty()
            && levels.scale.is_finite()
            && levels.scale >= 0.0
            && levels.meters.iter().all(|meter| {
                meter.peak.is_finite()
                    && meter.rms.is_finite()
                    && meter.peak >= 0.0
                    && meter.rms >= 0.0
                    && (meter.peak * levels.scale).is_finite()
                    && (meter.rms * levels.scale).is_finite()
            })
    });
    if let Some(levels) = valid {
        if state.token != Some(levels.token) {
            state.token = Some(levels.token);
            state.last_seen = now;
        }
        state.channels = levels.meters.len();
    }
    let available =
        valid.is_some() && state.token.is_some() && now - state.last_seen <= FRESH_SECONDS;
    for channel in 0..state.channels {
        let peak = if available {
            level_db(
                valid.expect("available levels").meters[channel].peak
                    * valid.expect("available levels").scale,
            )
        } else {
            MIN_METER_DB
        };
        let rms = if available {
            level_db(
                valid.expect("available levels").meters[channel].rms
                    * valid.expect("available levels").scale,
            )
        } else {
            MIN_METER_DB
        };
        state.display[channel] = peak
            .max(state.display[channel] - DECAY_DB_PER_SECOND * dt)
            .clamp(MIN_METER_DB, 0.0);
        state.rms[channel] = rms.clamp(MIN_METER_DB, 0.0);
        if peak >= state.held[channel] || now >= state.hold_until[channel] {
            state.held[channel] = peak.clamp(MIN_METER_DB, 0.0);
            state.hold_until[channel] = now + PEAK_HOLD_SECONDS;
        }
        if available && peak >= 0.0 {
            state.clip_until = now + 1.0;
        }
    }
    available
}

fn paint_meters(ui: &mut egui::Ui, source: Id, state: &MeterState, available: bool, muted: bool) {
    let count = state.channels.clamp(1, 8);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), count as f32 * 6.0 + 14.0),
        egui::Sense::hover(),
    );
    let bar_width = rect.width();
    let muted_color = egui::Color32::from_gray(95);
    for channel in 0..count {
        let bar = egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, channel as f32 * 6.0),
            egui::vec2(bar_width, 4.0),
        );
        for (low, high, color, inactive) in meter_segments(ui) {
            let segment = egui::Rect::from_min_max(
                egui::pos2(bar.left() + bar_width * meter_position(low), bar.top()),
                egui::pos2(bar.left() + bar_width * meter_position(high), bar.bottom()),
            );
            ui.painter().rect_filled(
                segment,
                0.0,
                if muted {
                    egui::Color32::from_gray(45)
                } else {
                    inactive
                },
            );
            if available {
                let end = state.display[channel].min(high);
                if end > low {
                    let fill = egui::Rect::from_min_max(
                        segment.min,
                        egui::pos2(bar.left() + bar_width * meter_position(end), bar.bottom()),
                    );
                    ui.painter()
                        .rect_filled(fill, 0.0, if muted { muted_color } else { color });
                }
            }
        }
        if available {
            let rms = bar.left() + bar_width * meter_position(state.rms[channel]);
            ui.painter().vline(
                rms,
                bar.y_range(),
                egui::Stroke::new(1.0, egui::Color32::BLACK),
            );
        }
        let held = bar.left() + bar_width * meter_position(state.held[channel]);
        ui.painter().vline(
            held,
            bar.y_range(),
            egui::Stroke::new(
                1.0,
                if available {
                    egui::Color32::WHITE
                } else {
                    muted_color
                },
            ),
        );
    }
    let clipped = ui.input(|input| input.time) < state.clip_until;
    let ticks: &[i32] = if bar_width < 210.0 {
        &[-60, -30, 0]
    } else {
        &[-60, -40, -20, -9, 0]
    };
    for tick in ticks {
        let x = rect.left() + bar_width * meter_position(*tick as f32);
        let align = if *tick == -60 {
            egui::Align2::LEFT_TOP
        } else if *tick == 0 {
            egui::Align2::RIGHT_TOP
        } else {
            egui::Align2::CENTER_TOP
        };
        ui.painter().text(
            egui::pos2(x, rect.top() + count as f32 * 6.0),
            align,
            if *tick == 0 {
                if clipped {
                    format!("{} · 0 dB", crate::i18n::tr(ui, "Clipping"))
                } else {
                    "0 dB".into()
                }
            } else {
                tick.to_string()
            },
            egui::FontId::proportional(10.0),
            if clipped && *tick == 0 {
                crate::theme::interface_color(ui,skylive_core::InterfaceColor::MeterRedActive)
            } else {
                ui.visuals().weak_text_color()
            },
        );
    }
    if clipped {
        ui.painter().rect_stroke(
            rect.shrink(0.5),
            0.0,
            egui::Stroke::new(1.0, crate::theme::interface_color(ui,skylive_core::InterfaceColor::MeterRedActive)),
            egui::StrokeKind::Inside,
        );
    }
    let widget_info = {
        egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            true,
            format!(
                "{} · {} · {} dB · {}",
                crate::i18n::tr(ui, "Audio Mixer"),
                source.get(),
                if available {
                    format!("{:.1}", state.display[0])
                } else {
                    "−∞".into()
                },
                if clipped {
                    crate::i18n::tr(ui, "Clipping")
                } else {
                    String::new()
                }
            ),
        )
    };
    response.widget_info(|| widget_info.clone());
    response.on_hover_ui(|ui| {
        for channel in 0..state.channels {
            ui.label(crate::i18n::tf(
                ui,
                "Ch {} · peak {:.1} dB · RMS {:.1} dB",
                &[
                    ("{}", (channel + 1).to_string()),
                    ("{:.1}", format!("{:.1}", state.display[channel])),
                    ("{:.1}", format!("{:.1}", state.rms[channel])),
                ],
            ));
        }
        if clipped {
            ui.colored_label(crate::theme::interface_color(ui,skylive_core::InterfaceColor::MeterRedActive), crate::i18n::tr(ui, "Clipping"));
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn source_options(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: &Source,
    session: u64,
    capture: &AudioCaptureSnapshot,
    levels: Option<&Levels<'_>>,
    editable: bool,
    intents: &mut Vec<Intent>,
) {
    controls::source_menu(ui, model, source, session, editable, intents);
    if let Some(properties) = super::docks::source_properties_intent(source) {
        if ui
            .add_enabled(
                editable,
                egui::Button::new(crate::i18n::tr(ui, "Source Properties")),
            )
            .on_hover_text(crate::hints::text(ui, "Source Properties"))
            .on_disabled_hover_text(crate::hints::text(ui, "Source Properties"))
            .clicked()
        {
            intents.push(properties);
            ui.close_menu();
        }
    }
    if ui
        .add_enabled(
            editable
                && (model
                    .preferences()
                    .plugin_enabled(skylive_core::AUDIO_PAN_PLUGIN)
                    || model
                        .preferences()
                        .plugin_enabled(skylive_core::CRAFT_AUDIO_PLUGIN)
                    || model
                        .preferences()
                        .plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)
                    || !source.audio_filters().is_empty()),
            egui::Button::new(crate::i18n::tr(ui, "Audio Filters")),
        )
        .on_hover_text(crate::hints::text(ui, "Audio Filters"))
        .on_disabled_hover_text(crate::i18n::tr(
            ui,
            "Enable an audio filter provider in Tools > Plugins.",
        ))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::audio_filters(source)));
        ui.close_menu();
    }
    ui.separator();
    source_details(ui, source, capture, levels, editable);
}
fn source_details(
    ui: &mut egui::Ui,
    source: &Source,
    capture: &AudioCaptureSnapshot,
    levels: Option<&Levels<'_>>,
    editable: bool,
) {
    ui.set_max_width(380.0);
    ui.strong(source.name());
    ui.label(crate::i18n::tr(
        ui,
        if levels.is_some_and(|levels| levels.post_filter) {
            "Post-filter Program source"
        } else if levels.is_some() {
            "Source capture before filters"
        } else {
            "Waiting for audio levels"
        },
    ));
    ui.label(crate::i18n::tr(ui, source.audio_monitoring().label()));
    if !editable {
        ui.label(crate::i18n::tr(
            ui,
            "Retired Program source · restore its authored reference to edit",
        ));
    }
    if let Some(packet) = capture.packet(source.id()).filter(|packet| {
        source.kind().audio_capture_binding()==Some(&packet.binding)
    }) {
        if let Some(name) = &packet.resolved_device {
            ui.label(crate::i18n::tf(
                ui,
                "Device: {name}",
                &[("{name}", name.to_string())],
            ));
        }
        ui.label(crate::i18n::tf(
            ui,
            "Captured packet {} · {} Hz · {} channels",
            &[
                ("{}", packet.sequence.to_string()),
                ("{}", packet.block.format.sample_rate.to_string()),
                ("{}", packet.block.format.channels.to_string()),
            ],
        ));
    }
    for problem in capture
        .problems()
        .iter()
        .filter(|problem| problem.source == source.id())
    {
        ui.colored_label(
            ui.visuals().error_fg_color,
            crate::i18n::tf(
                ui,
                "{}: {}",
                &[
                    ("{}", problem.code.to_owned()),
                    ("{}", problem.detail.clone()),
                ],
            ),
        );
    }
}

fn audio_slot_label(slot: AudioSlot) -> &'static str {
    match slot {
        AudioSlot::Desktop1 => "Desktop Audio 1",
        AudioSlot::Desktop2 => "Desktop Audio 2",
        AudioSlot::Mic1 => "Mic/Aux 1",
        AudioSlot::Mic2 => "Mic/Aux 2",
        AudioSlot::Mic3 => "Mic/Aux 3",
        AudioSlot::Mic4 => "Mic/Aux 4",
    }
}
fn add_global(model: &StudioSnapshot, slot: AudioSlot, intents: &mut Vec<Intent>) {
    let label = audio_slot_label(slot);
    let names: HashSet<_> = model.sources().iter().map(|source| source.name()).collect();
    let name = if !names.contains(label) {
        label.to_owned()
    } else {
        (2..=4097)
            .map(|index| format!("{label} {index}"))
            .find(|name| !names.contains(name.as_str()))
            .unwrap_or_else(|| format!("{label} copy"))
    };
    intents.push(Intent::OpenDialog(Dialog::audio(
        AudioEdit::AddGlobal { slot },
        name,
        slot.flow(),
    )));
}
fn global_slots(ui: &mut egui::Ui, model: &StudioSnapshot, intents: &mut Vec<Intent>) {
    ui.menu_button(crate::i18n::tr(ui, "Global Audio Slots"), |ui| {
        for slot in [AudioSlot::Desktop1, AudioSlot::Desktop2, AudioSlot::Mic1, AudioSlot::Mic2, AudioSlot::Mic3, AudioSlot::Mic4] {
            ui.push_id(slot, |ui| {
                ui.menu_button(crate::i18n::tr(ui, audio_slot_label(slot)), |ui| {
                    let current = model.global_audio().iter().find(|entry| entry.slot() == slot).map(|entry| entry.source());
                    if ui.selectable_label(current.is_none(), crate::i18n::tr(ui, "Disabled")).on_hover_text(crate::hints::text(ui,"Disabled")).clicked() {
                        intents.push(Intent::Command(Command::SetGlobalAudio { slot, source: None })); ui.close_menu();
                    }
                    for source in model.sources().iter().filter(|source| matches!(source.kind(), SourceKind::AudioCapture { binding } if binding.flow() == slot.flow())) {
                        if ui.selectable_label(current == Some(source.id()), source.name()).on_hover_text(crate::hints::text(ui,"Source")).clicked() {
                            intents.push(Intent::Command(Command::SetGlobalAudio { slot, source: Some(source.id()) })); ui.close_menu();
                        }
                    }
                    if ui.button(crate::i18n::tr(ui, "Add New Audio Source")).on_hover_text(crate::hints::text(ui, "Add New Audio Source")).clicked() {
                        add_global(model, slot, intents); ui.close_menu();
                    }
                    if let Some(source) = current.and_then(|id| model.source(id)) {
                        if let SourceKind::AudioCapture { binding } = source.kind() {
                            if ui.button(crate::i18n::tr(ui, "Source Properties")).on_hover_text(crate::hints::text(ui,"Source Properties")).clicked() {
                                intents.push(Intent::OpenDialog(Dialog::audio_properties(source.id(), source.name().to_owned(), binding))); ui.close_menu();
                            }
                        }
                    }
                }).response.on_hover_text(crate::hints::text(ui, "Menu"));
            });
        }
    }).response.on_hover_text(crate::hints::text(ui, "Global Audio Slots"));
}

fn program_audio_panel(
    ui: &mut egui::Ui,
    program: Option<&ProgramAudioView>,
    stats: Option<&ProgramTransportStats>,
    settings: skylive_core::AudioSettings,
    revision: u64,
    peak_mode: skylive_core::MixerPeakMode,
) {
    ui.set_max_width(480.0);
    egui::ScrollArea::vertical()
        .id_salt("program-audio-status")
        .max_height(320.0)
        .show(ui, |ui| {
            ui.strong(crate::i18n::tr(ui, "Program Audio"));
            let program = program.filter(|program| program.key.settings == settings);
            ui.label(crate::i18n::tf(
                ui,
                "Project format: {} Hz · {}",
                &[
                    ("{}", (settings.sample_rate()).to_string()),
                    ("{}", crate::i18n::tr(ui, settings.speakers().label())),
                ],
            ));
            if let Some(stats) = stats {
                ui.label(if stats.running {
                    "Audio processing active"
                } else {
                    "Audio processing stopped"
                });
                ui.label(crate::i18n::tf(
                    ui,
                    "{} buffer: {:.1} / {:.1} ms",
                    &[
                        (
                            "{}",
                            crate::i18n::tr(
                                ui,
                                if settings.buffering().fixed() {
                                    "Fixed"
                                } else {
                                    "Adaptive"
                                },
                            ),
                        ),
                        (
                            "{:.1}",
                            format!(
                                "{:.1}",
                                f64::from(stats.buffer_blocks) * 1024.0 * 1000.0
                                    / f64::from(settings.sample_rate())
                            ),
                        ),
                        (
                            "{:.1}",
                            format!(
                                "{:.1}",
                                f64::from(stats.max_buffer_blocks) * 1024.0 * 1000.0
                                    / f64::from(settings.sample_rate())
                            ),
                        ),
                    ],
                ));
                ui.label(crate::i18n::tf(
                    ui,
                    "Buffering waits remaining: {} · pending sources: {}",
                    &[
                        ("{}", (stats.waiting_blocks).to_string()),
                        ("{}", (stats.pending_inputs).to_string()),
                    ],
                ));
                ui.label(crate::i18n::tf(
                    ui,
                    "Buffering pauses: {} · buffer increases: {}",
                    &[
                        ("{}", (stats.suppressed_blocks).to_string()),
                        ("{}", (stats.buffering_growths).to_string()),
                    ],
                ));
                ui.label(crate::i18n::tf(
                    ui,
                    "Input errors: {} · undelivered audio frames: {}",
                    &[
                        ("{}", (stats.input_errors).to_string()),
                        ("{}", (stats.dropped_frames).to_string()),
                    ],
                ));
                ui.label(crate::i18n::tf(
                    ui,
                    "Late audio frames: {} · skipped after a pause: {}",
                    &[
                        ("{}", (stats.late_frames).to_string()),
                        ("{}", (stats.skipped_frames).to_string()),
                    ],
                ));
                if peak_mode == skylive_core::MixerPeakMode::TruePeakEstimate {
                    let outputs = [
                        ("SkyLive.Mixer.SubmittedRecording", "Recording queue — estimated True Peak", &stats.recording_submitted_true_peaks),
                        ("SkyLive.Mixer.SubmittedStreaming", "Streaming queue — estimated True Peak", &stats.streaming_submitted_true_peaks),
                    ];
                    let has_current = outputs.iter().any(|(_, _, peaks)| peaks.iter().any(|peak| peak.revision == revision));
                    if has_current {
                        ui.separator();
                        ui.label(crate::i18n::tr_key_or(ui, "SkyLive.Mixer.SubmittedPeakHint",
                            "PCM accepted by the output queue, before encoding and recording pause trimming. Delivery and encoder acceptance are not confirmed."));
                        for (key, fallback, peaks) in outputs {
                            let mut current = peaks.iter().filter(|peak| peak.revision == revision).peekable();
                            if current.peek().is_none() { continue; }
                            ui.strong(crate::i18n::tr_key_or(ui, key, fallback));
                            for peak in current {
                                let values = peak.estimated_peaks.iter().take(peak.channels.min(peak.estimated_peaks.len()))
                                    .enumerate().map(|(channel, value)| {
                                        let db = if !value.is_finite() || *value < 0.0 { "—".to_owned() }
                                            else if *value == 0.0 { "−∞".to_owned() }
                                            else { format!("{:.1}", 20.0 * value.log10()) };
                                        format!("{}: {} dBTP", channel + 1, db)
                                    }).collect::<Vec<_>>().join(" · ");
                                let text = crate::i18n::tr_key_or(ui, "SkyLive.Mixer.SubmittedPeakTrack",
                                    "Track {track} · frames {start} + {frames} · revision {revision}: {peaks}")
                                    .replace("{track}", &peak.track.to_string())
                                    .replace("{start}", &peak.start_frame.to_string())
                                    .replace("{frames}", &peak.frames.to_string())
                                    .replace("{revision}", &peak.revision.to_string())
                                    .replace("{peaks}", &values);
                                ui.label(text);
                            }
                        }
                    }
                }
                if let Some(problem) = &stats.problem {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        match problem.code {
                            "program_audio_conversion" => {
                                "A source audio format could not be converted. Check its settings."
                            }
                            "program_audio_backpressure" | "audio_capture_backpressure" => {
                                "Audio buffering could not keep up."
                            }
                            _ => "Audio processing reported an error. Check the source settings.",
                        },
                    );
                }
            } else {
                ui.label(crate::i18n::tr(ui, "Waiting for audio processing"));
            }
            if let Some(program) = program.filter(|program| program.source_meter_revision == Some(revision)) {
                for (channel, meter) in program.meters.iter().enumerate() {
                    ui.add(
                        egui::ProgressBar::new(meter.peak.clamp(0.0, 1.0) as f32).text(format!(
                            "Program ch {} peak {:.3} · RMS {:.3}",
                            channel + 1,
                            meter.peak,
                            meter.rms
                        )),
                    );
                }
                ui.label(crate::i18n::tf(
                    ui,
                    "Missing source audio frames: {} · clipped samples: {}",
                    &[
                        ("{}", (program.missing_source_frames).to_string()),
                        ("{}", (program.clipped_samples).to_string()),
                    ],
                ));
            } else {
                ui.label(crate::i18n::tr(ui, "Waiting for audio levels"));
            }

        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget, Session};
    use skylive_host::audio_capture::{AudioCapabilities, AudioCaptureWorker};
    use std::sync::Arc;

    fn empty_capture() -> AudioCaptureSnapshot {
        let mut worker = AudioCaptureWorker::new(
            Box::new(|| panic!("presentation must never initialize a native backend")),
            AudioCapabilities {
                backend: "presentation test",
                input: false,
                output: false,
                device_enumeration: false,
                exclude_own_audio: false,
                note: None,
            },
        )
        .unwrap();
        let snapshot = worker.snapshot();
        worker.join().unwrap();
        snapshot
    }

    fn audio_session() -> (Session, Id) {
        let mut session = Session::new();
        session
            .apply(Command::AddAudioCaptureSource {
                scene: session.preview(),
                name: "Very long source name that must fit a compact source strip".into(),
                binding: AudioCaptureBinding::new(
                    AudioFlow::Input,
                    AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        (session, source)
    }

    fn render(
        context: &egui::Context,
        session: &Session,
        capture: &AudioCaptureSnapshot,
        events: Vec<egui::Event>,
        time: f64,
        intents: &mut Vec<Intent>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 250.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    show(
                        ui,
                        &session.snapshot(),
                        session.preview(),
                        MixerViewState {
                            session: session.identity(),
                            audio: capture,
                            program_views: &[],
                            preview: Some(scene),
                            program_audio: None,
                            program_status: None,
                        },
                        intents,
                    );
                });
            },
        )
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn obs_log_fader_has_literal_attenuation_oracle_and_preserves_boost_domain() {
        assert_eq!(deflection_gain(0.0), 0.0);
        assert_eq!(deflection_gain(1.0), 1.0);
        assert!((deflection_gain(0.5) - 0.115_629_41).abs() < 0.000_000_1);
        assert!((gain_deflection(0.115_629_41) - 0.5).abs() < 0.000_001);
        assert_eq!(gain_deflection(20.0), 1.0);
        assert_eq!(db_gain(gain_db(20.0)), 20.0);
        assert!((db_gain(-6.0206) - 0.5).abs() < 0.000_001);
        assert_eq!(format_db(gain_db(0.0)), "−∞");
        assert_eq!(parse_db("−∞"), Some(SILENCE_DB));
        assert_eq!(parse_db("NaN"), None);
        assert_eq!(parse_db("inf"), None);
    }
    #[test]
    fn audio_media_appears_once_in_mixer_and_actual_mute_edits_shared_source_while_silent_video_stays_out()
     {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddMediaSource {
                scene,
                name: "AAC movie".into(),
                media: skylive_core::MediaSource::new("movie.mp4".into(), 64, 48, true).unwrap(),
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::AddMediaSource {
                scene,
                name: "Silent movie".into(),
                media: skylive_core::MediaSource::new("silent.mp4".into(), 64, 48, false).unwrap(),
            })
            .unwrap();
        let capture = empty_capture();
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let mut intents = vec![];
        render(&context, &session, &capture, vec![], 0.0, &mut intents);
        let output = render(&context, &session, &capture, vec![], 0.1, &mut intents);
        fn caption(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| caption(shape, label))
                }
                _ => None,
            }
        }
        let visible = |label| {
            output.shapes.iter().find_map(|shape| {
                caption(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
        };
        assert!(visible("AAC movie").is_some());
        assert!(visible("Silent movie").is_none());
        let pos = visible("Mute").expect("audio media has a visible real mute control");
        for pressed in [true, false] {
            render(
                &context,
                &session,
                &capture,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                0.2,
                &mut intents,
            );
        }
        let [
            Intent::Command(Command::SetAudio {
                source: target,
                gain,
                muted,
            }),
        ] = intents.as_slice()
        else {
            panic!("actual media mute must emit one shared SetAudio command")
        };
        assert_eq!(*target, source);
        assert_eq!(*gain, 1.0);
        assert!(*muted);
        session
            .apply(Command::SetAudio {
                source: *target,
                gain: *gain,
                muted: *muted,
            })
            .unwrap();
        assert!(session.snapshot().source(source).unwrap().muted());
        session.apply(Command::Undo).unwrap();
        assert!(!session.snapshot().source(source).unwrap().muted());
    }

    #[test]
    fn actual_meter_values_have_db_thresholds_freshness_and_hold_without_fake_idle_signal() {
        let meters = [
            ChannelMeter {
                peak: 0.5,
                rms: 0.25,
            },
            ChannelMeter {
                peak: 1.2,
                rms: 0.1,
            },
        ];
        let mut state = MeterState::default();
        let levels = Levels {
            meters: &meters,
            scale: 1.0,
            token: (1, 1, 0),
            post_filter: true,
        };
        assert!(update_meter(&mut state, Some(&levels), 1.0));
        assert!((state.display[0] + 6.020_6).abs() < 0.000_01);
        assert!((state.rms[0] + 12.041_2).abs() < 0.000_01);
        assert_eq!(state.display[1], 0.0);
        assert_eq!(state.channels, 2);
        assert!(state.clip_until > 1.0);
        assert_eq!(meter_position(WARNING_DB), 2.0 / 3.0);
        assert_eq!(meter_position(ERROR_DB), 0.85);
        let held = state.held[0];
        assert!(
            !update_meter(&mut state, Some(&levels), 1.6),
            "an unchanged packet expires"
        );
        assert_eq!(state.held[0], held, "held peak survives missing signal");
        assert!(!update_meter(&mut MeterState::default(), None, 1.6));
        let invalid = [ChannelMeter {
            peak: f64::INFINITY,
            rms: 1.0,
        }];
        assert!(!update_meter(
            &mut state,
            Some(&Levels {
                meters: &invalid,
                ..levels
            }),
            1.7
        ));
    }

    #[test]
    fn painting_an_arbitrary_finite_fader_gain_does_not_quantize_or_author_an_edit() {
        let context = egui::Context::default();
        let capture = empty_capture();
        let (mut session, source) = audio_session();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.5001234,
                muted: false,
            })
            .unwrap();
        let original = session.snapshot();
        let mut intents = vec![];
        for frame in 0..5 {
            render(
                &context,
                &session,
                &capture,
                vec![],
                f64::from(frame) * 0.01,
                &mut intents,
            );
        }
        assert!(intents.is_empty());
        assert!(Arc::ptr_eq(&original, &session.snapshot()));
        assert_eq!(session.snapshot().source(source).unwrap().gain(), 0.5001234);
    }

    #[test]
    fn painting_boost_is_read_only_and_keyboard_attenuation_commits_once_or_cancels() {
        let context = egui::Context::default();
        let capture = empty_capture();
        let (mut session, source) = audio_session();
        session
            .apply(Command::SetAudio {
                source,
                gain: 20.0,
                muted: false,
            })
            .unwrap();
        let original = session.snapshot();
        let mut intents = Vec::new();
        let _ = render(&context, &session, &capture, vec![], 0.0, &mut intents);
        let _ = render(&context, &session, &capture, vec![], 0.01, &mut intents);
        assert!(intents.is_empty());
        assert!(Arc::ptr_eq(&original, &session.snapshot()));
        assert_eq!(session.snapshot().source(source).unwrap().gain(), 20.0);
        let slider_id = context
            .data_mut(|data| {
                data.get_temp::<egui::Id>(egui::Id::new(("audio-fader-widget", source.get())))
            })
            .unwrap();
        context.memory_mut(|memory| memory.request_focus(slider_id));
        let _ = render(
            &context,
            &session,
            &capture,
            vec![key(egui::Key::ArrowLeft)],
            0.02,
            &mut intents,
        );
        assert!(intents.is_empty(), "a keyboard gesture is still a draft");
        assert!(Arc::ptr_eq(&original, &session.snapshot()));
        let _ = render(
            &context,
            &session,
            &capture,
            vec![key(egui::Key::Escape)],
            0.03,
            &mut intents,
        );
        assert!(intents.is_empty());
        assert_eq!(session.snapshot().source(source).unwrap().gain(), 20.0);
        context.memory_mut(|memory| memory.request_focus(slider_id));
        let _ = render(
            &context,
            &session,
            &capture,
            vec![key(egui::Key::ArrowLeft)],
            0.04,
            &mut intents,
        );
        let _ = render(
            &context,
            &session,
            &capture,
            vec![key(egui::Key::Enter)],
            0.05,
            &mut intents,
        );
        assert_eq!(intents.len(), 1);
        let Intent::Command(command) = intents.pop().unwrap() else {
            panic!("volume command required");
        };
        session.apply(command).unwrap();
        let value = session.snapshot().source(source).unwrap().gain();
        assert!(
            value > 0.0 && value < 1.0,
            "intentional OBS attenuation replaces the boost"
        );
        assert_eq!(session.snapshot().revision(), original.revision() + 1);
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().source(source).unwrap().gain(), 20.0);
    }

    #[test]
    fn program_source_meters_use_actual_post_filter_channels_and_reject_foreign_configuration() {
        let (session, source) = audio_session();
        let model = session.snapshot();
        let capture = empty_capture();
        let mut program = ProgramAudioView {
            source_meter_revision: Some(model.revision()),
            key: skylive_host::audio_capture::ProgramCaptureKey {
                session: session.identity(),
                clock_epoch: 2,
                generation: 7,
                settings: model.audio(),
            },
            start_frame: 2048,
            source_meters: vec![skylive_host::audio::SourceMeter {
                source,
                channels: vec![ChannelMeter {
                    peak: 0.75,
                    rms: 0.125,
                }],
            }],
            meters: vec![],
            missing_source_frames: 0,
            clipped_samples: 0,
        };
        let levels = source_levels(
            model.source(source).unwrap(),
            session.identity(),
            &model,
            &capture,
            Some(&program),
        )
        .unwrap();
        assert!(levels.post_filter);
        assert_eq!(
            levels.scale, 1.0,
            "Program meters already include authored gain/mute"
        );
        assert_eq!(
            levels.meters,
            &[ChannelMeter {
                peak: 0.75,
                rms: 0.125
            }]
        );
        assert_eq!(levels.token, (1, 7, 2048));
        for revision in [None, Some(model.revision() + 1)] {
            program.source_meter_revision = revision;
            assert!(
                source_levels(
                    model.source(source).unwrap(),
                    session.identity(),
                    &model,
                    &capture,
                    Some(&program)
                )
                .is_none()
            );
        }
        program.source_meter_revision = Some(model.revision());
        program.missing_source_frames = 1;
        // A gap on a different source must not hide this retained meter.
        assert!(
            source_levels(
                model.source(source).unwrap(),
                session.identity(),
                &model,
                &capture,
                Some(&program)
            )
            .is_some_and(|level| level.post_filter)
        );
        program.missing_source_frames = 0;
        program.key.session += 1;
        assert!(
            source_levels(
                model.source(source).unwrap(),
                session.identity(),
                &model,
                &capture,
                Some(&program)
            )
            .is_none()
        );
    }

    #[test]
    fn stale_canonical_audio_edit_invalidates_draft_and_no_source_paint_authors_no_binding() {
        let (mut session, source) = audio_session();
        let original = session.snapshot();
        let mut draft = VolumeDraft::new(session.identity(), original.source(source).unwrap());
        draft.value = 0.25;
        draft.dirty = true;
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.75,
                muted: true,
            })
            .unwrap();
        assert!(!draft.matches(
            session.identity(),
            session.snapshot().source(source).unwrap()
        ));
        draft = VolumeDraft::new(
            session.identity(),
            session.snapshot().source(source).unwrap(),
        );
        assert_eq!(draft.value, 0.75);
        assert!(!draft.dirty);
        assert!(!draft.matches(
            session.identity() + 1,
            session.snapshot().source(source).unwrap()
        ));

        let context = egui::Context::default();
        let capture = empty_capture();
        let empty = Session::new();
        let before = empty.snapshot();
        let mut intents = Vec::new();
        let _ = render(&context, &empty, &capture, vec![], 0.0, &mut intents);
        assert!(intents.is_empty());
        assert!(before.sources().is_empty());
        assert!(Arc::ptr_eq(&before, &empty.snapshot()));
        add_global(&before, AudioSlot::Desktop1, &mut intents);
        assert!(matches!(intents.as_slice(), [Intent::OpenDialog(_)]));
        assert!(
            empty.snapshot().global_audio().is_empty(),
            "only Apply authors a binding"
        );
    }
    #[test]
    fn obs_audio_provider_alone_keeps_actual_filter_editor_accessible() {
        fn position(shape: &egui::epaint::Shape) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Audio Filters" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(position),
                _ => None,
            }
        }
        for retained in [false, true] {
            let (mut session, source) = audio_session();
            if retained {
                session
                    .apply(Command::SetSourceAudioFilters {
                        source,
                        filters: vec![skylive_core::AudioFilter::ObsGain {
                            db: 3.0,
                            enabled: true,
                        }],
                    })
                    .unwrap();
            }
            for (plugin, enabled) in [
                (skylive_core::CRAFT_AUDIO_PLUGIN, false),
                (skylive_core::AUDIO_PAN_PLUGIN, false),
            ] {
                session
                    .apply(Command::SetPluginEnabled {
                        plugin: plugin.into(),
                        enabled,
                    })
                    .unwrap();
            }
            let before = session.snapshot();
            assert!(
                before
                    .preferences()
                    .plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)
            );
            assert!(
                session
                    .apply(Command::SetPluginEnabled {
                        plugin: skylive_core::OBS_VIDEO_FILTERS_PLUGIN.into(),
                        enabled: false,
                    })
                    .is_err(),
                "built-in OBS provider cannot be disabled"
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
            let context = egui::Context::default();
            let capture = empty_capture();
            let mut intents = vec![];
            let mut draw = |events| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(500.0, 500.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            source_options(
                                ui,
                                &before,
                                before.source(source).unwrap(),
                                &capture,
                                None,
                                true,
                                &mut intents,
                            );
                        });
                    },
                )
            };
            let _ = draw(vec![]);
            let output = draw(vec![]);
            let point = output
                .shapes
                .iter()
                .find_map(|shape| position(&shape.shape).filter(|p| shape.clip_rect.contains(*p)))
                .unwrap();
            for pressed in [true, false] {
                let _ = draw(vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            assert_eq!(intents.len(), 1);
            {
                assert!(matches!(
                    intents.as_slice(),
                    [Intent::OpenDialog(
                        super::super::dialogs::Dialog::AudioFilters { .. }
                    )]
                ));
            }
            assert!(
                Arc::ptr_eq(&before, &session.snapshot()),
                "opening an editor does not author device/filter state"
            );
        }
    }
}
