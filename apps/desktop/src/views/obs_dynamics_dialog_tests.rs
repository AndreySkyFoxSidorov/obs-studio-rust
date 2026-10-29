use super::*;
#[test]
fn builtin_dynamics_real_add_cancel_invalid_apply_and_atomic_undo() {
    use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget, Command};
    let mut session = skylive_core::Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Dynamics input".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    let baseline = session.snapshot();
    let context = egui::Context::default();
    crate::theme::install(&context);
    let mut slot = Some(Dialog::audio_filters(baseline.source(source).unwrap()));
    for label in ["Add OBS Limiter", "Add OBS Compressor", "Add OBS Expander"] {
        assert!(click(&context, &session, &mut slot, label).is_empty());
    }
    assert!(
        matches!(&slot,Some(Dialog::AudioFilters{filters,..})if filters==&vec![AudioFilter::obs_limiter(),AudioFilter::obs_compressor(),AudioFilter::obs_expander()])
    );
    click(&context, &session, &mut slot, "Cancel");
    assert!(slot.is_none());
    assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
    slot = Some(Dialog::audio_filters(baseline.source(source).unwrap()));
    click(&context, &session, &mut slot, "Add OBS Limiter");
    if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
        filters[0] = AudioFilter::ObsLimiter {
            threshold_db: 0.5,
            release_ms: 60,
            enabled: true,
        };
    }
    assert!(matches!(
        click(&context, &session, &mut slot, "Apply").as_slice(),
        [Intent::DialogError(_)]
    ));
    assert!(
        matches!(&slot,Some(Dialog::AudioFilters{filters,..})if matches!(filters[0],AudioFilter::ObsLimiter{threshold_db,..}if threshold_db==0.5))
    );
    assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
    if let Some(Dialog::AudioFilters { filters, .. }) = &mut slot {
        filters[0] = AudioFilter::obs_limiter();
    }
    let command = click(&context, &session, &mut slot, "Apply")
        .into_iter()
        .find_map(|i| {
            if let Intent::ApplyPluginEditor { command: c, .. } = i {
                Some(c)
            } else {
                None
            }
        })
        .unwrap();
    session.apply(command).unwrap();
    assert_eq!(session.snapshot().revision(), baseline.revision() + 1);
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        [AudioFilter::obs_limiter()]
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
