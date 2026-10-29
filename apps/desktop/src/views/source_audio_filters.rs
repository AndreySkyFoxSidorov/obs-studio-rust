//! Source filter drafts. Apply/Cancel and publication stay in the shared dialog.
use eframe::egui;
use skylive_core::{AudioFilter, CompressorDetector, Id, StudioSnapshot};

fn selection_id(source:Id,binding:Option<super::plugin_routes::PluginEditorToken>)->egui::Id {
    egui::Id::new(("audio-filter-selection",source.get(),binding.map(|token|(token.session,token.generation))))
}
pub(crate) fn select_appended(context:&egui::Context,source:Id,binding:Option<super::plugin_routes::PluginEditorToken>,index:usize){
    context.data_mut(|data|data.insert_temp(selection_id(source,binding),index));
}

pub fn editor(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: Id,
    binding: Option<super::plugin_routes::PluginEditorToken>,
    name: &str,
    filters: &mut Vec<AudioFilter>,
) {
    ui.label(crate::i18n::tf(
        ui,
        "Source: {name}",
        &[("{name}", name.to_owned())],
    ));
    ui.label(crate::i18n::tr(ui,"Filters execute from top to bottom, following OBS. This source's settings are shared by every scene reference. Apply commits the complete chain; Cancel discards this draft."));
    let selected_id = selection_id(source,binding);
    let mut selected = ui
        .ctx()
        .data_mut(|data| data.get_temp::<usize>(selected_id))
        .unwrap_or(0)
        .min(filters.len().saturating_sub(1));
    ui.push_id("audio-filter-add-actions", |ui| {
        let mut chosen=None;
        ui.add_enabled_ui(filters.len()<skylive_core::MAX_AUDIO_FILTERS,|ui|{
            ui.menu_button(crate::i18n::tr(ui,"Add Filter"),|ui|{
                let choices=skylive_core::AudioFilterFactory::ALL.map(|factory|super::filter_add_menu::Choice {
                    label:factory.label(),hint:factory.hint(),
                    enabled:model.preferences().plugin_enabled(factory.plugin_id()),disabled_hint:None,group:None,
                });
                chosen=super::filter_add_menu::show(ui,&choices);
            }).response.on_hover_text(crate::i18n::tr(ui,"Add an audio filter to the end of this source's chain."))
                .on_disabled_hover_text(crate::i18n::tr(ui,"A source supports at most eight audio filters. Remove a filter before adding another."));
        });
        if let Some(factory)=chosen.and_then(|index|skylive_core::AudioFilterFactory::ALL.get(index).copied()) {
            filters.push(factory.prototype());selected=filters.len()-1;
        }
    });
    if ui.available_width() >= 620.0 {
        let (area, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 400.0),
            egui::Sense::hover(),
        );
        let left_rect =
            egui::Rect::from_min_max(area.min, egui::pos2(area.left() + 224.0, area.bottom()));
        let right_rect =
            egui::Rect::from_min_max(egui::pos2(left_rect.right() + 16.0, area.top()), area.max);
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("audio-filter-list-pane")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
        left.set_width(left_rect.width());
        list(&mut left, model, source, binding, filters, &mut selected, 224.0);
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("audio-filter-properties-pane")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        right.set_clip_rect(right_rect.intersect(ui.clip_rect()));
        right.set_width(right_rect.width());
        properties(&mut right, model, source, filters, selected);
    } else {
        list(ui, model, source, binding, filters, &mut selected, 112.0);
        ui.separator();
        properties(ui, model, source, filters, selected);
    }
    ui.ctx()
        .data_mut(|data| data.insert_temp(selected_id, selected));
}
fn list(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: Id,
    binding: Option<super::plugin_routes::PluginEditorToken>,
    filters: &mut Vec<AudioFilter>,
    selected: &mut usize,
    height: f32,
) {
    let mut duplicate = None;
    let can_duplicate = filters.len() < skylive_core::MAX_AUDIO_FILTERS;
    egui::ScrollArea::vertical()
        .id_salt("source-audio-filter-chain")
        .max_height(height)
        .show(ui, |ui| {
            let scope=binding.and_then(|binding|super::filter_chain_reorder::Scope::audio(
                ui.id().with(source.get()),binding.session,model.revision(),binding.generation,filters));
            let mut rectangles=Vec::with_capacity(filters.len());
            for (index, filter) in filters.iter().enumerate() {
                let label = if matches!(filter,AudioFilter::CraftDither{..}) {
                    format!("{} {}",crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.Name","Craft Dither"),index+1)
                } else if matches!(filter,AudioFilter::CraftRectifier{..}) {
                    format!("{} {}",crate::i18n::tr_key_or(ui,"SkyLive.Craft.Rectifier.Name","Craft Rectifier"),index+1)
                } else if matches!(filter,AudioFilter::CraftDcRemoval{..}) {
                    format!("{} {}",crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dc.Name","Craft DC Removal"),index+1)
                } else if matches!(filter,AudioFilter::CraftLoFi{..}) {
                    format!("{} {}",crate::i18n::tr_key_or(ui,"SkyLive.Craft.LoFi.Name","Craft Lo-Fi"),index+1)
                } else if matches!(filter,AudioFilter::CraftSaturator{..}) {
                    format!("{} {}",crate::i18n::tr_key_or(ui,"SkyLive.Craft.Saturator.Name","Craft Saturator"),index+1)
                } else {crate::i18n::tf(
                    ui,
                    match filter {
                        AudioFilter::ObsNoiseSuppression {..}=>"OBS Noise Suppression (RNNoise) {}",
                        AudioFilter::ObsThreeBandEq {..}=>"OBS 3-Band Equalizer {}",
                        AudioFilter::ObsUpwardCompressor {..}=>"OBS Upward Compressor {}",
                        AudioFilter::ObsNoiseGate {..}=>"OBS Noise Gate {}",
                        AudioFilter::ObsLimiter { .. } => "OBS Limiter {}",
                        AudioFilter::ObsCompressor { .. } => "OBS Compressor {}",
                        AudioFilter::CraftModDelay{..}=>"Craft Mod Delay {}",
                        AudioFilter::CraftFdnReverb{..}=>"Craft FDN Reverb {}",
                        AudioFilter::CraftSaturator{..}=>"Craft Saturator {}",
                        AudioFilter::CraftDither{..}=>"Craft Dither {}",
                        AudioFilter::CraftRectifier{..}=>"Craft Rectifier {}",
                        AudioFilter::CraftDcRemoval{..}=>"Craft DC Removal {}",
                        AudioFilter::CraftLoFi{..}=>"Craft Lo-Fi {}",
                        AudioFilter::ObsExpander { .. } => "OBS Expander {}",
                        AudioFilter::ObsGain { .. } => "Gain {}",
                        AudioFilter::ObsPolarityInvert { .. } => "Invert Polarity {}",
                        AudioFilter::Pan { .. } => "Audio Pan Filter {}",
                        AudioFilter::Compressor { .. } => "Compressor {}",
                        AudioFilter::Gate { .. } => "Gate {}",
                        AudioFilter::SimpleEq { .. } => "Simple EQ {}",
                    },
                    &[("{}", (index + 1).to_string())],
                )};
                let response=ui
                    .push_id(index, |ui| {
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::Button::new(label)
                                .selected(index == *selected)
                                .sense(egui::Sense::click_and_drag())
                                .truncate(),
                        )
                        .on_hover_text(crate::i18n::tr(
                            ui,
                            "Select this filter to edit its properties. Drag to change its processing order, or use Move Filter Up and Move Filter Down. Apply saves the order; Cancel discards it.",
                        ))
                    })
                    .inner;
                if let Some(scope)=scope {
                    super::filter_chain_reorder::row(ui,&response,scope,index as u32+1,
                        model.preferences().plugin_enabled(filter.plugin_id()));
                }
                rectangles.push((index as u32+1,response.rect));
                if response.clicked() {
                    *selected = index;
                }
                response.context_menu(|ui| {
                    if ui.add_enabled(can_duplicate, egui::Button::new(crate::i18n::tr(ui, "Duplicate"))).clicked() {
                        duplicate = Some(index);
                        ui.close_menu();
                    }
                });
            }
            if filters.is_empty() {
                ui.label(crate::i18n::tr(ui, "No audio filters."));
            }
            if let Some(scope)=scope {
                if let Some((old,index))=super::filter_chain_reorder::finish(ui,scope,&rectangles,
                    |id|id.checked_sub(1).and_then(|index|filters.get(index as usize))
                        .is_some_and(|filter|model.preferences().plugin_enabled(filter.plugin_id()))) {
                    let filter=filters.remove(old);
                    filters.insert(index,filter);
                    *selected=index;
                }
            }
        });
    if let Some(index) = duplicate.filter(|index| *index < filters.len() && can_duplicate) {
        filters.insert(index + 1, filters[index]);
        *selected = index + 1;
    }
    let available = filters
        .get(*selected)
        .is_some_and(|filter| model.preferences().plugin_enabled(filter.plugin_id()));
    let actions = [
        super::modal_footer::Action::new(
            "Move Filter Up",
            available && *selected > 0,
            "Move Filter Up",
        ),
        super::modal_footer::Action::new(
            "Move Filter Down",
            available && *selected + 1 < filters.len(),
            "Move Filter Down",
        ),
        super::modal_footer::Action::new("Duplicate", !filters.is_empty() && filters.len() < skylive_core::MAX_AUDIO_FILTERS, "Duplicate"),
        super::modal_footer::Action::new("Remove Filter", !filters.is_empty(), "Remove Filter"),
    ];
    ui.push_id("audio-filter-stack-actions", |ui| {
        if let Some(action) = super::modal_footer::show(ui, &actions) {
            match action {
                0 => {
                    filters.swap(*selected, *selected - 1);
                    *selected -= 1;
                }
                1 => {
                    filters.swap(*selected, *selected + 1);
                    *selected += 1;
                }
                2 => {
                    filters.insert(*selected + 1, filters[*selected]);
                    *selected += 1;
                }
                3 => {
                    filters.remove(*selected);
                    *selected = (*selected).min(filters.len().saturating_sub(1));
                }
                _ => {}
            }
        }
    });
}
fn properties(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: Id,
    filters: &mut [AudioFilter],
    selected: usize,
) {
    let Some(filter) = filters.get_mut(selected) else {
        return;
    };
    let available = model.preferences().plugin_enabled(filter.plugin_id());
    if !available {
        ui.label(crate::i18n::tr(ui,"Module disabled · settings retained. Enable it in Tools > Plugins to edit these properties."));
    }
    if ui.add_enabled(available,egui::Button::new(crate::i18n::tr(ui,"Defaults")))
        .on_hover_text(crate::i18n::tr(ui,"Restore this filter's default settings in the draft. Apply saves them; Cancel keeps the original settings. The filter name and enabled state are preserved."))
        .on_disabled_hover_text(crate::i18n::tr(ui,"Enable this filter's module in Tools > Plugins before resetting it.")).clicked(){filter.reset_settings();}
    egui::ScrollArea::vertical().id_salt(("selected-audio-filter",selected)).max_height(390.0).show(ui,|ui|{
            ui.add_enabled_ui(available,|ui|match filter{
                AudioFilter::CraftDither{..}|AudioFilter::CraftRectifier{..}|AudioFilter::CraftDcRemoval{..}|AudioFilter::CraftLoFi{..}|AudioFilter::CraftSaturator{..}|AudioFilter::CraftModDelay{..}|AudioFilter::CraftFdnReverb{..}=>super::craft_time_controls::editor(ui,filter),
                AudioFilter::ObsCompressor{..}=>{
                    super::obs_dynamics_controls::editor(ui,filter);
                    if let AudioFilter::ObsCompressor{sidechain,..}=filter{
                        let label=ui.label(crate::i18n::tr(ui,"Sidechain / Ducking source"));
                        let selected=match *sidechain{
                            None=>crate::i18n::tr(ui,"None (own source)"),
                            Some(id)=>model.source(id).map(|source|source.name().to_owned()).unwrap_or_else(||crate::i18n::tf(ui,"Missing source #{id}",&[("{id}",id.get().to_string())])),
                        };
                        egui::ComboBox::from_id_salt(("compressor-sidechain",source.get(),selected.clone()))
                            .selected_text(selected).show_ui(ui,|ui|{
                                ui.selectable_value(sidechain,None,crate::i18n::tr(ui,"None (own source)"));
                                for key in model.sources().iter().filter(|key|key.id()!=source&&key.kind().supports_audio_settings()){
                                    ui.selectable_value(sidechain,Some(key.id()),key.name());
                                }
                            }).response.labelled_by(label.id)
                            .on_hover_text(crate::i18n::tr(ui,"Choose a microphone to lower music while you speak. Key volume does not affect ducking; key Mute does. A missing or inactive key is silence, never the music itself. Self references and cycles are rejected on Apply."))
                            .on_disabled_hover_text(crate::i18n::tr(ui,"Enable this filter's module before changing its sidechain."));
                    }
                },
                AudioFilter::ObsLimiter{..}|AudioFilter::ObsExpander{..}|AudioFilter::ObsNoiseGate{..}|AudioFilter::ObsUpwardCompressor{..}=>super::obs_dynamics_controls::editor(ui,filter),
                AudioFilter::ObsNoiseSuppression{..}|AudioFilter::ObsThreeBandEq{..}|AudioFilter::ObsGain{..}|AudioFilter::ObsPolarityInvert{..}=>super::obs_audio_controls::editor(ui,filter),
                AudioFilter::Gate{..}|AudioFilter::SimpleEq{..}=>super::craft_audio_controls::editor(ui,filter,model.audio().sample_rate()),
                AudioFilter::Pan{pan_left,pan_right,gain_left_db,gain_right_db,invert_left,invert_right}=>{
                    egui::Grid::new("pan-properties").show(ui,|ui|{
                        for(label,value,suffix)in[("Pan (Left), -100 to 100%",pan_left,"%"),("Pan (Right), -100 to 100%",pan_right,"%"),("Gain (Left), -80 to 30 dB",gain_left_db," dB"),("Gain (Right), -80 to 30 dB",gain_right_db," dB")]{let name=ui.label(crate::i18n::tr(ui,label));ui.add(egui::DragValue::new(value).speed(1.0).suffix(suffix)).labelled_by(name.id).on_hover_text(crate::i18n::tr(ui,label)).on_disabled_hover_text(crate::i18n::tr(ui,label));ui.end_row();}
                    });
                    ui.checkbox(invert_left,crate::i18n::tr(ui,"Invert (Left)")).on_hover_text(crate::hints::text(ui,"Invert (Left)"));ui.checkbox(invert_right,crate::i18n::tr(ui,"Invert (Right)")).on_hover_text(crate::hints::text(ui,"Invert (Right)"));
                }
                AudioFilter::Compressor{threshold_db,ratio,knee_db,attack_ms,release_ms,makeup_db,detector,enabled}=>{
                    ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass or enable this compressor without removing its settings."));
                    egui::Grid::new("compressor-properties").show(ui,|ui|{
                        for(label,value,min,max,suffix,speed)in[("Threshold",threshold_db,-60.0,0.0," dB",0.1),("Ratio",ratio,1.0,30.0,":1",0.1),("Knee",knee_db,0.0,24.0," dB",0.1),("Attack",attack_ms,0.01,300.0," ms",0.1),("Release",release_ms,1.0,3000.0," ms",1.0),("Make-up Gain",makeup_db,-40.0,40.0," dB",0.1)]{
                            let label_response=ui.label(crate::i18n::tr(ui,label));
                            ui.add(egui::DragValue::new(value).range(min..=max).clamp_existing_to_range(false).speed(speed).suffix(suffix)).labelled_by(label_response.id)
                                .on_hover_text(crate::i18n::tr(ui,match label{"Threshold"=>"Set the level above which compression reduces the signal.","Ratio"=>"Set how strongly levels above the threshold are reduced.","Knee"=>"Set the width of the gradual transition into compression.","Attack"=>"Set how quickly gain reduction follows a louder signal.","Release"=>"Set how quickly gain reduction recovers after the signal becomes quieter.",_=>"Adjust the compressor output gain. Changes ramp smoothly over 20 ms."}));ui.end_row();
                        }
                    });
                    ui.horizontal_wrapped(|ui|{
                        ui.label(crate::i18n::tr(ui,"Detector"));
                        ui.selectable_value(detector,CompressorDetector::Peak,crate::i18n::tr(ui,"Peak")).on_hover_text(crate::i18n::tr(ui,"Detect the loudest channel's sample peak; this is not true-peak limiting."));
                        ui.selectable_value(detector,CompressorDetector::Rms,"RMS").on_hover_text(crate::i18n::tr(ui,"Detect the loudest channel's smoothed mean-square level."));
                    });
                }
            });
            if matches!(filter,AudioFilter::Compressor{..}){curve(ui,*filter);}
    });
}

fn curve(ui: &mut egui::Ui, filter: AudioFilter) {
    let Ok(parameters) = skylive_host::audio::compressor_parameters(filter) else {
        ui.colored_label(
            ui.visuals().error_fg_color,
            crate::i18n::tr(
                ui,
                "Correct the compressor parameters to display its curve.",
            ),
        );
        return;
    };
    ui.label(crate::i18n::tr(
        ui,
        "Static response · input −60…0 dB · output −100…40 dB",
    ));
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(440.0), 112.0),
        egui::Sense::hover(),
    );
    response.on_hover_text(crate::i18n::tr(ui,"The curve uses the compressor's actual target settings. Attack and release affect how quickly the sound follows this curve."));
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, 2.0, crate::theme::preview_background(ui));
    for step in 1..6 {
        let x = rect.left() + rect.width() * step as f32 / 6.0;
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.25)),
        );
    }
    let points: Vec<_> = (0..=64)
        .filter_map(|index| {
            let input = -60.0 + 60.0 * index as f32 / 64.0;
            parameters.transfer_db(input).ok().map(|output| {
                egui::pos2(
                    rect.left() + rect.width() * index as f32 / 64.0,
                    rect.bottom() - rect.height() * (output + 100.0) / 140.0,
                )
            })
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(
            2.0,
            if ui.visuals().dark_mode {
                egui::Color32::from_rgb(75, 175, 255)
            } else {
                ui.visuals().hyperlink_color
            },
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{
        Intent,
        dialogs::{self, Dialog},
    };
    mod obs_dynamics_dialog_tests {
        include!("obs_dynamics_dialog_tests.rs");
    }
    #[test]
    fn builtin_gain_and_polarity_actions_stage_validate_cancel_and_apply_one_undo() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget, Command};
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Input".into(),
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
        let baseline = session.snapshot();
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut slot = Some(Dialog::audio_filters(baseline.source(source).unwrap()));
        assert!(click(&context, &session, &mut slot, "Add Gain").is_empty());
        assert!(click(&context, &session, &mut slot, "Add Invert Polarity").is_empty());
        assert!(click(&context, &session, &mut slot, "Enabled").is_empty());
        assert!(
            matches!(&slot,Some(Dialog::AudioFilters{filters,..}) if filters==&vec![AudioFilter::obs_gain(),AudioFilter::ObsPolarityInvert{enabled:false}])
        );
        let _ = click(&context, &session, &mut slot, "Cancel");
        assert!(slot.is_none());
        assert_eq!(session.snapshot().revision(), baseline.revision());
        let mut slot = Some(Dialog::audio_filters(baseline.source(source).unwrap()));
        click(&context, &session, &mut slot, "Add Gain");
        click(&context, &session, &mut slot, "Add Invert Polarity");
        if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
            filters[0] = AudioFilter::ObsGain {
                db: 31.0,
                enabled: true,
            };
        }
        assert!(
            matches!(click(&context,&session,&mut slot,"Apply").as_slice(),[Intent::DialogError(message)] if message.contains("OBS Gain requires finite -30..=30 dB"))
        );
        assert!(
            matches!(&slot,Some(Dialog::AudioFilters{filters,..}) if matches!(filters[0],AudioFilter::ObsGain{db,..} if db==31.0))
        );
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        assert!(slot.is_some());
        assert_eq!(session.snapshot().revision(), baseline.revision());
        if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
            filters[0] = AudioFilter::ObsGain {
                db: 20.0,
                enabled: true,
            };
        }
        let command = click(&context, &session, &mut slot, "Apply")
            .into_iter()
            .find_map(|intent| {
                if let Intent::ApplyPluginEditor { command, .. } = intent {
                    Some(command)
                } else {
                    None
                }
            })
            .unwrap();
        session.apply(command).unwrap();
        assert_eq!(session.snapshot().revision(), baseline.revision() + 1);
        assert_eq!(
            session.snapshot().source(source).unwrap().audio_filters(),
            [
                AudioFilter::ObsGain {
                    db: 20.0,
                    enabled: true
                },
                AudioFilter::obs_polarity_invert()
            ]
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .is_empty()
        );
    }
    fn frame(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<Dialog>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<Intent>) {
        crate::i18n::install(
            context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut intents = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
        );
        (output, intents)
    }
    fn position(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| position(shape, label))
            }
            _ => None,
        }
    }
    fn click(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<Dialog>,
        label: &str,
    ) -> Vec<Intent> {
        frame(context, session, slot, vec![]);
        let (output, _) = frame(context, session, slot, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                position(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .expect("source filter action is visible");
        frame(
            context,
            session,
            slot,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            context,
            session,
            slot,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .1
    }
    #[test]
    fn actual_compressor_editor_stages_cancel_rejects_invalid_and_commits_one_undo() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget, Command};
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Voice".into(),
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
        let context = egui::Context::default();
        crate::theme::install(&context);
        let baseline = session.snapshot();
        let mut slot = Some(Dialog::audio_filters(baseline.source(source).unwrap()));
        assert!(click(&context, &session, &mut slot, "Add Compressor").is_empty());
        assert!(
            matches!(&slot,Some(Dialog::AudioFilters{filters,..}) if filters==&vec![AudioFilter::compressor()])
        );
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        let (output, _) = frame(&context, &session, &mut slot, vec![]);
        assert!(output.shapes.iter().any(|shape| {
            position(
                &shape.shape,
                "Static response · input −60…0 dB · output −100…40 dB",
            )
            .is_some()
        }));
        click(&context, &session, &mut slot, "Cancel");
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        slot = Some(Dialog::audio_filters(baseline.source(source).unwrap()));
        click(&context, &session, &mut slot, "Add Compressor");
        if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
            if let AudioFilter::Compressor { ratio, .. } = &mut filters[0] {
                *ratio = 0.0;
            }
        }
        assert!(
            click(&context, &session, &mut slot, "Apply")
                .iter()
                .any(|intent| matches!(intent, Intent::DialogError(_)))
        );
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
            if let AudioFilter::Compressor {
                ratio,
                threshold_db,
                ..
            } = &mut filters[0]
            {
                *ratio = 4.0;
                *threshold_db = -30.0;
            }
        }
        let command = click(&context, &session, &mut slot, "Apply")
            .into_iter()
            .find_map(|intent| {
                if let Intent::ApplyPluginEditor { command, .. } = intent {
                    Some(command)
                } else {
                    None
                }
            })
            .unwrap();
        session.apply(command).unwrap();
        assert_eq!(session.snapshot().revision(), baseline.revision() + 1);
        assert!(
            matches!(session.snapshot().source(source).unwrap().audio_filters(),[AudioFilter::Compressor{threshold_db,..}] if *threshold_db== -30.0)
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .is_empty()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .len(),
            1
        );
    }
    #[test]
    fn actual_gate_and_eq_editors_stage_cancel_validate_and_commit_one_chain_undo() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget, Command};
        let mut session = skylive_core::Session::new();
        session
            .apply(Command::AddAudioCaptureSource {
                scene: session.preview(),
                name: "Voice".into(),
                binding: AudioCaptureBinding::new(
                    AudioFlow::Input,
                    AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap();
        let model = session.snapshot();
        let source = model.sources()[0].id();
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut slot = Some(Dialog::audio_filters(model.source(source).unwrap()));
        assert!(click(&context, &session, &mut slot, "Add Gate").is_empty());
        click(&context, &session, &mut slot, "Enabled");
        assert!(
            matches!(&slot,Some(Dialog::AudioFilters{filters,..})if matches!(filters.as_slice(),[AudioFilter::Gate{enabled:false,..}]))
        );
        click(&context, &session, &mut slot, "Cancel");
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&model, &session.snapshot()));
        slot = Some(Dialog::audio_filters(model.source(source).unwrap()));
        click(&context, &session, &mut slot, "Add Gate");
        click(&context, &session, &mut slot, "Add Simple EQ");
        // Edit an actual numeric widget rather than assigning the desired
        // command. All values still belong to the selected presentation row.
        frame(&context, &session, &mut slot, vec![]);
        let pos = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("craft-audio-field", "Mid Gain")))
            })
            .unwrap()
            .center();
        for pressed in [true, false] {
            frame(
                &context,
                &session,
                &mut slot,
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
        // DragValue enters text mode on a keyboard focus event; Ctrl+A and text
        // replace its displayed value without publishing a source edit.
        frame(
            &context,
            &session,
            &mut slot,
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
                egui::Event::Text("6".into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(
            matches!(&slot,Some(Dialog::AudioFilters{filters,..})if matches!(filters[1],AudioFilter::SimpleEq{mid_db,..}if (mid_db-6.0).abs()<0.001))
        );
        assert!(std::sync::Arc::ptr_eq(&model, &session.snapshot()));
        if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
            if let AudioFilter::SimpleEq { mid_q, .. } = &mut filters[1] {
                *mid_q = 0.0;
            }
        }
        assert!(matches!(
            click(&context, &session, &mut slot, "Apply").as_slice(),
            [Intent::DialogError(_)]
        ));
        assert!(std::sync::Arc::ptr_eq(&model, &session.snapshot()));
        assert!(
            matches!(&slot,Some(Dialog::AudioFilters{filters,..})if matches!(filters[1],AudioFilter::SimpleEq{mid_q,..} if mid_q==0.0))
        );
        if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
            if let AudioFilter::SimpleEq { mid_q, .. } = &mut filters[1] {
                *mid_q = 0.7;
            }
        }
        let command = click(&context, &session, &mut slot, "Apply")
            .into_iter()
            .find_map(|intent| {
                if let Intent::ApplyPluginEditor { command, .. } = intent {
                    Some(command)
                } else {
                    None
                }
            })
            .unwrap();
        session.apply(command).unwrap();
        assert_eq!(session.snapshot().revision(), model.revision() + 1);
        assert!(
            matches!(session.snapshot().source(source).unwrap().audio_filters(),[AudioFilter::Gate{..},AudioFilter::SimpleEq{mid_db,..}]if (*mid_db-6.0).abs()<0.001)
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .is_empty()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .len(),
            2
        );
    }
    #[test]
    fn eight_equalizers_keep_one_selected_panel_and_painting_preserves_invalid_draft() {
        let session = skylive_core::Session::new();
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let mut filters = vec![AudioFilter::simple_eq(); 8];
        if let AudioFilter::SimpleEq { mid_q, .. } = &mut filters[0] {
            *mid_q = 0.0;
        }
        let before = filters.clone();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 800.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor(
                        ui,
                        &session.snapshot(),
                        skylive_core::Id::new(1),
                        None,
                        "Voice",
                        &mut filters,
                    )
                });
            },
        );
        assert_eq!(
            output
                .shapes
                .iter()
                .filter(|shape| position(&shape.shape, "Low Gain").is_some())
                .count(),
            1
        );
        assert_eq!(
            filters, before,
            "painting cannot repair a rejected draft or expand eight property forms"
        );
    }
    #[test]
    fn eight_compressors_keep_one_selected_property_panel() {
        let session = skylive_core::Session::new();
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut filters = vec![AudioFilter::compressor(); 8];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 800.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor(
                        ui,
                        &session.snapshot(),
                        skylive_core::Id::new(1),
                        None,
                        "Voice",
                        &mut filters,
                    )
                });
            },
        );
        assert_eq!(
            output
                .shapes
                .iter()
                .filter(|shape| position(&shape.shape, "Threshold").is_some())
                .count(),
            1,
            "a long chain must not expand eight property forms"
        );
        assert_eq!(filters, vec![AudioFilter::compressor(); 8]);
    }
}
