use super::*;
#[test]
fn source_channel_properties_are_atomic_undoable_and_noops_preserve_snapshot() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Voice".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    let old = session.snapshot();
    let defaults = SourceAudioProperties::default();
    assert!(
        !session
            .apply(Command::SetSourceAudioProperties {
                source,
                properties: defaults
            })
            .unwrap()
            .changed
    );
    assert!(Arc::ptr_eq(&old, &session.snapshot()));
    let properties = SourceAudioProperties::new(true, 0.25).unwrap();
    assert!(
        session
            .apply(Command::SetSourceAudioProperties { source, properties })
            .unwrap()
            .changed
    );
    assert_eq!(
        *session
            .snapshot()
            .source(source)
            .unwrap()
            .audio_properties(),
        properties
    );
    assert!(old.source(source).unwrap().audio_properties().is_default());
    session.apply(Command::Undo).unwrap();
    assert!(
        session
            .snapshot()
            .source(source)
            .unwrap()
            .audio_properties()
            .is_default()
    );
    session.apply(Command::Redo).unwrap();
    assert_eq!(
        *session
            .snapshot()
            .source(source)
            .unwrap()
            .audio_properties(),
        properties
    );
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Color".into(),
            rgba: [1, 2, 3, 255],
        })
        .unwrap();
    let color = session
        .snapshot()
        .sources()
        .iter()
        .find(|source| matches!(source.kind(), SourceKind::Color { .. }))
        .unwrap()
        .id();
    let before = session.snapshot();
    assert!(
        session
            .apply(Command::SetSourceAudioProperties {
                source: color,
                properties
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    assert!(
        session
            .apply(Command::SetSourceAudioProperties {
                source: Id::new(u64::MAX),
                properties
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
}
