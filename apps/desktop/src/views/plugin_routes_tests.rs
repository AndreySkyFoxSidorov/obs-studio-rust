use super::*;
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget, Command, Session};

fn fixture() -> (Session, Id, Id) {
    let mut owner = Session::new();
    let scene = owner.preview();
    owner
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Real audio target".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let audio = owner.snapshot().sources()[0].id();
    owner
        .apply(Command::AddColorSource {
            scene,
            name: "Real video target".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let video = owner.snapshot().sources()[1].id();
    (owner, audio, video)
}
#[test]
fn compatible_targets_are_existing_audio_video_or_scene_owners() {
    let (owner, audio, video) = fixture();
    let model = owner.snapshot();
    let scene = owner.preview();
    assert_eq!(
        choices(&model, PluginOperation::AudioFilters),
        vec![PluginTarget::Source(audio)]
    );
    assert_eq!(
        choices(&model, PluginOperation::VideoFilters),
        vec![PluginTarget::Source(video), PluginTarget::Scene(scene)]
    );
    assert_eq!(
        choices(&model, PluginOperation::SceneNotes),
        vec![PluginTarget::Scene(scene)]
    );
    assert!(!compatible(
        &model,
        PluginOperation::AudioFilters,
        PluginTarget::Source(video)
    ));
    assert!(!compatible(
        &model,
        PluginOperation::VideoFilters,
        PluginTarget::Source(audio)
    ));
    assert!(!compatible(
        &model,
        PluginOperation::SceneNotes,
        PluginTarget::Source(video)
    ));
    assert!(!compatible(
        &model,
        PluginOperation::AudioFilters,
        PluginTarget::Scene(scene)
    ));
    assert!(!compatible(
        &model,
        PluginOperation::VideoFilters,
        PluginTarget::Source(Id::new(u64::MAX))
    ));
}
fn painted(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
        }
        egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| painted(shape, label)),
        _ => None,
    }
}
fn frame(
    context: &egui::Context,
    model: &StudioSnapshot,
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
                egui::vec2(380.0, 700.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    model,
                    skylive_core::AUDIO_PAN_PLUGIN,
                    &skylive_core::plugin_entry_points(skylive_core::AUDIO_PAN_PLUGIN)[0],
                    (71, 29),
                    &mut intents,
                )
            });
        },
    );
    (output, intents)
}
#[test]
fn real_button_emits_full_identity_and_disabled_provider_emits_nothing() {
    let (mut owner, audio, _) = fixture();
    let context = egui::Context::default();
    frame(&context, &owner.snapshot(), vec![]);
    let (output, _) = frame(&context, &owner.snapshot(), vec![]);
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| painted(&shape.shape, "Open Audio Filters").is_some())
    );
    for active in [false, true] {
        owner
            .apply(Command::SetPluginEnabled {
                plugin: skylive_core::AUDIO_PAN_PLUGIN.into(),
                enabled: active,
            })
            .unwrap();
        let model = owner.snapshot();
        frame(&context, &model, vec![]);
        let (output, _) = frame(&context, &model, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                painted(&shape.shape, "Open Audio Filters")
                    .filter(|rect| shape.clip_rect.contains_rect(*rect))
            })
            .unwrap()
            .center();
        frame(
            &context,
            &model,
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
        let (_, intents) = frame(
            &context,
            &model,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        if active {
            assert!(
                matches!(&intents[..],[Intent::OpenPluginEditor(request)] if request.session==71 && request.manager==29 && request.revision==model.revision() && request.plugin==skylive_core::AUDIO_PAN_PLUGIN && request.operation==PluginOperation::AudioFilters && request.target==PluginTarget::Source(audio))
            );
        } else {
            assert!(intents.is_empty());
        }
    }
}
