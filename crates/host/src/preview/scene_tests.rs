//! Literal nested-scene pixels and PRIVATE_REFS lifetime, without native devices.
use super::*;
use crate::{playback::PlaybackClock, transition::Controller};
use skylive_core::{Command, Transform};
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
fn setup() -> (Session, Id, Id) {
    let mut session = Session::new();
    let outer = session.preview();
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let child = session
        .apply(Command::AddScene {
            name: "Shared child".into(),
        })
        .unwrap()
        .created
        .unwrap();
    (session, outer, child)
}
fn color(session: &mut Session, scene: Id, name: &str, rgba: [u8; 4]) -> Id {
    session
        .apply(Command::AddColorSource {
            scene,
            name: name.into(),
            rgba,
        })
        .unwrap()
        .created
        .unwrap()
}
fn render_scene_now(session: &Session, scene: Id) -> RgbaFrame {
    render_at(&session.snapshot(), scene, 8, 4, MediaTime(0)).unwrap()
}
fn pixel(image: &RgbaFrame, x: usize, y: usize) -> [u8; 4] {
    image.pixels()[(y * image.width() as usize + x) * 4..][..4]
        .try_into()
        .unwrap()
}
#[test]
fn nested_native_canvas_pattern_crop_flip_alpha_and_native_roundtrip() {
    let (mut session, outer, child) = setup();
    color(&mut session, outer, "Outer blue", BLUE);
    let red = color(&mut session, child, "Left red", RED);
    let green = color(&mut session, child, "Right green", GREEN);
    let mut left = Transform::full(16, 16);
    left.width = 8.0;
    session
        .apply(Command::SetTransform {
            scene: child,
            item: red,
            transform: left.clone(),
        })
        .unwrap();
    let mut right = left;
    right.x = 8.0;
    session
        .apply(Command::SetTransform {
            scene: child,
            item: green,
            transform: right,
        })
        .unwrap();
    let nested = session
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap()
        .created
        .unwrap();
    let image = render_scene_now(&session, outer);
    for y in 0..4 {
        for x in 0..8 {
            assert_eq!(pixel(&image, x, y), if x < 4 { RED } else { GREEN });
        }
    }
    let mut transform = Transform::full(16, 16);
    transform.flip_x = true;
    transform.x = 16.0;
    session
        .apply(Command::SetTransform {
            scene: outer,
            item: nested,
            transform: transform.clone(),
        })
        .unwrap();
    let image = render_scene_now(&session, outer);
    assert_eq!(pixel(&image, 0, 0), GREEN);
    assert_eq!(pixel(&image, 7, 3), RED);
    transform.flip_x = false;
    transform.x = 0.0;
    transform.crop.left = 8;
    session
        .apply(Command::SetTransform {
            scene: outer,
            item: nested,
            transform,
        })
        .unwrap();
    let image = render_scene_now(&session, outer);
    for x in 0..8 {
        assert_eq!(pixel(&image, x, 0), if x < 4 { GREEN } else { BLUE });
    }
    let bytes = crate::storage::encode(&session.snapshot()).unwrap();
    let restored = crate::storage::decode(&bytes, true).unwrap();
    assert_eq!(
        restored.snapshot().collection(),
        session.snapshot().collection()
    );
    assert_eq!(render_scene_now(&restored, outer).pixels(), image.pixels());

    let (mut alpha, outer, child) = setup();
    color(&mut alpha, child, "Half white", [255, 255, 255, 128]);
    alpha
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap();
    assert_eq!(
        pixel(&render_scene_now(&alpha, outer), 4, 2),
        [188, 188, 188, 255],
        "transparent nested canvas must not premultiply encoded white or flatten early"
    );
}
#[test]
fn private_root_retains_reference_while_nested_layout_is_shared_and_removed_fallback_is_latest() {
    let (mut session, outer, child) = setup();
    color(&mut session, child, "Red leaf", RED);
    let nested = session
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap()
        .created
        .unwrap();
    let mut owner = Controller::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    let mut clock = PlaybackClock::new();
    let mut sample = |session: &mut Session, owner: &mut Controller| {
        let snapshot = owner.sync(session, MediaTime(0)).unwrap();
        let playback = clock
            .update_outputs(session, &snapshot.output.scenes(), MediaTime(0))
            .unwrap();
        render_output(&session.snapshot(), &snapshot.output, 8, 4, &playback).unwrap()
    };
    assert_eq!(pixel(&sample(&mut session, &mut owner), 0, 0), RED);
    owner.before_change(&session).unwrap();
    color(&mut session, child, "New top green", GREEN);
    assert_eq!(
        pixel(&sample(&mut session, &mut owner), 0, 0),
        GREEN,
        "nested scene layout stays shared under PRIVATE_REFS"
    );
    owner.before_change(&session).unwrap();
    session
        .apply(Command::RemoveItem {
            scene: outer,
            item: nested,
        })
        .unwrap();
    assert_eq!(
        pixel(&sample(&mut session, &mut owner), 0, 0),
        GREEN,
        "root item layout remains private"
    );
    owner.before_change(&session).unwrap();
    session
        .apply(Command::RemoveScene { scene: child })
        .unwrap();
    assert_eq!(
        pixel(&sample(&mut session, &mut owner), 0, 0),
        GREEN,
        "last observed removed nested layout and sources remain available"
    );
}
#[test]
fn nested_audio_routes_deduplicate_shared_leaf_and_hidden_parent_stops_activation() {
    let (mut session, outer, child) = setup();
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene: child,
            name: "Nested audio".into(),
            binding: skylive_core::AudioCaptureBinding::new(
                skylive_core::AudioFlow::Input,
                skylive_core::AudioTarget::Default,
                false,
                false,
            )
            .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let source = session
        .snapshot()
        .scene(child)
        .unwrap()
        .items()
        .iter()
        .find(|entry| entry.id() == item)
        .unwrap()
        .source();
    let first = session
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap()
        .created
        .unwrap();
    let second = session
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap()
        .created
        .unwrap();
    let output = FrameComposition::Scene(FrameScene::Live(outer));
    let plan = crate::audio::derive_audio_plan(&session, &output).unwrap();
    assert_eq!(plan.sources().len(), 1);
    assert_eq!(plan.sources()[0].source, source);
    session
        .apply(Command::SetVisible {
            scene: outer,
            item: first,
            visible: false,
        })
        .unwrap();
    assert_eq!(
        crate::audio::derive_audio_plan(&session, &output)
            .unwrap()
            .sources()
            .len(),
        1
    );
    session
        .apply(Command::SetVisible {
            scene: outer,
            item: second,
            visible: false,
        })
        .unwrap();
    assert!(
        crate::audio::derive_audio_plan(&session, &output)
            .unwrap()
            .sources()
            .is_empty()
    );
}
#[test]
fn nested_intermediate_admission_rejects_large_chain_before_any_large_raster() {
    let (mut session, outer, _) = setup();
    session
        .apply(Command::SetVideo {
            width: 4096,
            height: 2048,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let mut parent = outer;
    for n in 0..10 {
        let child = session
            .apply(Command::AddScene {
                name: format!("Large child {n}"),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::AddSceneSource {
                scene: parent,
                target: child,
            })
            .unwrap();
        parent = child;
    }
    let model = session.snapshot();
    let playback = PlaybackSnapshot::at_time(&session, MediaTime(0));
    let capture = CaptureSnapshot::empty(session.identity());
    let mut cache = ClipCache::default();
    let mut resources = RenderResources::new(&mut cache, &capture);
    assert!(matches!(
        render_frame(
            &model,
            &FrameScene::Live(outer),
            8,
            4,
            &playback,
            &mut resources
        ),
        Err(PreviewError::Transition(
            crate::transition::TransitionError::Limit(_)
        ))
    ));
    assert!(
        resources.nested.is_empty(),
        "admission stops descent before unwinding/allocating large outputs"
    );
}

fn root_opacity(value: f32) -> Vec<skylive_core::VideoFilter> {
    let mut effect = skylive_core::VideoEffect::color_correction();
    if let skylive_core::VideoEffect::ColorCorrection { opacity, .. } = &mut effect {
        *opacity = value;
    }
    vec![
        skylive_core::VideoFilter::new(effect)
            .unwrap()
            .with_name("Scene alpha".into())
            .unwrap(),
    ]
}
#[test]
fn root_and_nested_scene_filters_apply_once_with_native_identity_and_last_reference_retention() {
    let (mut session, outer, child) = setup();
    color(&mut session, child, "White", [255; 4]);
    session
        .apply(Command::SetSceneVideoFilters {
            scene: child,
            filters: root_opacity(0.5),
        })
        .unwrap();
    let root = render_scene_now(&session, child);
    assert_eq!(
        pixel(&root, 0, 0),
        [188, 188, 188, 255],
        "one half-opacity linear scene, black flattened after filter"
    );
    let nested = session
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap()
        .created
        .unwrap();
    assert_eq!(
        pixel(&render_scene_now(&session, outer), 0, 0),
        [188, 188, 188, 255],
        "nested must not receive its root filter twice"
    );
    let owner = session.snapshot().scene_source(child).unwrap().id();
    let filter = session
        .snapshot()
        .scene_source(child)
        .unwrap()
        .video_filters()[0]
        .clone();
    session
        .apply(Command::RemoveItem {
            scene: outer,
            item: nested,
        })
        .unwrap();
    assert_eq!(session.snapshot().scene_source(child).unwrap().id(), owner);
    assert_eq!(
        pixel(&render_scene_now(&session, child), 0, 0),
        [188, 188, 188, 255]
    );
    let bytes = crate::storage::encode(&session.snapshot()).unwrap();
    let restored = crate::storage::decode_with_report(&bytes, true).unwrap();
    assert!(!restored.migration.has_semantic_changes());
    assert!(!restored.session.is_dirty());
    assert_eq!(
        restored
            .session
            .snapshot()
            .scene_source(child)
            .unwrap()
            .video_filters()[0],
        filter
    );
    assert_eq!(
        render_scene_now(&restored.session, child).pixels(),
        root.pixels()
    );
    session.apply(Command::Undo).unwrap();
    session
        .apply(Command::SetSceneVideoFilters {
            scene: outer,
            filters: root_opacity(0.5),
        })
        .unwrap();
    assert_eq!(
        pixel(&render_scene_now(&session, outer), 0, 0),
        [137, 137, 137, 255],
        "two distinct Scene owners each apply half once"
    );
}
#[test]
fn private_root_copies_own_filters_while_nested_owner_remains_live_and_deleted_fallback_retains_latest()
 {
    let (mut session, outer, child) = setup();
    color(&mut session, child, "White", [255; 4]);
    session
        .apply(Command::AddSceneSource {
            scene: outer,
            target: child,
        })
        .unwrap();
    session
        .apply(Command::SetSceneVideoFilters {
            scene: outer,
            filters: root_opacity(0.5),
        })
        .unwrap();
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    let mut clock = PlaybackClock::new();
    let mut sample = |session: &mut Session, controller: &mut Controller| {
        let output = controller.sync(session, MediaTime(0)).unwrap().output;
        let playback = clock
            .update_outputs(session, &output.scenes(), MediaTime(0))
            .unwrap();
        render_output(&session.snapshot(), &output, 8, 4, &playback).unwrap()
    };
    assert_eq!(
        pixel(&sample(&mut session, &mut controller), 0, 0),
        [188, 188, 188, 255]
    );
    controller.before_change(&session).unwrap();
    session
        .apply(Command::SetSceneVideoFilters {
            scene: outer,
            filters: root_opacity(0.25),
        })
        .unwrap();
    assert_eq!(
        pixel(&sample(&mut session, &mut controller), 0, 0),
        [188, 188, 188, 255],
        "private root retains captured half, live Preview would be quarter"
    );
    assert_eq!(
        pixel(&render_scene_now(&session, outer), 0, 0),
        [137, 137, 137, 255]
    );
    controller.before_change(&session).unwrap();
    session
        .apply(Command::SetSceneVideoFilters {
            scene: child,
            filters: root_opacity(0.5),
        })
        .unwrap();
    assert_eq!(
        pixel(&sample(&mut session, &mut controller), 0, 0),
        [137, 137, 137, 255],
        "nested owner remains live"
    );
    controller.before_change(&session).unwrap();
    session
        .apply(Command::RemoveScene { scene: child })
        .unwrap();
    assert_eq!(
        pixel(&sample(&mut session, &mut controller), 0, 0),
        [137, 137, 137, 255],
        "removed nested fallback retains latest source properties"
    );
}
#[test]
fn root_filter_is_applied_to_transparent_native_canvas_before_black_flatten() {
    let (mut session, outer, _) = setup();
    let mut effect = skylive_core::VideoEffect::color_correction();
    if let skylive_core::VideoEffect::ColorCorrection { brightness, .. } = &mut effect {
        *brightness = 1.0;
    }
    session
        .apply(Command::SetSceneVideoFilters {
            scene: outer,
            filters: vec![skylive_core::VideoFilter::new(effect).unwrap()],
        })
        .unwrap();
    // Shader matrix translation uses alpha as its homogeneous component.
    // Transparent canvas remains transparent; flattening before the shader
    // would incorrectly brighten an empty Scene to white.
    assert_eq!(
        pixel(&render_scene_now(&session, outer), 0, 0),
        [0, 0, 0, 255]
    );
}

#[test]
fn private_old_a_can_reach_new_live_a_through_b_without_false_cycle_or_audio_loss() {
    let (mut session, a, b) = setup();
    let nested = session
        .apply(Command::AddSceneSource {
            scene: a,
            target: b,
        })
        .unwrap()
        .created
        .unwrap();
    color(&mut session, b, "Old B white", [255; 4]);
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    controller.before_change(&session).unwrap();
    session
        .apply(Command::RemoveItem {
            scene: a,
            item: nested,
        })
        .unwrap();
    color(&mut session, a, "New A green", GREEN);
    session
        .apply(Command::AddAudioCaptureSource {
            scene: a,
            name: "New A input".into(),
            binding: skylive_core::AudioCaptureBinding::new(
                skylive_core::AudioFlow::Input,
                skylive_core::AudioTarget::Default,
                false,
                false,
            )
            .unwrap(),
        })
        .unwrap();
    controller.before_change(&session).unwrap();
    session
        .apply(Command::AddSceneSource {
            scene: b,
            target: a,
        })
        .unwrap();
    let state = controller.sync(&mut session, MediaTime(0)).unwrap();
    let mut clock = PlaybackClock::new();
    let playback = clock
        .update_outputs(&session, &state.output.scenes(), MediaTime(0))
        .unwrap();
    let image = render_output(&session.snapshot(), &state.output, 8, 4, &playback).unwrap();
    assert_eq!(
        pixel(&image, 0, 0),
        GREEN,
        "old private A→live B→new live A is not self recursion"
    );
    let plan = crate::audio::derive_audio_plan(&session, &state.output).unwrap();
    assert_eq!(
        plan.sources().len(),
        1,
        "new live A input must be discovered once"
    );
    let before = session.snapshot();
    assert!(
        session
            .apply(Command::AddSceneSource {
                scene: a,
                target: b
            })
            .is_err(),
        "real live A↔B cycle remains invalid"
    );
    assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
    // Retire both canonical targets. The frozen closure must have separately
    // retained latest live A, instead of mistaking its private root for A.
    controller.before_change(&session).unwrap();
    let third = session
        .apply(Command::AddScene {
            name: "Survivor".into(),
        })
        .unwrap()
        .created
        .unwrap();
    controller.before_change(&session).unwrap();
    session.apply(Command::RemoveScene { scene: b }).unwrap();
    // Capture the refreshed immutable output while A remains canonical. A
    // subsequent root deletion legitimately changes the Controller's selected
    // Program; the already-issued output still owns its retained dependencies.
    let state = controller.sync(&mut session, MediaTime(0)).unwrap();
    controller.before_change(&session).unwrap();
    session.apply(Command::RemoveScene { scene: a }).unwrap();
    assert!(session.snapshot().scene(third).is_some());
    // Deleted logical Program selections are invalid. Preserve the issued
    // private visual under the surviving selection explicitly, as the FTB
    // return path does, instead of bypassing the renderer's freshness checks.
    let retained_output = FrameComposition::SceneAlias {
        scene: state.output.scenes().into_iter().next().unwrap(),
        logical_scene: third,
        epoch: state.epoch,
    };
    let playback = clock
        .update_outputs(&session, &retained_output.scenes(), MediaTime(0))
        .unwrap();
    let image = render_output(&session.snapshot(), &retained_output, 8, 4, &playback).unwrap();
    assert_eq!(
        pixel(&image, 0, 0),
        GREEN,
        "private root and retired live A each keep their own layout"
    );
    assert_eq!(
        crate::audio::derive_audio_plan(&session, &retained_output)
            .unwrap()
            .sources()
            .len(),
        1
    );
}
