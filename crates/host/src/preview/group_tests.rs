//! Real private-container rasters, shared leaves and retained Program lifetime.
use super::*;
use crate::{playback::PlaybackClock, transition::Controller};
use skylive_core::{
    AudioCaptureBinding, AudioFlow, AudioTarget, Command, SourceKind, Transform, TransformAction,
};
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn audio_only_media_nested_groups_are_nonvisual_until_a_real_video_child_is_added() {
    let mut session = Session::new();
    let root = session.preview();
    let media_item = session
        .apply(Command::AddMediaSource {
            scene: root,
            name: "Audio file".into(),
            media: skylive_core::MediaSource::new("audio-only.mp4".into(), 0, 0, true).unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let inner = session
        .apply(Command::GroupItems {
            scene: root,
            name: "Inner audio".into(),
            items: vec![media_item],
        })
        .unwrap()
        .created
        .unwrap();
    let outer = session
        .apply(Command::AddGroup {
            scene: root,
            name: "Outer audio".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::MoveIntoGroup {
            scene: root,
            item: inner,
            group: outer,
        })
        .unwrap();
    let model = session.snapshot();
    let item = model
        .scene(root)
        .unwrap()
        .items()
        .iter()
        .find(|item| item.id() == outer)
        .unwrap();
    let source = model.source(item.source()).unwrap();
    let outer_source = source.id();
    assert_eq!(
        crate::transition::FrameScene::Live(root)
            .source_video_dimensions(&model, source)
            .unwrap(),
        None
    );
    let SourceKind::Group { group } = source.kind() else {
        panic!("outer must be group");
    };
    let outer_container = group.scene();
    assert_eq!(
        model.group_native_dimensions(outer_container).unwrap(),
        None
    );
    let nested = model
        .scene_container(outer_container)
        .unwrap()
        .items()
        .first()
        .unwrap();
    let SourceKind::Group { group } = model.source(nested.source()).unwrap().kind() else {
        panic!("inner must be group");
    };
    let inner_container = group.scene();
    let bytes = crate::storage::encode(&model).unwrap();
    let restored = crate::storage::decode(&bytes, false).unwrap();
    let restored = restored.snapshot();
    let restored_source = restored.source(outer_source).unwrap();
    assert_eq!(
        crate::transition::FrameScene::Live(root)
            .source_video_dimensions(&restored, restored_source)
            .unwrap(),
        None
    );
    // A true visual leaf makes both ancestors visual. The audio-only item's
    // inert transform and source resource remain unchanged.
    let audio_before = model
        .scene_container(inner_container)
        .unwrap()
        .items()
        .first()
        .unwrap()
        .clone();
    session
        .apply(Command::AddColorSource {
            scene: inner_container,
            name: "Visible".into(),
            rgba: RED,
        })
        .unwrap();
    let model = session.snapshot();
    let source = model.source(outer_source).unwrap();
    assert!(
        crate::transition::FrameScene::Live(root)
            .source_video_dimensions(&model, source)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        model
            .scene_container(inner_container)
            .unwrap()
            .items()
            .iter()
            .find(|item| item.id() == audio_before.id())
            .unwrap(),
        &audio_before
    );
}
fn setup() -> (Session, Id, Id, Id) {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let mut leaves = vec![];
    for (name, rgba, x) in [("Red", RED, 2.0), ("Blue", BLUE, 8.0)] {
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: name.into(),
                rgba,
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
            .find(|entry| entry.id() == item)
            .unwrap()
            .source();
        session
            .apply(Command::SetColorSize {
                source,
                width: 4,
                height: 2,
            })
            .unwrap();
        let mut transform = Transform::full(4, 2);
        transform.x = x;
        transform.y = 2.0;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        leaves.push(item);
    }
    let group = session
        .apply(Command::GroupItems {
            scene,
            name: "Group".into(),
            items: leaves,
        })
        .unwrap()
        .created
        .unwrap();
    let model = session.snapshot();
    let source = model.scene(scene).unwrap().items()[0].source();
    let SourceKind::Group { group: binding } = model.source(source).unwrap().kind() else {
        panic!("group source");
    };
    let private = binding.scene();
    (session, scene, group, private)
}
fn pixel(image: &RgbaFrame, x: usize, y: usize) -> [u8; 4] {
    image.pixels()[(y * image.width() as usize + x) * 4..][..4]
        .try_into()
        .unwrap()
}
fn image(session: &Session, scene: Id) -> RgbaFrame {
    render_at(&session.snapshot(), scene, 16, 16, MediaTime(0)).unwrap()
}
#[test]
fn literal_group_pixels_flip_rotation_crop_ungroup_and_native_roundtrip() {
    let (mut session, scene, item, _) = setup();
    let original = image(&session, scene);
    for y in 0..16 {
        for x in 0..16 {
            assert_eq!(
                pixel(&original, x, y),
                if (2..4).contains(&y) && (2..6).contains(&x) {
                    RED
                } else if (2..4).contains(&y) && (8..12).contains(&x) {
                    BLUE
                } else {
                    BLACK
                }
            );
        }
    }
    session
        .apply(Command::TransformItem {
            scene,
            item,
            action: TransformAction::FlipHorizontal,
        })
        .unwrap();
    let flipped = image(&session, scene);
    for x in 2..6 {
        assert_eq!(pixel(&flipped, x, 2), BLUE);
    }
    for x in 8..12 {
        assert_eq!(pixel(&flipped, x, 2), RED);
    }
    session.apply(Command::Undo).unwrap();
    let mut rotated = Transform::full(10, 2);
    rotated.x = 6.0;
    rotated.y = 2.0;
    rotated.rotation_degrees = 90.0;
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: rotated,
        })
        .unwrap();
    let rotated = image(&session, scene);
    assert_eq!(pixel(&rotated, 4, 2), RED);
    assert_eq!(pixel(&rotated, 5, 5), RED);
    assert_eq!(pixel(&rotated, 4, 8), BLUE);
    assert_eq!(pixel(&rotated, 5, 11), BLUE);
    assert_eq!(pixel(&rotated, 6, 2), BLACK);
    session.apply(Command::Undo).unwrap();
    let mut crop = Transform::full(10, 2);
    crop.x = 2.0;
    crop.y = 2.0;
    crop.crop.left = 2;
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: crop,
        })
        .unwrap();
    let cropped = image(&session, scene);
    assert_eq!(pixel(&cropped, 2, 2), RED);
    assert_eq!(pixel(&cropped, 3, 2), RED);
    assert_eq!(pixel(&cropped, 4, 2), BLACK);
    assert_eq!(pixel(&cropped, 6, 2), BLUE);
    assert_eq!(pixel(&cropped, 9, 2), BLUE);
    let native = crate::storage::encode(&session.snapshot()).unwrap();
    let restored = crate::storage::decode(&native, true).unwrap();
    assert_eq!(image(&restored, scene).pixels(), cropped.pixels());
    session.apply(Command::Ungroup { scene, item }).unwrap();
    assert_eq!(
        image(&session, scene).pixels(),
        original.pixels(),
        "OBS ungroup removes the group's clipping rather than copying it to independent children"
    );
    session.apply(Command::Undo).unwrap();
    assert_eq!(image(&session, scene).pixels(), cropped.pixels());
}
#[test]
fn empty_and_audio_only_groups_have_no_raster_and_shared_audio_is_mixed_once() {
    let mut session = Session::new();
    let scene = session.preview();
    let item = session
        .apply(Command::AddGroup {
            scene,
            name: "Audio group".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let model = session.snapshot();
    let source = model.scene(scene).unwrap().items()[0].source();
    let SourceKind::Group { group } = model.source(source).unwrap().kind() else {
        panic!("group");
    };
    let private = group.scene();
    assert_eq!(render(&model, scene, 1, 1).unwrap().pixels(), &BLACK);
    let child = session
        .apply(Command::AddAudioCaptureSource {
            scene: private,
            name: "Shared microphone".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let audio = session.snapshot().scene_container(private).unwrap().items()[0].source();
    session
        .apply(Command::AddExistingSource {
            scene,
            source: audio,
        })
        .unwrap();
    let output = FrameComposition::Scene(FrameScene::Live(scene));
    let plan = crate::audio::derive_audio_plan(&session, &output).unwrap();
    assert_eq!(plan.sources().len(), 1);
    assert_eq!(plan.sources()[0].source, audio);
    assert_eq!(
        render(&session.snapshot(), scene, 1, 1).unwrap().pixels(),
        &BLACK
    );
    session
        .apply(Command::SetVisible {
            scene,
            item,
            visible: false,
        })
        .unwrap();
    session
        .apply(Command::RemoveItem {
            scene: private,
            item: child,
        })
        .unwrap();
    assert!(session.snapshot().source(audio).is_some());
}
#[test]
fn held_program_group_is_live_then_retains_last_observed_private_container_after_removal() {
    let (mut session, scene, item, private) = setup();
    let mut owner = Controller::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    let mut clock = PlaybackClock::new();
    let mut sample = |session: &mut Session, owner: &mut Controller| {
        let snapshot = owner.sync(session, MediaTime(0)).unwrap();
        let playback = clock
            .update_outputs(session, &snapshot.output.scenes(), MediaTime(0))
            .unwrap();
        render_output(&session.snapshot(), &snapshot.output, 16, 16, &playback).unwrap()
    };
    assert_eq!(pixel(&sample(&mut session, &mut owner), 2, 2), RED);
    owner.before_change(&session).unwrap();
    let leaf = session.snapshot().scene_container(private).unwrap().items()[0].source();
    session
        .apply(Command::SetColor {
            source: leaf,
            rgba: [0, 255, 0, 255],
        })
        .unwrap();
    assert_eq!(
        pixel(&sample(&mut session, &mut owner), 2, 2),
        [0, 255, 0, 255]
    );
    owner.before_change(&session).unwrap();
    session.apply(Command::RemoveItem { scene, item }).unwrap();
    assert!(session.snapshot().groups().is_empty());
    assert_eq!(
        pixel(&sample(&mut session, &mut owner), 2, 2),
        [0, 255, 0, 255]
    );
}
#[test]
fn group_alpha_is_composited_as_linear_scene_content_and_hidden_parent_is_empty() {
    let (mut session, scene, item, private) = setup();
    let source = session.snapshot().scene_container(private).unwrap().items()[0].source();
    session
        .apply(Command::SetColor {
            source,
            rgba: [255, 255, 255, 128],
        })
        .unwrap();
    assert_eq!(pixel(&image(&session, scene), 2, 2), [188, 188, 188, 255]);
    session
        .apply(Command::SetVisible {
            scene,
            item,
            visible: false,
        })
        .unwrap();
    assert!(
        image(&session, scene)
            .pixels()
            .chunks_exact(4)
            .all(|pixel| pixel == BLACK)
    );
}
