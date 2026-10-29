//! Preview input/overlay state. Geometry and valid commands belong to host/core.
use eframe::egui;
use skylive_core::{Command, Id, Session, Transform};
use skylive_host::{
    capture::CaptureSnapshot,
    preview_interaction::{Handle, Target, pick, pick_below, pick_for_view, pick_below_for_view, point},
    transition::FrameComposition,
};

pub struct Commit {
    pub session: u64,
    pub revision: u64,
    pub command: Command,
    pub auto_text:skylive_host::preview::AutoTextView,
}
#[derive(Default)]
pub struct Actions {
    pub selection: Option<Option<Id>>,
    pub commit: Option<Commit>,
    pub intents: Vec<crate::views::Intent>,
}
pub struct View<'a> {
    pub session: &'a Session,
    pub capture: &'a CaptureSnapshot,
    pub auto_text:Option<&'a skylive_host::preview::AutoTextView>,
    pub scene: Id,
    pub selected: Option<Id>,
    pub canvas: egui::Rect,
    pub enabled: bool,
    /// Context actions remain available while direct preview editing is locked.
    pub menu_enabled: bool,
}
#[derive(Clone, Copy)]
enum Mode {
    Move,
    Rotate,
    Resize(Handle),
    GroupResize(Handle),
    GroupRotate,
    Keyboard,
}
struct Gesture {
    target: Target,
    /// Other unlocked top-level items participating in the same atomic drag.
    group: Vec<Target>,
    group_draft: Vec<(Id, Transform)>,
    keyboard_delta:[f64;2],
    snap_guides:[Option<f64>;2],
    snap_excluded:Vec<Id>,
    group_geometry:Option<skylive_core::ItemGeometry>,
    start: [f64; 2],
    draft: Transform,
    mode: Mode,
    crop: bool,
    cycle_on_click: bool,
    /// Selection before a modifier-click; a drag keeps its existing semantics.
    toggle_selection: Option<Vec<Id>>,
    moved: bool,
    epoch: u64,
    output: Option<FrameComposition>,
}
#[derive(Default)]
pub struct PreviewInteraction {
    gesture: Option<Gesture>,
    serial: u64,
    pub error: Option<String>,
}
impl PreviewInteraction {
    pub fn cancel(&mut self) {
        self.gesture = None;
    }
    pub fn draft_output(
        &mut self,
        session: &Session,
        capture: &CaptureSnapshot,
        scene: Id,
        selected: Option<Id>,
    ) -> Option<(u64, FrameComposition)> {
        let gesture = self.gesture.as_ref()?;
        if gesture.target.scene != scene || Some(gesture.target.item) != selected {
            self.cancel();
            return None;
        }
        if let Err(error) = gesture.target.validate(session, capture) {
            self.error = Some(error.to_string());
            self.cancel();
            return None;
        }
        if let Some(error)=gesture.group.iter().find_map(|target|target.validate(session,capture).err()) {
            self.error=Some(error.to_string());
            self.cancel();
            return None;
        }
        gesture.output.clone().map(|output| (gesture.epoch, output))
    }
    fn start(&mut self, target: Target, start: [f64; 2], mode: Mode, crop: bool) {
        let Some(epoch) = self.serial.checked_add(1) else {
            self.error = Some("Preview gesture identity is exhausted.".into());
            return;
        };
        self.serial = epoch;
        self.error = None;
        self.gesture = Some(Gesture {
            draft: target.transform.clone(),
            target,
            group: Vec::new(),
            group_draft: Vec::new(),
            keyboard_delta:[0.0;2],
            snap_guides:[None;2],
            snap_excluded:Vec::new(),
            group_geometry:None,
            start,
            mode,
            crop,
            cycle_on_click: false,
            toggle_selection: None,
            moved: false,
            epoch,
            output: None,
        });
    }
    fn set_draft(&mut self, session: &Session, transform: Transform) {
        let Some(gesture) = &mut self.gesture else {
            return;
        };
        if gesture.draft == transform {
            return;
        }
        let mut group_draft=Vec::new();
        let layout = if gesture.group.is_empty() && gesture.target.container==gesture.target.scene {
            gesture.target.draft_view(session,&[(gesture.target.item,transform.clone())])
        } else {
            let delta=match gesture.target.geometry_for(&transform) {
                Ok(geometry)=>[geometry.origin[0]-gesture.target.geometry.origin[0],geometry.origin[1]-gesture.target.geometry.origin[1]],
                Err(error)=>{self.error=Some(error.to_string());return;}
            };
            group_draft.push((gesture.target.item,transform.clone()));
            for target in &gesture.group {
                match target.moved(delta) {
                    Ok(value)=>group_draft.push((target.item,value)),
                    Err(error)=>{self.error=Some(error.to_string());return;}
                }
            }
            gesture.target.draft_view(session,&group_draft)
        };
        match layout {
            Ok(scene) => {
                gesture.draft = transform;
                gesture.group_draft = group_draft;
                gesture.output = Some(FrameComposition::Scene(scene));
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn set_group_scale(&mut self,session:&Session,anchor:[f64;2],factor:f64,guides:[Option<f64>;2]) {
        let Some(gesture)=&mut self.gesture else{return;};
        let mut transforms=Vec::with_capacity(gesture.group.len()+1);
        for target in std::iter::once(&gesture.target).chain(gesture.group.iter()) {
            match target.scaled_about(anchor,factor) {
                Ok(transform)=>transforms.push((target.item,transform)),
                Err(error)=>{self.error=Some(error.to_string());return;}
            }
        }
        match gesture.target.draft_view(session,&transforms) {
            Ok(scene)=>{
                gesture.draft=transforms[0].1.clone();
                gesture.group_draft=transforms;
                gesture.snap_guides=guides;
                gesture.output=Some(FrameComposition::Scene(scene));
                self.error=None;
            }
            Err(error)=>self.error=Some(error.to_string()),
        }
    }
    fn set_group_rotation(&mut self,session:&Session,anchor:[f64;2],degrees:f64) {
        let Some(gesture)=&mut self.gesture else{return;};
        let mut transforms=Vec::with_capacity(gesture.group.len()+1);
        for target in std::iter::once(&gesture.target).chain(gesture.group.iter()) {
            match target.rotated_about(anchor,degrees) {
                Ok(transform)=>transforms.push((target.item,transform)),
                Err(error)=>{self.error=Some(error.to_string());return;}
            }
        }
        match gesture.target.draft_view(session,&transforms) {
            Ok(scene)=>{gesture.draft=transforms[0].1.clone();gesture.group_draft=transforms;gesture.output=Some(FrameComposition::Scene(scene));self.error=None;}
            Err(error)=>self.error=Some(error.to_string()),
        }
    }
    fn finish(&mut self, session: &Session, capture: &CaptureSnapshot, auto_text:Option<&skylive_host::preview::AutoTextView>, actions: &mut Actions) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if gesture.draft == gesture.target.transform {
            if gesture.cycle_on_click && !gesture.moved {
                let below=if let Some(view)=auto_text {pick_below_for_view(session,capture,gesture.target.scene,gesture.start,gesture.target.item,view)} else {pick_below(session,capture,gesture.target.scene,gesture.start,gesture.target.item)};
                if let Some(target)=below {
                    actions.selection=Some(Some(target.item));
                }
            }
            return;
        }
        let command=if gesture.group.is_empty() {
            gesture.target.command(session,capture,gesture.draft)
        } else {
            let edits=gesture.group_draft.iter().map(|(item,transform)|{
                let target=std::iter::once(&gesture.target).chain(gesture.group.iter()).find(|target|target.item==*item)
                    .ok_or(skylive_host::preview_interaction::InteractionError("A selected preview item changed."))?;
                Ok((target,transform.clone()))
            }).collect::<Result<Vec<_>,skylive_host::preview_interaction::InteractionError>>();
            edits.and_then(|edits|Target::atomic_command(session,capture,&edits))
        };
        match command {
            Ok(command) => {
                actions.commit = Some(Commit {
                    session: gesture.target.session,
                    revision: gesture.target.revision,
                    auto_text:gesture.target.auto_text_view(),
                    command,
                })
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, view: View<'_>) -> Actions {
        let View {
            session,
            capture,
            auto_text,
            scene,
            selected,
            canvas,
            enabled,
            menu_enabled,
        } = view;
        let mut actions = Actions::default();
        let before = self
            .gesture
            .as_ref()
            .and_then(|gesture| gesture.output.as_ref())
            .map(FrameComposition::identity);
        if !enabled && !menu_enabled {
            self.cancel();
            return actions;
        }
        let model = session.snapshot();
        let has_auto=model.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::Text{settings} if settings.auto_extents()));
        // A gesture must target the exact geometry that produced the currently
        // displayed pixels; changed worker generations cancel the gesture.
        let enabled=enabled&&(!has_auto||auto_text.is_some_and(|view|view.matches(session.identity(),&model)));
        let read_target=|item|if let Some(view)=auto_text {Target::read_for_view(session,capture,scene,item,view)}else{Target::read(session,capture,scene,item)};
        let viewport=ui.ctx().data(|data|data.get_temp::<egui::Rect>(egui::Id::new(("preview-pane-viewport","preview"))))
            .unwrap_or(canvas).intersect(ui.clip_rect());
        let size = [
            f64::from(model.video().width()),
            f64::from(model.video().height()),
        ];
        let to_canvas = |p: egui::Pos2| {
            [
                (f64::from(p.x) - f64::from(canvas.left())) * size[0] / f64::from(canvas.width()),
                (f64::from(p.y) - f64::from(canvas.top())) * size[1] / f64::from(canvas.height()),
            ]
        };
        let to_view = |p: [f64; 2]| {
            egui::pos2(
                (f64::from(canvas.left()) + p[0] * f64::from(canvas.width()) / size[0]) as f32,
                (f64::from(canvas.top()) + p[1] * f64::from(canvas.height()) / size[1]) as f32,
            )
        };
        let response = ui.interact(
            canvas,
            ui.id().with("editable-preview"),
            egui::Sense::click_and_drag(),
        );
        let widget_info = {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Image,
                true,
                crate::i18n::tr(ui, if enabled { "Editable Preview" } else { "Preview" }),
            )
        };
        response.widget_info(|| widget_info.clone());
        if has_auto {response.clone().on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.AutoExtentsNestedEditHelp","Preview edits are checked against the displayed text geometry. Edits that cannot be mapped safely are rejected."));}
        // OBS opens the source popup for the current selection on the preview.
        // Reuse the source-list commands so filters, properties and transforms
        // continue through the same validated application actions.
        if menu_enabled { response.clone().context_menu(|ui| {
            crate::views::docks::source_list_actions(
                ui, &model, scene, selected, session.identity(), &mut actions.intents,
            );
            ui.separator();
            crate::projectors::menu(
                ui, session.identity(), model.revision(),
                skylive_host::projector::Target::Preview, &mut actions.intents,
            );
            crate::views::screenshot::button(
                ui, session.identity(), model.revision(),
                skylive_host::screenshot::Target::Preview, !session.is_closed(), &mut actions.intents,
            );
        }); }
        if !enabled {
            self.cancel();
            return actions;
        }
        response.clone().on_hover_ui(|ui| {
            ui.label(crate::i18n::tr(ui,"Select and drag a source. Drag a square handle to resize or the round handle to rotate. Hold Shift to stretch or snap rotation, Alt to crop, or Ctrl for free rotation. Arrow keys move by one pixel; Shift moves by ten. Escape cancels the current edit."));
            ui.label(crate::i18n::tr(ui,"Hold Ctrl while dragging to bypass source alignment snapping."));
            ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Preview.GroupMoveHelp","Ctrl-click toggles selection. Drag a selected source to move its selected siblings; drag the outer handles to scale uniformly."));
        });
        let (position, press, released, escape, focused, modifiers) = ui.input(|input| {
            // A slow frame can contain the complete drag. The current pointer
            // is then its endpoint, so retain the button event's real anchor.
            let press = input.events.iter().rev().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                } => Some((*pos, *modifiers)),
                _ => None,
            });
            (
                input.pointer.interact_pos(),
                press,
                input.pointer.primary_released(),
                input.key_pressed(egui::Key::Escape),
                input.focused,
                input.modifiers,
            )
        });
        if escape || !focused {
            // The preview frame was prepared before input processing. If a
            // draft was visible, schedule the next frame to restore the
            // canonical scene after cancelling the gesture.
            let had_draft=self.gesture.as_ref().is_some_and(|gesture|gesture.output.is_some());
            self.cancel();
            if had_draft {ui.ctx().request_repaint();}
            return actions;
        }
        if let Some(gesture) = &self.gesture {
            if gesture.target.scene != scene || Some(gesture.target.item) != selected {
                self.cancel();
            } else if let Err(error) = gesture.target.validate(session, capture) {
                self.error = Some(error.to_string());
                self.cancel();
            }
        }
        let (target,selected_problem)=match selected.map(|item|read_target(item)){Some(Ok(target))=>(Some(target),None),Some(Err(error))=>(None,Some(error.to_string())),None=>(None,None)};
        let group_targets:Vec<Target>=crate::views::group_source::selected_items_context(ui.ctx(),&model,scene,session.identity(),selected)
            .into_iter().filter_map(|item|read_target(item).ok()).collect();
        let group_box=if group_targets.len()>1 && target.as_ref().is_some_and(|target|group_targets.iter().all(|item|item.container==target.container)) {
            selection_geometry(&group_targets,&[])
        }else{None};
        let group_rotation_supported=group_box.is_some()&&group_targets.iter().all(Target::can_rotate_group);
        if group_box.is_some()&&!group_rotation_supported {
            response.clone().on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Preview.GroupRotationShearHelp","Group rotation is unavailable when a parent is nonuniformly scaled, because the result would require shear."));
        }
        if let Some((position, press_modifiers)) = press.filter(|(position, _)| {
            viewport.contains(*position)
                && ui.clip_rect().contains(*position)
                && ui.ctx().layer_id_at(*position) == Some(response.layer_id)
        }) {
            response.request_focus();
            {
                let handle = target.as_ref().filter(|target|target.container==scene).and_then(|target| nearest_handle(target.geometry,position,&to_view));
                let rotate=target.as_ref().is_some_and(|target| {
                    target.container==scene && rotation_handle(target.geometry,target.transform.flip_y,&to_view).1.distance(position)<=8.0
                });
                let group_handle=group_box.and_then(|geometry|nearest_handle(geometry,position,&to_view));
                let group_rotate=group_rotation_supported && group_box.is_some_and(|bounds|rotation_handle(bounds,false,&to_view).1.distance(position)<=8.0);
                if let (true,Some(target),Some(bounds))=(group_rotate,target.clone(),group_box) {
                    self.start(target.clone(),to_canvas(position),Mode::GroupRotate,false);
                    if let Some(gesture)=&mut self.gesture {
                        gesture.group=group_targets.iter().filter(|item|item.item!=target.item).cloned().collect();
                        gesture.group_geometry=Some(bounds);
                    }
                } else if let (Some(handle),Some(target),Some(bounds))=(group_handle,target.clone(),group_box) {
                    self.start(target.clone(),to_canvas(position),Mode::GroupResize(handle),false);
                    if let Some(gesture)=&mut self.gesture {
                        gesture.group=group_targets.iter().filter(|item|item.item!=target.item).cloned().collect();
                        gesture.group_geometry=Some(bounds);
                        gesture.snap_excluded=crate::views::group_source::selected_items_context(ui.ctx(),&model,scene,session.identity(),selected);
                    }
                } else if let Some(target)=target.clone().filter(|_|rotate && group_box.is_none()) {
                    self.start(target,to_canvas(position),Mode::Rotate,false);
                } else if let (Some(handle), Some(target)) = (handle.filter(|_|group_box.is_none()), target.clone()) {
                    self.start(
                        target,
                        to_canvas(position),
                        Mode::Resize(handle),
                        press_modifiers.alt,
                    );
                } else if let Some(target) = target.clone().filter(|target|target.contains(to_canvas(position)))
                    .or_else(||crate::views::group_source::selected_items_context(ui.ctx(),&model,scene,session.identity(),selected).into_iter().rev()
                        .filter_map(|item|read_target(item).ok()).find(|target|target.contains(to_canvas(position))))
                    .or_else(||if let Some(view)=auto_text{pick_for_view(session,capture,scene,to_canvas(position),view)}else{pick(session, capture, scene, to_canvas(position))}) {
                    let selection=crate::views::group_source::selected_items_context(ui.ctx(), &model, scene, session.identity(), selected);
                    let snap_excluded=if selection.contains(&target.item)&&!press_modifiers.ctrl&&!press_modifiers.command {selection.clone()}else{Vec::new()};
                    // Any member of a multi-selection can become the drag anchor.
                    let group:Vec<Target> = if selection.contains(&target.item) && selection.len()>1 && !press_modifiers.ctrl && !press_modifiers.command {
                        selection.iter().filter(|item|**item!=target.item)
                            .filter_map(|item|read_target(*item).ok()).collect()
                    } else { Vec::new() };
                    let cycle=target.container==scene && group.is_empty() && Some(target.item)==selected && !press_modifiers.alt && !press_modifiers.shift && !press_modifiers.ctrl && !press_modifiers.command;
                    let toggle = if press_modifiers.ctrl || press_modifiers.command {
                        Some(crate::views::group_source::selected_items_context(ui.ctx(), &model, scene, session.identity(), selected))
                    } else { None };
                    if !group.is_empty() {
                        let _=crate::views::group_source::focus_preview_selection(ui.ctx(),&model,scene,session.identity(),target.item,selection);
                    }
                    actions.selection = Some(Some(target.item));
                    self.start(target, to_canvas(position), Mode::Move, false);
                    if let Some(gesture)=&mut self.gesture {gesture.cycle_on_click=cycle;gesture.toggle_selection=toggle;gesture.group=group;gesture.snap_excluded=snap_excluded;}
                } else {
                    if !press_modifiers.ctrl && !press_modifiers.command {
                        actions.selection = Some(None);
                    }
                    self.cancel();
                }
            }
        }
        if let (Some(gesture), Some(position)) = (&mut self.gesture, position) {
            if matches!(gesture.mode,Mode::Move|Mode::GroupResize(_)|Mode::GroupRotate) && to_view(gesture.start).distance(position)>3.0 {gesture.moved=true;}
        }
        if let (Some(gesture), Some(position)) = (&self.gesture, position) {
            let current = to_canvas(position);
            let delta = [current[0] - gesture.start[0], current[1] - gesture.start[1]];
            let next = match gesture.mode {
                Mode::Move if gesture.moved && (!gesture.group.is_empty()||gesture.snap_excluded.len()>1) => Some(gesture.target.moved_group_snapped(session,capture,&gesture.group,&gesture.snap_excluded,delta,[f64::from(canvas.width())/size[0],f64::from(canvas.height())/size[1]],modifiers.ctrl)),
                Mode::Move if gesture.moved && gesture.target.container!=scene => Some(gesture.target.moved(delta).map(|t|(t,[None;2]))),
                Mode::Move if gesture.moved => Some(gesture.target.moved_snapped(session,capture,delta,[f64::from(canvas.width())/size[0],f64::from(canvas.height())/size[1]],modifiers.ctrl).map(|t|(t,[None;2]))),
                Mode::Move => None,
                Mode::Rotate => Some(gesture.target.rotated(gesture.start,current,modifiers.shift,modifiers.ctrl).map(|t|(t,[None;2]))),
                Mode::Resize(handle) => Some(gesture.target.resized_snapped(
                    session,capture,handle,
                    delta,
                    modifiers.shift,
                    gesture.crop,
                    [f64::from(canvas.width())/size[0],f64::from(canvas.height())/size[1]],modifiers.ctrl,
                ).map(|t|(t,[None;2]))),
                Mode::GroupResize(_) | Mode::GroupRotate | Mode::Keyboard => None,
            };
            if let Some(next) = next {
                match next {
                    Ok((transform,guides)) => {if let Some(gesture)=&mut self.gesture{gesture.snap_guides=guides;}self.set_draft(session, transform)},
                    Err(error) => self.error = Some(error.to_string()),
                };
            }
        }
        if let (Some(gesture),Some(position))=(&self.gesture,position) {
            if let (Mode::GroupResize(handle),Some(box_geometry))=(gesture.mode,gesture.group_geometry) {
                let unit=handle.unit();let anchor=point(box_geometry,[1.0-unit[0],1.0-unit[1]]);
                let start=[gesture.start[0]-anchor[0],gesture.start[1]-anchor[1]];
                let current=to_canvas(position);let now=[current[0]-anchor[0],current[1]-anchor[1]];
                let factor=if unit[0]==0.5 {now[1]/start[1]} else if unit[1]==0.5 {now[0]/start[0]} else {
                    (now[0]*start[0]+now[1]*start[1])/(start[0]*start[0]+start[1]*start[1])
                };
                if gesture.moved && factor.is_finite()&&factor>0.0001 {
                    match gesture.target.group_scale_snapped(session,capture,box_geometry,handle,&gesture.snap_excluded,factor,[f64::from(canvas.width())/size[0],f64::from(canvas.height())/size[1]],modifiers.ctrl) {
                        Ok((factor,guides))=>self.set_group_scale(session,anchor,factor,guides),
                        Err(error)=>self.error=Some(error.to_string()),
                    }
                }
            }
        }
        if let (Some(gesture),Some(position))=(&self.gesture,position) {
            if let (Mode::GroupRotate,Some(box_geometry))=(gesture.mode,gesture.group_geometry) {
                let anchor=point(box_geometry,[0.5,0.5]);let current=to_canvas(position);
                let start=[gesture.start[0]-anchor[0],gesture.start[1]-anchor[1]];
                let now=[current[0]-anchor[0],current[1]-anchor[1]];
                if gesture.moved && start[0].hypot(start[1])>1e-9 && now[0].hypot(now[1])>1e-9 {
                    let mut degrees=(now[1].atan2(now[0])-start[1].atan2(start[0])).to_degrees();
                    if modifiers.shift {degrees=(degrees/15.0).round()*15.0;}
                    else if !modifiers.ctrl {let snapped=(degrees/45.0).round()*45.0;if (degrees-snapped).abs()<5.0 {degrees=snapped;}}
                    self.set_group_rotation(session,anchor,degrees);
                }
            }
        }
        if released
            && self
                .gesture
                .as_ref()
                .is_some_and(|gesture| !matches!(gesture.mode, Mode::Keyboard))
        {
            if let Some(gesture) = self.gesture.as_ref().filter(|gesture| {
                matches!(gesture.mode, Mode::Move) && !gesture.moved && gesture.draft == gesture.target.transform
                    && gesture.target.validate(session, capture).is_ok()
            }) {
                if let Some(previous) = gesture.toggle_selection.clone() {
                    let focus = crate::views::group_source::toggle_preview_selection(
                        ui.ctx(), &model, scene, session.identity(), gesture.target.item, previous,
                    );
                    actions.selection = Some(focus);
                }
            }
            self.finish(session, capture, auto_text, &mut actions);
        }
        if response.has_focus() {
            let keys = [
                (egui::Key::ArrowLeft, [-1.0, 0.0]),
                (egui::Key::ArrowRight, [1.0, 0.0]),
                (egui::Key::ArrowUp, [0.0, -1.0]),
                (egui::Key::ArrowDown, [0.0, 1.0]),
            ];
            let mut delta = [0.0, 0.0];
            for (key, direction) in keys {
                if ui.input_mut(|input| {
                    input.consume_key(egui::Modifiers::NONE, key)
                        || input.consume_key(egui::Modifiers::SHIFT, key)
                }) {
                    let step = if modifiers.shift { 10.0 } else { 1.0 };
                    delta[0] += direction[0] * step;
                    delta[1] += direction[1] * step;
                }
            }
            if delta != [0.0, 0.0] {
                if self.gesture.is_none() {
                    if let Some(target) = target.as_ref() {
                        self.start(target.clone(), [0.0, 0.0], Mode::Keyboard, false);
                        let selected_items=crate::views::group_source::selected_items_context(ui.ctx(),&model,scene,session.identity(),selected);
                        if let Some(gesture)=&mut self.gesture {
                            gesture.group=selected_items.into_iter().filter(|item|*item!=target.item)
                                .filter_map(|item|read_target(item).ok()).collect();
                        }
                    }
                }
                if let Some(gesture) = &mut self.gesture {
                    if matches!(gesture.mode, Mode::Keyboard) {
                        gesture.keyboard_delta[0]+=delta[0];gesture.keyboard_delta[1]+=delta[1];
                        if let Ok(transform)=gesture.target.moved(gesture.keyboard_delta) {
                            self.set_draft(session, transform);
                        }
                    }
                }
            }
            if self
                .gesture
                .as_ref()
                .is_some_and(|gesture| matches!(gesture.mode, Mode::Keyboard))
                && !ui.input(|input| keys.iter().any(|(key, _)| input.key_down(*key)))
            {
                self.finish(session, capture, auto_text, &mut actions);
            }
        } else if self
            .gesture
            .as_ref()
            .is_some_and(|gesture| matches!(gesture.mode, Mode::Keyboard))
        {
            self.cancel();
        }
        if self.gesture.is_none() {
            if let Some(position)=position.filter(|position| viewport.contains(*position)&&ui.ctx().layer_id_at(*position)==Some(response.layer_id)) {
                if let Some(hover)=if let Some(view)=auto_text{pick_for_view(session,capture,scene,to_canvas(position),view)}else{pick(session,capture,scene,to_canvas(position))} {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    if Some(hover.item)!=selected{
                        let painter=ui.painter().with_clip_rect(viewport);
                        let corners=[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0]].map(|unit|to_view(point(hover.geometry,unit)));
                        let color=crate::theme::interface_color(ui,skylive_core::InterfaceColor::Hover);
                        for edge in 0..4 {painter.line_segment([corners[edge],corners[(edge+1)%4]],egui::Stroke::new(1.0,color));}
                    }
                }
            }
        }
        // Other selected sources remain visibly selected on the canvas even
        // though transform handles belong to the focused source only.
        let selected_items = crate::views::group_source::selected_items_context(
            ui.ctx(), &model, scene, session.identity(), selected,
        );
        let painter = ui.painter().with_clip_rect(viewport);
        if let Some(gesture)=&self.gesture {
            let color=crate::theme::interface_color(ui,skylive_core::InterfaceColor::Selection);
            if let Some(x)=gesture.snap_guides[0] {painter.line_segment([to_view([x,0.0]),to_view([x,size[1]])],egui::Stroke::new(1.0,color));}
            if let Some(y)=gesture.snap_guides[1] {painter.line_segment([to_view([0.0,y]),to_view([size[0],y])],egui::Stroke::new(1.0,color));}
        }
        for item in selected_items.into_iter().filter(|item| Some(*item) != selected) {
            if let Ok(target) = read_target(item) {
                let geometry=self.gesture.as_ref().and_then(|gesture|gesture.group_draft.iter().find(|(id,_)|*id==item))
                    .and_then(|(_,transform)|target.geometry_for(transform).ok()).unwrap_or(target.geometry);
                let corners = [[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0]]
                    .map(|unit| to_view(point(geometry, unit)));
                let color = crate::theme::interface_color(ui, skylive_core::InterfaceColor::Selection);
                for edge in 0..4 {
                    painter.line_segment([corners[edge], corners[(edge+1)%4]], egui::Stroke::new(1.5, color));
                }
            }
        }
        let visual = if group_box.is_some() {
            selection_geometry(&group_targets,self.gesture.as_ref().map_or(&[][..],|gesture|gesture.group_draft.as_slice()))
        } else { self
            .gesture
            .as_ref()
            .and_then(|gesture| gesture.target.geometry_for(&gesture.draft).ok())
            .or_else(|| {
                selected.and_then(|item| {
                    read_target(item)
                        .ok()
                        .map(|target| target.geometry)
                })
            }) };
        if let Some(geometry) = visual {
            let painter = ui
                .painter()
                .with_clip_rect(viewport);
            let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
                .map(|p| to_view(point(geometry, p)));
            let color = if self.gesture.as_ref().is_some_and(|gesture| gesture.crop) {
                crate::theme::interface_color(ui,skylive_core::InterfaceColor::Crop)
            } else {
                crate::theme::interface_color(ui,skylive_core::InterfaceColor::Selection)
            };
            for index in 0..4 {
                painter.line_segment(
                    [corners[index], corners[(index + 1) % 4]],
                    egui::Stroke::new(1.5, color),
                );
            }
            if group_box.is_some() || target.as_ref().is_some_and(|target|target.container==scene) {
            let hovered_handle=if self.gesture.is_none() {
                position.filter(|position|viewport.contains(*position)
                    &&ui.ctx().layer_id_at(*position)==Some(response.layer_id))
                    .and_then(|position|nearest_handle(geometry,position,&to_view))
            }else{None};
            for handle in Handle::ALL {
                let handle_position = to_view(point(geometry, handle.unit()));
                painter.rect_filled(
                    egui::Rect::from_center_size(handle_position, egui::vec2(7.0, 7.0)),
                    0.0,
                    color,
                );
                let handle_response=ui.interact(egui::Rect::from_center_size(handle_position,egui::vec2(14.0,14.0)).intersect(viewport),
                    egui::Id::new(("preview-resize-handle",session.identity(),selected,handle)),egui::Sense::hover());
                // AccessKit builds widget information while holding egui's
                // context write lock. Translate before entering the callback.
                let handle_name=crate::i18n::tr(ui,handle_label(handle));
                handle_response.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Other,true,handle_name.clone()));
                let hovered=hovered_handle==Some(handle);
                let active=self.gesture.as_ref().is_some_and(|gesture|matches!(gesture.mode,Mode::Resize(value)|Mode::GroupResize(value)if value==handle));
                if active||self.gesture.is_none()&&hovered{ui.ctx().set_cursor_icon(resize_cursor(geometry,handle,&to_view));}
                if hovered{handle_response.on_hover_ui(|ui|{
                    ui.label(crate::i18n::tr(ui,handle_label(handle)));
                    if group_box.is_some() {ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Preview.GroupScaleHelp","Drag to scale uniformly around the opposite handle. Hold Ctrl to disable snapping; Escape cancels."));}
                    else {ui.label(crate::i18n::tr(ui,"Drag to resize. Hold Shift to stretch, Alt to crop, or Ctrl to bypass canvas snapping. Escape cancels."));}
                });}
            }
            if group_box.is_none() || group_rotation_supported {
            let flipped_y=if group_box.is_some(){false}else{self.gesture.as_ref().map(|g|g.draft.flip_y)
                .or_else(||target.as_ref().map(|t|t.transform.flip_y)).unwrap_or(false)};
            let (stem,knob)=rotation_handle(geometry,flipped_y,&to_view);
            painter.line_segment([stem,knob],egui::Stroke::new(1.5,color));
            painter.circle_filled(knob,4.5,color);
            let tip=if group_box.is_some(){crate::i18n::tr_key_or(ui,"SkyLive.Preview.GroupRotateHelp","Drag to rotate the selection around its center. Shift snaps to 15°; Ctrl disables snapping.")}
                else{crate::i18n::tr(ui,"Drag to rotate around the source center. Shift snaps to 15°; Ctrl disables angle snapping.")};
            let knob_response=ui.interact(egui::Rect::from_center_size(knob,egui::vec2(16.0,16.0)).intersect(viewport),
                egui::Id::new(("preview-rotation-handle",session.identity(),selected)),egui::Sense::hover())
                .on_hover_text(tip);
            if self.gesture.as_ref().is_some_and(|g|matches!(g.mode,Mode::Rotate|Mode::GroupRotate)) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if knob_response.hovered() {ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);}
            }
            }
        }
        if self.gesture.as_ref().is_some_and(|gesture|matches!(gesture.mode,Mode::Move)){ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);}
        let after = self
            .gesture
            .as_ref()
            .and_then(|gesture| gesture.output.as_ref())
            .map(FrameComposition::identity);
        if before != after {
            ui.ctx().request_repaint();
        }
        if let Some(error)=selected_problem.as_ref().or(self.error.as_ref()) {
            ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error)).on_hover_text(crate::i18n::tr(ui,error));
        }
        actions
    }
}

