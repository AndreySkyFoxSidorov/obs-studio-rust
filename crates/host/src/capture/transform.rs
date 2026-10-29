//! Pure presentation projection for live capture transform editors. Context
//! stores authored metadata and observed dimensions, never image ownership.
use super::{CaptureBinding, CaptureError, CaptureSnapshot};
use skylive_core::{Command, Id, SceneItem, Session, Source, StudioSnapshot, Transform};

#[derive(Clone, Debug)]
pub struct CaptureTransformContext {
    session: u64,
    scene: Id,
    item: Id,
    source: Id,
    binding: Option<CaptureBinding>,
    basis:(u32,u32),
    canvas:(u32,u32),
    revision:u64,
    original_authored: Transform,
    original_projected: Transform,
    observed_frame_size: Option<(u32, u32)>,
}
impl CaptureTransformContext {
    pub fn observed_frame_size(&self) -> Option<(u32, u32)> {
        self.observed_frame_size
    }
}
#[derive(Clone, Debug)]
pub struct CaptureTransformDraft {
    pub transform: Transform,
    pub context: CaptureTransformContext,
}

fn selected(
    current: &StudioSnapshot,
    scene: Id,
    item: Id,
) -> Result<(&SceneItem, &Source), CaptureError> {
    let selected = current
        .scene_container(scene)
        .ok_or(CaptureError::Engine(skylive_core::Error::NotFound(scene)))?
        .items()
        .iter()
        .find(|candidate| candidate.id() == item)
        .ok_or(CaptureError::Engine(skylive_core::Error::NotFound(item)))?;
    let source = current
        .source(selected.source())
        .ok_or(CaptureError::Engine(skylive_core::Error::NotFound(
            selected.source(),
        )))?;
    Ok((selected, source))
}
fn observed_size(
    capture: &CaptureSnapshot,
    source: Id,
    binding: &CaptureBinding,
) -> Option<(u32, u32)> {
    capture
        .frame(source)
        .filter(|frame| &frame.binding == binding)
        .map(|frame| (frame.image.width(), frame.image.height()))
}
fn reopen(detail: &'static str) -> CaptureError {
    CaptureError::InvalidFrame(detail)
}

/// Project width/height into current native frame space for a display source.
/// Without a matching frame, basis dimensions remain the explicit fallback.
/// Other source types return None and require no capture-session dependency.
pub fn project_capture_transform(
    session: &Session,
    capture: &CaptureSnapshot,
    scene: Id,
    item: Id,
) -> Result<Option<CaptureTransformDraft>, CaptureError> {
    if session.is_closed() {
        return Err(CaptureError::Closed);
    }
    let current = session.snapshot();
    let (selected, source) = selected(&current, scene, item)?;
    let binding=CaptureBinding::from_source(source.kind());
    if binding.is_none() && !source.video_filters().iter().any(|f|f.enabled && current.preferences().plugin_enabled(f.plugin_id()) && matches!(f.effect,skylive_core::VideoEffect::CropPad {..}|skylive_core::VideoEffect::ScaleFilter {..}|skylive_core::VideoEffect::Scroll {..}|skylive_core::VideoEffect::GaussianBlur {..})) {return Ok(None);}
    let basis=current.source_video_dimensions(source.kind()).map_err(CaptureError::Engine)?.ok_or(reopen("source has no video geometry"))?;
    if binding.is_some() && capture.session()!=session.identity(){return Err(reopen("capture snapshot belongs to another session; reopen Transform"));}
    let observed_frame_size=binding.as_ref().and_then(|b|observed_size(capture,source.id(),b));
    let canvas=(current.video().width(),current.video().height());
    let (width,height)=current.effective_filtered_video_dimensions(source,observed_frame_size.unwrap_or(basis),Some(canvas)).map_err(CaptureError::Engine)?;
    let original_authored = selected.transform().clone();
    let mut projected = original_authored.clone();
    projected.width =
        (f64::from(original_authored.width) * f64::from(width) / f64::from(basis.0)) as f32;
    projected.height = (f64::from(original_authored.height) * f64::from(height)
        / f64::from(basis.1)) as f32;
    let context = CaptureTransformContext {
        session: session.identity(),
        scene,
        item,
        source: source.id(),
        binding: binding.clone(),
        basis,canvas,revision:current.revision(),
        original_authored,
        original_projected: projected.clone(),
        observed_frame_size,
    };
    Ok(Some(CaptureTransformDraft {
        transform: projected,
        context,
    }))
}

