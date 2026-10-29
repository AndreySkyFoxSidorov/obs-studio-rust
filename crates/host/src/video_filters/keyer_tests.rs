use super::*;
use skylive_core::{Command, Session, VideoFilter};
fn authored(effect: VideoEffect) -> (Session, skylive_core::Id) {
    let mut session = Session::new();
    session
        .apply(Command::AddColorSource {
            scene: session.preview(),
            name: "Background".into(),
            rgba: [0, 255, 0, 255],
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    session
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: vec![
                VideoFilter::new(effect)
                    .unwrap()
                    .with_name("Мій ключ".into())
                    .unwrap(),
            ],
        })
        .unwrap();
    (session, source)
}
#[test]
fn scratch_and_texel_work_share_admission_and_cached_results_cost_neither() {
    let (session, id) = authored(VideoEffect::chroma_key_v2());
    let model = session.snapshot();
    let source = model.source(id).unwrap();
    let input = Arc::new(RgbaFrame::solid(2, 2, [0, 255, 0, 255]).unwrap());
    let mut cache = FilterCache::default();
    assert_eq!(
        cache
            .apply(source, input.clone(), false, 111, 4)
            .err()
            .unwrap()
            .kind,
        crate::resource_issue::ResourceIssueKind::Limit
    );
    assert_eq!((cache.bytes, cache.texel_visits), (0, 0));
    let result = cache.apply(source, input.clone(), false, 112, 4).unwrap();
    assert_eq!(result.image.pixels(), &[0; 16]);
    assert_eq!(result.new_bytes, 16);
    assert_eq!(cache.texel_visits, 36);
    let hit = cache.apply(source, input.clone(), false, 0, 0).unwrap();
    assert!(Arc::ptr_eq(&hit.image, &result.image));
    assert_eq!(cache.texel_visits, 36);
    let other = Arc::new(RgbaFrame::solid(2, 2, [255, 0, 0, 255]).unwrap());
    cache.texel_visits = MAX_TEXEL_VISITS - 35;
    assert!(cache.apply(source, other, false, 112, 4).is_err());
    assert_eq!(cache.texel_visits, MAX_TEXEL_VISITS - 35);
}
#[test]
fn authored_keyer_settings_identity_and_name_survive_native_undo_and_exact_cache_keys() {
    let (mut session, id) = authored(VideoEffect::color_key_v2());
    let old = session.snapshot();
    let filter = old.source(id).unwrap().video_filters()[0].clone();
    let identity = filter.id();
    let input = Arc::new(RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap());
    let mut cache = FilterCache::default();
    let a = cache
        .apply(old.source(id).unwrap(), input.clone(), false, 4, 1)
        .unwrap();
    let mut changed = filter.clone();
    if let VideoEffect::ColorKey {
        gamma, key_color, ..
    } = &mut changed.effect
    {
        *gamma = f64::from_bits(0.2f64.to_bits() + 1);
        *key_color = [1, 2, 3];
    }
    session
        .apply(Command::SetSourceVideoFilters {
            source: id,
            filters: vec![changed],
        })
        .unwrap();
    let current = session.snapshot();
    let b = cache
        .apply(current.source(id).unwrap(), input, false, 4, 1)
        .unwrap();
    assert!(!Arc::ptr_eq(&a.image, &b.image));
    assert_eq!(
        current.source(id).unwrap().video_filters()[0].id(),
        identity
    );
    let reopened =
        crate::storage::decode(&crate::storage::encode(&current).unwrap(), true).unwrap();
    assert_eq!(
        reopened.snapshot().source(id).unwrap().video_filters(),
        current.source(id).unwrap().video_filters()
    );
    session.apply(Command::Undo).unwrap();
    assert_eq!(
        session.snapshot().source(id).unwrap().video_filters(),
        &[filter]
    );
}

// These exercise the bounded shared owner rather than storing an unrelated
// synthetic frame in a second cache. No camera or application window is used.
fn camera_scope(session:u64, source:skylive_core::Id) -> CameraScope {
    let mode=skylive_core::VideoCaptureMode::new(2,2,30,1,skylive_core::VideoPixelFormat::Rgb32).unwrap();
    let binding=skylive_core::VideoCaptureBinding::new(
        skylive_core::VideoCaptureTarget::Windows {symbolic_link:"test-camera".into()},mode,false).unwrap();
    CameraScope {session,source,binding:crate::capture::CaptureBinding::Video(Arc::new(binding)),
        origin:crate::capture::NativeFrameOrigin::Camera{owner:41,stream:42}}
}
fn camera_test_providers()->FilterProviders {
    FilterProviders {canvas:None,craft:false,person:false,freeze:false}
}