fn handle_label(handle:Handle)->&'static str{match handle{
    Handle::TopLeft=>"Resize top-left corner",Handle::Top=>"Resize top edge",Handle::TopRight=>"Resize top-right corner",Handle::Right=>"Resize right edge",
    Handle::BottomRight=>"Resize bottom-right corner",Handle::Bottom=>"Resize bottom edge",Handle::BottomLeft=>"Resize bottom-left corner",Handle::Left=>"Resize left edge",
}}
fn selection_geometry(targets:&[Target],draft:&[(Id,Transform)])->Option<skylive_core::ItemGeometry> {
    let mut bounds=None::<skylive_core::ItemBounds>;
    for target in targets {
        let geometry=draft.iter().find(|(id,_)|*id==target.item)
            .and_then(|(_,transform)|target.geometry_for(transform).ok()).unwrap_or(target.geometry);
        let item=geometry.bounds();
        bounds=Some(match bounds {None=>item,Some(old)=>skylive_core::ItemBounds{
            left:old.left.min(item.left),top:old.top.min(item.top),right:old.right.max(item.right),bottom:old.bottom.max(item.bottom),
        }});
    }
    let b=bounds?;let width=b.right-b.left;let height=b.bottom-b.top;
    if !width.is_finite()||!height.is_finite()||width<=0.0||height<=0.0{return None;}
    Some(skylive_core::ItemGeometry{origin:[b.left,b.top],axis_x:[width,0.0],axis_y:[0.0,height],uv:[0.0,0.0,1.0,1.0]})
}