fn unchanged(candidate: f32, presented: f32, original: f32) -> f32 {
    if candidate == presented {
        original
    } else {
        candidate
    }
}
fn inverse(candidate: f32, presented: f32, original: f32, basis: u32, actual: u32) -> f32 {
    if candidate == presented {
        original
    } else {
        (f64::from(candidate) * f64::from(basis) / f64::from(actual)) as f32
    }
}
/// Revalidate an editor's semantic dependencies and prepare one canonical edit.
/// Changing pixels/generation alone is harmless; changing binding, frame-size
/// availability or another edit to the target transform requires reopening.
/// The presented draft itself may be outside authored extent bounds. Validate
/// only after converting to the authored basis, then check known frame crop.
pub fn prepare_capture_transform(
    session: &Session,
    capture: &CaptureSnapshot,
    scene: Id,
    item: Id,
    draft: Transform,
    context: &CaptureTransformContext,
) -> Result<Command, CaptureError> {
    if session.is_closed() {
        return Err(CaptureError::Closed);
    }
    if context.session != session.identity() || (context.binding.is_some() && capture.session()!=session.identity()) {
        return Err(reopen(
            "capture transform session changed; reopen Transform",
        ));
    }
    if context.scene != scene || context.item != item {
        return Err(reopen("capture transform target changed; reopen Transform"));
    }
    let current = session.snapshot();
    let (selected, source) = selected(&current, scene, item).map_err(|_| {
        reopen("capture transform item or source is no longer available; reopen Transform")
    })?;
    if source.id() != context.source {
        return Err(reopen(
            "capture transform source identity changed; reopen Transform",
        ));
    }
    let binding=CaptureBinding::from_source(source.kind());
    let basis=current.source_video_dimensions(source.kind()).map_err(CaptureError::Engine)?.ok_or(reopen("source no longer has video geometry"))?;
    if binding!=context.binding || basis!=context.basis || (current.video().width(),current.video().height())!=context.canvas || current.revision()!=context.revision {
        return Err(reopen("source/filter dimensions or revision changed while editing; reopen Transform"));
    }
    if selected.transform()!=&context.original_authored {return Err(reopen("source transform changed while editing; reopen Transform"));}
    let observed=binding.as_ref().and_then(|b|observed_size(capture,source.id(),b));
    if observed!=context.observed_frame_size {return Err(reopen("source frame dimensions or availability changed while editing; reopen Transform"));}
    let actual=current.effective_filtered_video_dimensions(source,observed.unwrap_or(basis),Some(context.canvas)).map_err(CaptureError::Engine)?;
    let original = &context.original_authored;
    let shown = &context.original_projected;
    let mut canonical = draft;
    canonical.width = inverse(
        canonical.width,
        shown.width,
        original.width,
        basis.0,
        actual.0,
    );
    canonical.height = inverse(
        canonical.height,
        shown.height,
        original.height,
        basis.1,
        actual.1,
    );
    canonical.x = unchanged(canonical.x, shown.x, original.x);
    canonical.y = unchanged(canonical.y, shown.y, original.y);
    canonical.rotation_degrees = unchanged(
        canonical.rotation_degrees,
        shown.rotation_degrees,
        original.rotation_degrees,
    );
    canonical.opacity = unchanged(canonical.opacity, shown.opacity, original.opacity);
    canonical.geometry_for_frame(basis.0,basis.1,actual.0,actual.1).map_err(CaptureError::Engine)?;
    Ok(Command::SetTransform {
        scene,
        item,
        transform: canonical,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::CapturedFrame;
    use skylive_core::{Crop, DisplayCaptureBinding, DisplayTarget};
    use skylive_media::{MediaTime, RgbaFrame};
    use std::sync::Arc;
    fn binding(width: u32, height: u32) -> DisplayCaptureBinding {
        DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: "projection-display".into(),
            },
            true,
            width,
            height,
        )
        .unwrap()
    }
    fn snapshot(
        session: &Session,
        source: Id,
        binding: DisplayCaptureBinding,
        actual: Option<(u32, u32)>,
    ) -> CaptureSnapshot {
        let mut snapshot = CaptureSnapshot::empty(session.identity());
        if let Some((width, height)) = actual {
            snapshot.frames.insert(
                source,
                CapturedFrame {native_origin:None,
                    binding: binding.into(),
                    sequence: 1,
                    requested_at: MediaTime(0),
                    native_timestamp_ns: None,
                    native_clock_epoch: None,
                    device_qpc_ns: None, directshow_clock: None,
                    async_delay: None,draft_raw:None,
                    image: Arc::new(RgbaFrame::solid(width, height, [255; 4]).unwrap()).into(),
                    display: None,
                    window: None,
                },
            );
        }
        snapshot
    }
    fn fixture(
        basis: (u32, u32),
        actual: Option<(u32, u32)>,
        transform: Transform,
    ) -> (Session, Id, Id, Id, CaptureSnapshot) {
        let mut session = Session::new();
        let scene = session.preview();
        let binding = binding(basis.0, basis.1);
        let item = session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Display".into(),
                binding: binding.clone(),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        let snapshot = snapshot(&session, source, binding, actual);
        (session, scene, item, source, snapshot)
    }
    fn prepared(command: Command) -> Transform {
        let Command::SetTransform { transform, .. } = command else {
            panic!("expected canonical transform command")
        };
        transform
    }
    #[test]
    fn projects_world_dimensions_only_and_prepares_one_inverse_edit_without_publishing() {
        let original = Transform {
            x: 7.0,
            y: 9.0,
            width: 16.0,
            height: 8.0,
            rotation_degrees: 90.0,
            flip_x: true,
            opacity: 0.25,
            crop: Crop {
                left: 1,
                ..Default::default()
            },
            ..Transform::full(2, 2)
        };
        let (mut session, scene, item, _, capture) =
            fixture((2, 2), Some((4, 2)), original.clone());
        let before = session.snapshot();
        let mut draft = project_capture_transform(&session, &capture, scene, item)
            .unwrap()
            .unwrap();
        assert_eq!((draft.transform.width, draft.transform.height), (32.0, 8.0));
        assert_eq!(draft.context.observed_frame_size(), Some((4, 2)));
        assert_eq!(
            (
                draft.transform.x,
                draft.transform.y,
                draft.transform.rotation_degrees,
                draft.transform.flip_x,
                draft.transform.opacity,
                draft.transform.crop
            ),
            (7.0, 9.0, 90.0, true, 0.25, original.crop)
        );
        let unchanged = prepare_capture_transform(
            &session,
            &capture,
            scene,
            item,
            draft.transform.clone(),
            &draft.context,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        assert!(!session.apply(unchanged).unwrap().changed);
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        draft.transform.width = 16.0;
        draft.transform.height = 4.0;
        let command = prepare_capture_transform(
            &session,
            &capture,
            scene,
            item,
            draft.transform,
            &draft.context,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let transformed = prepared(command.clone());
        assert_eq!((transformed.width, transformed.height), (8.0, 4.0));
        assert_eq!(
            (transformed.x, transformed.y, transformed.crop),
            (7.0, 9.0, original.crop)
        );
        session.apply(command).unwrap();
        assert_eq!(session.snapshot().revision(), before.revision() + 1);
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            &original
        );
    }
    #[test]
    fn unchanged_projected_fields_restore_original_bits_despite_a_one_ulp_naive_roundtrip() {
        let original = Transform {
            width: f32::from_bits(0x3fc0_0001),
            ..Transform::full(3, 1)
        };
        let (mut session, scene, item, _, capture) =
            fixture((3, 1), Some((2, 1)), original.clone());
        let before = session.snapshot();
        let mut draft = project_capture_transform(&session, &capture, scene, item)
            .unwrap()
            .unwrap();
        assert_ne!(
            ((f64::from(draft.transform.width) * 3.0 / 2.0) as f32).to_bits(),
            original.width.to_bits(),
            "fixture exercises a lossy roundtrip"
        );
        let command = prepare_capture_transform(
            &session,
            &capture,
            scene,
            item,
            draft.transform.clone(),
            &draft.context,
        )
        .unwrap();
        assert_eq!(
            prepared(command.clone()).width.to_bits(),
            original.width.to_bits()
        );
        assert!(!session.apply(command).unwrap().changed);
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        draft.transform.x = 4.0;
        let command = prepare_capture_transform(
            &session,
            &capture,
            scene,
            item,
            draft.transform,
            &draft.context,
        )
        .unwrap();
        assert_eq!(
            prepared(command.clone()).width.to_bits(),
            original.width.to_bits()
        );
        session.apply(command).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0]
                .transform()
                .x,
            4.0
        );
    }
    #[test]
    fn live_size_binding_target_transform_and_session_changes_require_reopening() {
        let (mut session, scene, item, source, capture) =
            fixture((2, 2), Some((4, 2)), Transform::full(16, 16));
        let draft = project_capture_transform(&session, &capture, scene, item)
            .unwrap()
            .unwrap();
        let resized = snapshot(&session, source, binding(2, 2), Some((2, 2)));
        assert!(
            matches!(prepare_capture_transform(&session,&resized,scene,item,draft.transform.clone(),&draft.context),Err(CaptureError::InvalidFrame(detail))if detail.contains("reopen"))
        );
        let other = Session::from_snapshot((*session.snapshot()).clone(), true).unwrap();
        assert!(matches!(
            prepare_capture_transform(
                &other,
                &capture,
                scene,
                item,
                draft.transform.clone(),
                &draft.context
            ),
            Err(CaptureError::InvalidFrame(_))
        ));
        assert!(matches!(
            prepare_capture_transform(
                &session,
                &capture,
                Id::new(999),
                item,
                draft.transform.clone(),
                &draft.context
            ),
            Err(CaptureError::InvalidFrame(_))
        ));
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: Transform {
                    x: 1.0,
                    ..Transform::full(16, 16)
                },
            })
            .unwrap();
        let changed = session.snapshot();
        assert!(matches!(
            prepare_capture_transform(
                &session,
                &capture,
                scene,
                item,
                draft.transform.clone(),
                &draft.context
            ),
            Err(CaptureError::InvalidFrame(_))
        ));
        assert!(Arc::ptr_eq(&changed, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        session
            .apply(Command::RebindDisplayCapture {
                source,
                binding: binding(3, 2),
            })
            .unwrap();
        assert!(matches!(
            prepare_capture_transform(
                &session,
                &capture,
                scene,
                item,
                draft.transform,
                &draft.context
            ),
            Err(CaptureError::InvalidFrame(_))
        ));
        let fallback = project_capture_transform(&session, &capture, scene, item)
            .unwrap()
            .unwrap();
        assert_eq!(fallback.context.observed_frame_size(), None);
        assert_eq!(
            fallback.transform.width, 24.0,
            "mismatched old frame must not supply dimensions"
        );
    }
    #[test]
    fn missing_frame_fallback_and_content_only_updates_have_explicit_context_dependencies() {
        let (session, scene, item, source, empty) = fixture((2, 2), None, Transform::full(16, 16));
        let fallback = project_capture_transform(&session, &empty, scene, item)
            .unwrap()
            .unwrap();
        assert_eq!(fallback.transform, Transform::full(16, 16));
        assert_eq!(fallback.context.observed_frame_size(), None);
        assert!(
            prepare_capture_transform(
                &session,
                &empty,
                scene,
                item,
                fallback.transform.clone(),
                &fallback.context
            )
            .is_ok()
        );
        let available = snapshot(&session, source, binding(2, 2), Some((2, 2)));
        assert!(matches!(
            prepare_capture_transform(
                &session,
                &available,
                scene,
                item,
                fallback.transform,
                &fallback.context
            ),
            Err(CaptureError::InvalidFrame(_))
        ));
        let draft = project_capture_transform(&session, &available, scene, item)
            .unwrap()
            .unwrap();
        let mut changed = available.clone();
        changed.generation = 99;
        changed.frames.get_mut(&source).unwrap().sequence = 99;
        changed.frames.get_mut(&source).unwrap().image =
            Arc::new(RgbaFrame::solid(2, 2, [0, 0, 0, 255]).unwrap()).into();
        assert!(
            prepare_capture_transform(
                &session,
                &changed,
                scene,
                item,
                draft.transform,
                &draft.context
            )
            .is_ok()
        );
        let weak = Arc::downgrade(&available.frame(source).unwrap().image.direct_arc().unwrap());
        drop(available);
        assert!(
            weak.upgrade().is_none(),
            "editor context must never retain pixel ownership"
        );
    }
    #[test]
    fn derived_tiny_and_huge_presented_extents_validate_only_after_inverse_conversion() {
        for (basis, actual, width) in [((8192, 1), (1, 1), 0.001), ((1, 1), (2, 1), 33_554_432.0)] {
            let (session, scene, item, _, capture) = fixture(
                basis,
                Some(actual),
                Transform {
                    width,
                    ..Transform::full(basis.0, basis.1)
                },
            );
            let draft = project_capture_transform(&session, &capture, scene, item)
                .unwrap()
                .unwrap();
            assert!(draft.transform.width < 0.001 || draft.transform.width > 33_554_432.0);
            let canonical = prepared(
                prepare_capture_transform(
                    &session,
                    &capture,
                    scene,
                    item,
                    draft.transform,
                    &draft.context,
                )
                .unwrap(),
            );
            assert_eq!(canonical.width.to_bits(), width.to_bits());
        }
        let (session, scene, item, _, capture) = fixture(
            (1, 1),
            Some((2, 1)),
            Transform {
                width: 33_554_432.0,
                ..Transform::full(1, 1)
            },
        );
        let mut draft = project_capture_transform(&session, &capture, scene, item)
            .unwrap()
            .unwrap();
        draft.transform.width = 33_554_432.0;
        assert_eq!(
            prepared(
                prepare_capture_transform(
                    &session,
                    &capture,
                    scene,
                    item,
                    draft.transform,
                    &draft.context
                )
                .unwrap()
            )
            .width,
            16_777_216.0
        );
    }
    #[test]
    fn invalid_drafts_and_known_actual_crop_reject_without_publication_but_crop_can_be_repaired() {
        let (session, scene, item, _, capture) = fixture(
            (4, 2),
            Some((2, 2)),
            Transform {
                crop: Crop {
                    left: 3,
                    ..Default::default()
                },
                ..Transform::full(16, 16)
            },
        );
        let before = session.snapshot();
        let mut draft = project_capture_transform(&session, &capture, scene, item)
            .unwrap()
            .unwrap();
        assert!(matches!(
            prepare_capture_transform(
                &session,
                &capture,
                scene,
                item,
                draft.transform.clone(),
                &draft.context
            ),
            Err(CaptureError::Engine(_))
        ));
        draft.transform.crop.left = 1;
        assert!(
            prepare_capture_transform(
                &session,
                &capture,
                scene,
                item,
                draft.transform.clone(),
                &draft.context
            )
            .is_ok()
        );
        for invalid in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            let mut invalid_draft = draft.transform.clone();
            invalid_draft.width = invalid;
            assert!(
                prepare_capture_transform(
                    &session,
                    &capture,
                    scene,
                    item,
                    invalid_draft,
                    &draft.context
                )
                .is_err()
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        let mut color = Session::new();
        let scene = color.preview();
        let item = color
            .apply(Command::AddColorSource {
                scene,
                name: "Color".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        assert!(
            project_capture_transform(&color, &CaptureSnapshot::empty(0), scene, item)
                .unwrap()
                .is_none()
        );
    }
}
