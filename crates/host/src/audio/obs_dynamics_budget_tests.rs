use super::*;
#[test]
fn boxed_expander_body_is_charged_and_never_reserved_in_each_legacy_slot() {
    let (mut session, scene, source) = super::super::plugin_tests::setup();
    super::super::plugin_tests::filters(&mut session, source, vec![AudioFilter::obs_limiter()]);
    let mut plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(scene))).unwrap();
    let before = planned_state_bytes(&plan).unwrap();
    plan.sources[0].filters = vec![AudioFilter::obs_expander()];
    assert_eq!(
        planned_state_bytes(&plan).unwrap(),
        before + size_of::<ObsExpander>()
    );
    let request = AudioMixRequest {
        sample_rate: 48000,
        channels: 2,
        frames: 1,
        advance_ns: 0,
    };
    let mut state = AudioDspState::default();
    state.prepare(&plan, request, 0).unwrap();
    state
        .process_source_block(source, &mut [0.001, 0.002], &mut [0.])
        .unwrap();
    let mut original = state.clone();
    let mut template = plan.sources[0].clone();
    template.filters = vec![AudioFilter::simple_eq(); 8];
    plan.sources.clear();
    for i in 0..MAX_AUDIO_INPUTS {
        let mut entry = template.clone();
        entry.source = Id::new(1000 + i as u64);
        plan.sources.push(entry);
    }
    assert!(planned_state_bytes(&plan).unwrap() > MAX_DSP_STATE_BYTES);
    assert!(matches!(
        state.prepare(&plan, request, 0),
        Err(AudioError::Limit(_))
    ));
    assert_eq!(state.sources.len(), original.sources.len());
    let mut a = [0.001, 0.002];
    let mut b = a;
    state
        .process_source_block(source, &mut a, &mut [0.])
        .unwrap();
    original
        .process_source_block(source, &mut b, &mut [0.])
        .unwrap();
    assert_eq!(a, b, "rejected replacement preserved RMS/gain history");
    while planned_state_bytes(&plan).unwrap() > MAX_DSP_STATE_BYTES {
        plan.sources.pop();
    }
    let admitted = planned_state_bytes(&plan).unwrap();
    assert!(admitted <= MAX_DSP_STATE_BYTES);
    let mut plus = plan.clone();
    let mut extra = template;
    extra.source = Id::new(9999);
    plus.sources.push(extra);
    assert!(planned_state_bytes(&plus).unwrap() > MAX_DSP_STATE_BYTES);
    state.prepare(&plan, request, 0).unwrap();
    for entry in &state.sources {
        assert_eq!(entry.kernels.len(), entry.kernels.capacity());
        assert_eq!(entry.filters.len(), entry.filters.capacity());
    }
    assert_eq!(state.sources.len(), state.sources.capacity());
    assert!(size_of::<Kernel>() < size_of::<SimpleEq>() / 2);
}