fn nearest_handle(geometry:skylive_core::ItemGeometry,position:egui::Pos2,to_view:&impl Fn([f64;2])->egui::Pos2)->Option<Handle>{
    // Several hit areas overlap at small preview scales. OBS chooses the
    // nearest marker; use the same choice for the cursor, tooltip and drag.
    Handle::ALL.into_iter().filter_map(|handle|{
        let distance=to_view(point(geometry,handle.unit())).distance(position);
        (distance<=7.0).then_some((handle,distance))
    }).min_by(|a,b|a.1.total_cmp(&b.1)).map(|(handle,_)|handle)
}
fn resize_cursor(geometry:skylive_core::ItemGeometry,handle:Handle,to_view:&impl Fn([f64;2])->egui::Pos2)->egui::CursorIcon{
    let origin=to_view(point(geometry,[0.0,0.0]));let x=to_view(point(geometry,[1.0,0.0]))-origin;let y=to_view(point(geometry,[0.0,1.0]))-origin;
    let normalize=|axis:egui::Vec2|if axis.length()>0.0{axis/axis.length()}else{egui::Vec2::ZERO};
    let unit=handle.unit();let direction=normalize(x)*(unit[0]as f32-0.5)+normalize(y)*(unit[1]as f32-0.5);
    let angle=direction.y.atan2(direction.x).to_degrees().rem_euclid(180.0);
    if !(22.5..157.5).contains(&angle){egui::CursorIcon::ResizeHorizontal}else if angle<67.5{egui::CursorIcon::ResizeNwSe}else if angle<112.5{egui::CursorIcon::ResizeVertical}else{egui::CursorIcon::ResizeNeSw}
}
fn rotation_handle(geometry:skylive_core::ItemGeometry,flipped_y:bool,to_view:&impl Fn([f64;2])->egui::Pos2)->(egui::Pos2,egui::Pos2) {
    let center=to_view(point(geometry,[0.5,0.5]));
    let edge=to_view(point(geometry,[0.5,if flipped_y {1.0}else{0.0}]));
    let direction=edge-center;
    let distance=direction.length();
    let outward=if distance>0.0 {direction/distance}else{egui::vec2(0.0,-1.0)};
    (edge,edge+outward*23.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Session, Id, Id, CaptureSnapshot) {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetVideo {
                width: 200,
                height: 100,
                fps_num: 30,
                fps_den: 1,
            })
            .unwrap();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap();
        let item = session.snapshot().scene(scene).unwrap().items()[0].id();
        let mut transform = Transform::full(100, 50);
        transform.x = 20.0;
        transform.y = 10.0;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let capture = CaptureSnapshot::empty(session.identity());
        (session, scene, item, capture)
    }
    fn at(
        owner: &mut PreviewInteraction,
        context: &egui::Context,
        session: &Session,
        capture: &CaptureSnapshot,
        item: Id,
        events: Vec<egui::Event>,
    ) -> Actions {
        let mut actions = Actions::default();
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                egui::Event::PointerButton { modifiers, .. }
                | egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                events,
                modifiers,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    actions = owner.show(
                        ui,
                        View {
                            session,
                            capture,
                            auto_text:None,
                            scene: session.preview(),
                            selected: Some(item),
                            canvas: egui::Rect::from_min_size(
                                egui::pos2(20.0, 20.0),
                                egui::vec2(400.0, 200.0),
                            ),
                            enabled: true,
                            menu_enabled: true,
                        },
                    );
                });
            },
        );
        actions
    }
    fn pointer(position: [f32; 2], pressed: bool, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        let pos = egui::pos2(position[0], position[1]);
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            },
        ]
    }
    fn key(key: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
        }
    }
    #[test]
    fn real_pointer_drag_is_dpi_independent_staged_and_one_undoable_commit() {
        for scale in [1.0, 2.0] {
            let (mut session, scene, item, capture) = fixture();
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut owner = PreviewInteraction::default();
            at(&mut owner, &ctx, &session, &capture, item, vec![]);
            at(&mut owner, &ctx, &session, &capture, item, vec![]);
            let before = session.snapshot();
            let start = at(
                &mut owner,
                &ctx,
                &session,
                &capture,
                item,
                pointer([120.0, 80.0], true, egui::Modifiers::NONE),
            );
            assert_eq!(start.selection, Some(Some(item)));
            at(
                &mut owner,
                &ctx,
                &session,
                &capture,
                item,
                vec![egui::Event::PointerMoved(egui::pos2(180.0, 120.0))],
            );
            assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
            assert_eq!(owner.gesture.as_ref().unwrap().draft.x, 50.0);
            assert!(
                owner
                    .draft_output(&session, &capture, scene, Some(item))
                    .is_some()
            );
            let commit = at(
                &mut owner,
                &ctx,
                &session,
                &capture,
                item,
                pointer([180.0, 120.0], false, egui::Modifiers::NONE),
            )
            .commit
            .unwrap();
            session
                .apply_at(Some(commit.revision), commit.command)
                .unwrap();
            assert_eq!(session.snapshot().revision(), before.revision() + 1);
            assert_eq!(
                session.snapshot().scene(scene).unwrap().items()[0]
                    .transform()
                    .y,
                30.0
            );
            session.apply(Command::Undo).unwrap();
            assert_eq!(
                session.snapshot().scene(scene).unwrap().items()[0].transform(),
                before.scene(scene).unwrap().items()[0].transform()
            );
        }
    }
    #[test]
    fn handles_resize_alt_crops_and_escape_or_stale_revision_discards_drafts() {
        let (mut session, scene, item, capture) = fixture();
        let ctx = egui::Context::default();
        let mut owner = PreviewInteraction::default();
        at(&mut owner, &ctx, &session, &capture, item, vec![]);
        at(&mut owner, &ctx, &session, &capture, item, vec![]);
        let before = session.snapshot();
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([260.0, 140.0], true, egui::Modifiers::NONE),
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![egui::Event::PointerMoved(egui::pos2(300.0, 160.0))],
        );
        let draft = &owner.gesture.as_ref().unwrap().draft;
        assert_eq!((draft.width, draft.height), (120.0, 60.0));
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::Escape, true, false)],
        );
        assert!(owner.gesture.is_none());
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([300.0, 160.0], false, egui::Modifiers::NONE),
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([60.0, 40.0], true, egui::Modifiers::ALT),
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![egui::Event::PointerMoved(egui::pos2(80.0, 50.0))],
        );
        let commit = at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([80.0, 50.0], false, egui::Modifiers::ALT),
        )
        .commit
        .unwrap();
        session
            .apply_at(Some(commit.revision), commit.command)
            .unwrap();
        let transformed = session.snapshot();
        let transform = transformed.scene(scene).unwrap().items()[0].transform();
        assert_eq!((transform.crop.left, transform.crop.top), (20, 10));
        assert_eq!((transform.x, transform.y), (30.0, 15.0));
        session.apply(Command::Undo).unwrap();
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([120.0, 80.0], true, egui::Modifiers::NONE),
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![egui::Event::PointerMoved(egui::pos2(140.0, 90.0))],
        );
        session
            .apply(Command::RenameScene {
                scene,
                name: "Changed".into(),
            })
            .unwrap();
        assert!(
            owner
                .draft_output(&session, &capture, scene, Some(item))
                .is_none()
        );
        assert!(owner.gesture.is_none());
        assert!(owner.error.is_some());
        assert!(
            at(
                &mut owner,
                &ctx,
                &session,
                &capture,
                item,
                pointer([140.0, 90.0], false, egui::Modifiers::NONE)
            )
            .commit
            .is_none()
        );
    }
    #[test]
    fn held_keyboard_nudges_commit_on_release_as_one_edit_and_escape_cancels() {
        let (mut session, scene, item, capture) = fixture();
        let ctx = egui::Context::default();
        let mut owner = PreviewInteraction::default();
        at(&mut owner, &ctx, &session, &capture, item, vec![]);
        at(&mut owner, &ctx, &session, &capture, item, vec![]);
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([120.0, 80.0], true, egui::Modifiers::NONE),
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([120.0, 80.0], false, egui::Modifiers::NONE),
        );
        let before = session.snapshot();
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::ArrowRight, true, false)],
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::ArrowRight, true, true)],
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let commit = at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::ArrowRight, false, false)],
        )
        .commit
        .unwrap();
        session
            .apply_at(Some(commit.revision), commit.command)
            .unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0]
                .transform()
                .x,
            22.0
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0]
                .transform()
                .x,
            20.0
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::ArrowDown, true, false)],
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::Escape, true, false)],
        );
        assert!(owner.gesture.is_none());
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0]
                .transform()
                .y,
            10.0
        );
    }
    #[test]
    fn actual_draft_texture_changes_while_program_and_canonical_pixels_stay_unchanged_then_cancel_recovers()
     {
        let (session, scene, item, capture) = fixture();
        let ctx = egui::Context::default();
        let mut owner = PreviewInteraction::default();
        let capture = std::sync::Arc::new(capture);
        let sample = skylive_host::playback::PlaybackSnapshot::at_time(
            &session,
            skylive_media::MediaTime(0),
        );
        let mut preview = crate::preview::PreviewPane::new("draft-pixels").unwrap();
        let mut program = crate::preview::PreviewPane::new("unchanged-program").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while preview.displayed_pixel().is_none() || program.displayed_pixel().is_none() {
            preview.update_playback_with_capture(&ctx, &session, scene, &sample, &capture);
            program.update_playback_with_capture(&ctx, &session, scene, &sample, &capture);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(preview.displayed_pixel().unwrap().0, [0, 0, 0, 255]);
        let before = session.snapshot();
        at(&mut owner, &ctx, &session, &capture, item, vec![]);
        at(&mut owner, &ctx, &session, &capture, item, vec![]);
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            pointer([120.0, 80.0], true, egui::Modifiers::NONE),
        );
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![egui::Event::PointerMoved(egui::pos2(40.0, 40.0))],
        );
        let (epoch, output) = owner
            .draft_output(&session, &capture, scene, Some(item))
            .unwrap();
        while preview
            .displayed_pixel()
            .is_none_or(|pixel| pixel.0 != [255, 0, 0, 255])
        {
            preview.update_draft_with_capture(&ctx, &session, &sample, &output, &capture, epoch);
            program.update_playback_with_capture(&ctx, &session, scene, &sample, &capture);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(program.displayed_pixel().unwrap().0, [0, 0, 0, 255]);
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        at(
            &mut owner,
            &ctx,
            &session,
            &capture,
            item,
            vec![key(egui::Key::Escape, true, false)],
        );
        assert!(owner.gesture.is_none());
        loop {
            preview.update_playback_with_capture(&ctx, &session, scene, &sample, &capture);
            if preview
                .displayed_pixel()
                .is_some_and(|pixel| pixel.0 == [0, 0, 0, 255])
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        // Cancel before consuming an admitted draft sample as well: even a
        // completed stale result must not repaint the canonical view.
        preview.update_draft_with_capture(&ctx, &session, &sample, &output, &capture, epoch + 1);
        loop {
            preview.update_playback_with_capture(&ctx, &session, scene, &sample, &capture);
            assert!(
                preview
                    .displayed_pixel()
                    .is_none_or(|pixel| pixel.0 == [0, 0, 0, 255])
            );
            if preview.displayed_pixel().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
    }
    #[test]
    fn coalesced_press_move_release_uses_actual_anchor_even_when_released_outside_canvas() {
        for endpoint in [[180.0, 120.0], [470.0, 250.0]] {
            let (mut session, scene, item, capture) = fixture();
            let context = egui::Context::default();
            let mut owner = PreviewInteraction::default();
            at(&mut owner, &context, &session, &capture, item, vec![]);
            at(&mut owner, &context, &session, &capture, item, vec![]);
            let before = session.snapshot();
            let mut events = pointer([120.0, 80.0], true, egui::Modifiers::NONE);
            events.extend(pointer(endpoint, false, egui::Modifiers::NONE));
            let actions = at(&mut owner, &context, &session, &capture, item, events);
            assert_eq!(actions.selection, Some(Some(item)));
            assert!(
                std::sync::Arc::ptr_eq(&before, &session.snapshot()),
                "input stages until the shared command is admitted"
            );
            let commit = actions
                .commit
                .expect("complete drag in one frame still commits");
            session
                .apply_at(Some(commit.revision), commit.command)
                .unwrap();
            let model = session.snapshot();
            let transform = model.scene(scene).unwrap().items()[0].transform();
            assert_eq!(transform.x, 20.0 + (endpoint[0] - 120.0) / 2.0);
            assert_eq!(transform.y, 10.0 + (endpoint[1] - 80.0) / 2.0);
            assert_eq!(model.revision(), before.revision() + 1);
            session.apply(Command::Undo).unwrap();
            assert_eq!(session.snapshot().scene(scene), before.scene(scene));
            assert!(owner.gesture.is_none());
        }
    }
    #[test]
    fn coalesced_drag_cannot_start_outside_canvas_and_finish_inside_it() {
        let (session, _, item, capture) = fixture();
        let context = egui::Context::default();
        let mut owner = PreviewInteraction::default();
        at(&mut owner, &context, &session, &capture, item, vec![]);
        at(&mut owner, &context, &session, &capture, item, vec![]);
        let mut events = pointer([470.0, 250.0], true, egui::Modifiers::NONE);
        events.extend(pointer([120.0, 80.0], false, egui::Modifiers::NONE));
        let actions = at(&mut owner, &context, &session, &capture, item, events);
        assert!(actions.selection.is_none());
        assert!(actions.commit.is_none());
        assert!(owner.gesture.is_none());
    }
}