#[test]
fn camera_fallback_reuses_only_a_completed_filtered_frame_and_retains_its_lease() {
    let (session,id)=authored(VideoEffect::color_key_v2());
    let model=session.snapshot();let source=model.source(id).unwrap();
    let scope=camera_scope(session.identity(),id);
    let runtime=SharedSourceFilters::new();
    let original=Arc::new(RgbaFrame::solid(2,2,[10,180,40,255]).unwrap());
    let mut first=FilterCache::default().with_shared(Some(runtime.clone()));
    first.camera_scope=Some(scope.clone());
    let completed=first.apply_with_providers(source,original,SourceBlend::LinearSrgb,camera_test_providers(),8*1024*1024,1024*1024).unwrap();
    assert!(runtime.stats().charged_bytes>=completed.image.byte_len());
    drop(first);
    let current=Arc::new(RgbaFrame::solid(2,2,[90,40,10,255]).unwrap());
    let mut later=FilterCache::default().with_shared(Some(runtime.clone()));
    later.camera_scope=Some(scope);
    let fallback=later.camera_fallback(source,&current,SourceBlend::LinearSrgb,camera_test_providers(),8*1024*1024).unwrap();
    assert!(Arc::ptr_eq(&fallback.image,&completed.image));
    assert!(!Arc::ptr_eq(&fallback.image,&current));
    assert_eq!(fallback.new_bytes,completed.image.byte_len());
    assert_eq!(later.camera_fallback_leases.len(),1);
    assert!(runtime.stats().charged_bytes>=fallback.image.byte_len());
    assert!(later.camera_fallback(source,&current,SourceBlend::LinearSrgb,camera_test_providers(),0).is_none());
}

#[test]
fn camera_fallback_rejects_changed_identity_settings_and_provider() {
    let (mut session,id)=authored(VideoEffect::color_key_v2());
    let model=session.snapshot();let source=model.source(id).unwrap();
    let scope=camera_scope(session.identity(),id);
    let runtime=SharedSourceFilters::new();
    let input=Arc::new(RgbaFrame::solid(2,2,[10,180,40,255]).unwrap());
    let mut cache=FilterCache::default().with_shared(Some(runtime));cache.camera_scope=Some(scope.clone());
    cache.apply_with_providers(source,input.clone(),SourceBlend::LinearSrgb,camera_test_providers(),8*1024*1024,1024*1024).unwrap();
    let lookup=|cache:&mut FilterCache,source:&skylive_core::Source,providers|cache.camera_fallback(source,&input,SourceBlend::LinearSrgb,providers,8*1024*1024).is_some();
    let mut other=scope.clone();other.source=skylive_core::Id::new(id.get()+1);cache.camera_scope=Some(other);
    assert!(!lookup(&mut cache,source,camera_test_providers()));
    let mut other=scope.clone();other.session+=1;cache.camera_scope=Some(other);
    assert!(!lookup(&mut cache,source,camera_test_providers()));
    let mut other=scope.clone();other.origin=crate::capture::NativeFrameOrigin::Camera{owner:41,stream:43};cache.camera_scope=Some(other);
    assert!(!lookup(&mut cache,source,camera_test_providers()));
    let mut other=scope.clone();other.origin=crate::capture::NativeFrameOrigin::Camera{owner:99,stream:42};cache.camera_scope=Some(other);
    assert!(!lookup(&mut cache,source,camera_test_providers()));
    let mut other=scope.clone();other.binding=camera_scope(session.identity()+1,id).binding;
    if let crate::capture::CaptureBinding::Video(binding)=&mut other.binding {
        let mode=skylive_core::VideoCaptureMode::new(2,2,30,1,skylive_core::VideoPixelFormat::Rgb32).unwrap();
        *binding=Arc::new(skylive_core::VideoCaptureBinding::new(skylive_core::VideoCaptureTarget::Windows {symbolic_link:"other-camera".into()},mode,false).unwrap());
    }
    cache.camera_scope=Some(other);assert!(!lookup(&mut cache,source,camera_test_providers()));
    cache.camera_scope=Some(scope);
    let mut providers=camera_test_providers();providers.craft=true;
    assert!(!lookup(&mut cache,source,providers));
    let mut changed=source.video_filters()[0].clone();
    if let VideoEffect::ColorKey {similarity,..}=&mut changed.effect {*similarity+=1;}
    session.apply(Command::SetSourceVideoFilters{source:id,filters:vec![changed]}).unwrap();
    let changed_model=session.snapshot();
    assert!(!lookup(&mut cache,changed_model.source(id).unwrap(),camera_test_providers()));
}

#[test]
fn camera_fallback_rejects_dynamic_or_resource_effects() {
    let (mut session,id)=authored(VideoEffect::color_key_v2());
    let model=session.snapshot();let source=model.source(id).unwrap();
    let runtime=SharedSourceFilters::new();let input=Arc::new(RgbaFrame::solid(2,2,[10,180,40,255]).unwrap());
    let mut cache=FilterCache::default().with_shared(Some(runtime));
    cache.camera_scope=Some(camera_scope(session.identity(),id));
    cache.apply_with_providers(source,input.clone(),SourceBlend::LinearSrgb,camera_test_providers(),8*1024*1024,1024*1024).unwrap();
    for extra in [VideoEffect::person_background_removal(),VideoEffect::apply_lut(),VideoEffect::image_mask_v2(),VideoEffect::freeze_filter(),VideoEffect::render_delay(),VideoEffect::scroll(),VideoEffect::scale_filter()] {
        let extra_filter=VideoFilter::new(extra).unwrap();
        if !session.snapshot().preferences().plugin_enabled(extra_filter.plugin_id()) {
            session.apply(Command::SetPluginEnabled {plugin:extra_filter.plugin_id().to_owned(),enabled:true}).unwrap();
        }
        let filters=vec![source.video_filters()[0].clone(),extra_filter];
        session.apply(Command::SetSourceVideoFilters{source:id,filters}).unwrap();
        let changed=session.snapshot();
        assert!(cache.camera_fallback(changed.source(id).unwrap(),&input,SourceBlend::LinearSrgb,camera_test_providers(),8*1024*1024).is_none(),"{extra:?}");
    }
}
