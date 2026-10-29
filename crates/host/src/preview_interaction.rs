//! Metadata-only Preview gestures. Canvas/native coordinate conversion and
//! candidate validation are shared host operations; canonical publication is
//! always one expected-revision core command on the application owner.
use crate::capture::CaptureSnapshot;
use skylive_core::{Command, Id, ItemGeometry, Session, SourceKind, Transform};

#[derive(Clone, Copy, Debug, PartialEq, Eq,Hash,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}
impl Handle {
    pub const ALL: [Self; 8] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Right,
        Self::BottomRight,
        Self::Bottom,
        Self::BottomLeft,
        Self::Left,
    ];
    pub fn unit(self) -> [f64; 2] {
        match self {
            Self::TopLeft => [0.0, 0.0],
            Self::Top => [0.5, 0.0],
            Self::TopRight => [1.0, 0.0],
            Self::Right => [1.0, 0.5],
            Self::BottomRight => [1.0, 1.0],
            Self::Bottom => [0.5, 1.0],
            Self::BottomLeft => [0.0, 1.0],
            Self::Left => [0.0, 0.5],
        }
    }
    fn left(self) -> bool {
        self.unit()[0] == 0.0
    }
    fn right(self) -> bool {
        self.unit()[0] == 1.0
    }
    fn top(self) -> bool {
        self.unit()[1] == 0.0
    }
    fn bottom(self) -> bool {
        self.unit()[1] == 1.0
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub session: u64,
    pub revision: u64,
    pub scene: Id,
    pub container: Id,
    pub item: Id,
    pub transform: Transform,
    pub geometry: ItemGeometry,
    source: Id,
    definition:skylive_core::SourceDefinitionEpoch,
    basis: (u32, u32),
    actual: (u32, u32),
    authored_frame:(u32,u32),
    root_mapping:skylive_core::VideoFilterMapping,
    root_canvas:(u32,u32),
    root_extent:(u32,u32),
    capture_epoch: Option<u64>,
    // Retains both the exact auto-text raster's credit and revision guard for
    // the entire gesture. It is None for ordinary authored source geometry.
    auto_text:Vec<crate::text::auto_worker::Snapshot>,
    /// Local group coordinates to parent coordinates, innermost first.
    ancestors: Vec<([f64;2],[f64;2],[f64;2])>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionError(pub &'static str);
impl std::fmt::Display for InteractionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InteractionError {}
fn invalid(_: skylive_core::Error) -> InteractionError {
    InteractionError("Transform is outside the valid source geometry.")
}

impl Target {
    pub fn auto_text_view(&self)->crate::preview::AutoTextView{crate::preview::AutoTextView{snapshots:self.auto_text.clone()}}
    /// Build interaction preview from the same auto-text generation used for
    /// the displayed hit-test; authored session state remains untouched.
    pub fn draft_view(&self,session:&Session,transforms:&[(Id,Transform)])->Result<crate::transition::FrameScene,crate::transition::TransitionError>{
        if self.auto_text.is_empty(){
            if transforms.len()==1&&self.container==self.scene {
                return crate::transition::FrameScene::preview_transform(session.identity(),&session.snapshot(),self.scene,transforms[0].0,transforms[0].1.clone());
            }
            return crate::transition::FrameScene::preview_transforms(session.identity(),&session.snapshot(),self.scene,self.container,transforms);
        }
        let authored=session.snapshot();
        let projected=crate::text::auto_worker::project_all(session.identity(),&authored,&self.auto_text)?;
        let view=crate::transition::FrameScene::preview_transforms_with_auto(session.identity(),&projected,self.scene,self.container,transforms,&self.auto_text)?;
        if !self.auto_text.iter().all(|snapshot|snapshot.matches_model(session.identity(),&authored)){
            return Err(crate::transition::TransitionError::Engine(skylive_core::Error::Unavailable("Automatic text changed during preview gesture")));
        }
        Ok(view)
    }
    /// Hit-test the same clipped affine box used by preview picking.
    pub fn contains(&self, canvas:[f64;2])->bool {
        if canvas[0]<0.0 || canvas[1]<0.0 || canvas[0]>=f64::from(self.root_extent.0) || canvas[1]>=f64::from(self.root_extent.1){return false;}
        inverse_vector(self.geometry,[canvas[0]-self.geometry.origin[0],canvas[1]-self.geometry.origin[1]])
            .is_ok_and(|uv|(0.0..=1.0).contains(&uv[0])&&(0.0..=1.0).contains(&uv[1]))
    }
    pub fn read(
        session: &Session,
        capture: &CaptureSnapshot,
        scene: Id,
        item: Id,
    ) -> Result<Self, InteractionError> {
        let authored=session.snapshot();
        let has_auto=authored.sources().iter().any(|source|matches!(source.kind(),SourceKind::Text{settings} if settings.auto_extents()));
        let frames=if has_auto {crate::text::auto_worker::shared_snapshots(session.identity(),&authored)
            .map_err(|_|InteractionError("Automatic text geometry is being prepared."))?}else{Vec::new()};
        Self::read_impl(session,capture,scene,item,false,false,&frames,None)
    }
    /// Internal entry point for a future activated auto-text source. The
    /// projection is recomputed from the authored snapshot on every read;
    /// stale text or source identity cancels the gesture.
    #[allow(dead_code)]
    pub(crate) fn read_auto(session:&Session,capture:&CaptureSnapshot,scene:Id,item:Id,
        auto:&crate::text::auto_worker::Snapshot)->Result<Self,InteractionError>{
        Self::read_impl(session,capture,scene,item,false,false,std::slice::from_ref(auto),None)
    }
    /// Hit-test against the exact auto-text generation used to draw the
    /// displayed preview. A revoked or mismatched view is never substituted by
    /// a newer generation during a gesture.
    pub fn read_for_view(session:&Session,capture:&CaptureSnapshot,scene:Id,item:Id,
        view:&crate::preview::AutoTextView)->Result<Self,InteractionError>{
        if !view.matches(session.identity(),&session.snapshot()){
            return Err(InteractionError("The displayed automatic text geometry is stale."));
        }
        Self::read_impl(session,capture,scene,item,false,false,&view.snapshots,None)
    }
    fn read_impl(
        session: &Session,
        capture: &CaptureSnapshot,
        scene: Id,
        item: Id,
        allow_locked: bool,
        include_hidden:bool,
        auto:&[crate::text::auto_worker::Snapshot],
        model_override:Option<&skylive_core::StudioSnapshot>,
    ) -> Result<Self, InteractionError> {
        if session.is_closed() {
            return Err(InteractionError("The scene collection is closed."));
        }
        let authored = session.snapshot();
        let mut projection=if auto.is_empty()||model_override.is_some(){None}else{Some(authored.as_ref().clone())};
        if let Some(model)=&mut projection {
            *model=crate::text::auto_worker::project_all(session.identity(),model,auto)
                .map_err(|_|InteractionError("Automatic text geometry changed; the gesture was cancelled."))?;
        }
        let model=model_override.or(projection.as_ref()).unwrap_or(authored.as_ref());
        let (container,row) = model
            .item_container(item)
            .ok_or(InteractionError("The selected item no longer exists."))?;
        if model.root_scene_for_container(container)!=Some(scene) || model.container_is_locked(container) {
            return Err(InteractionError("The selected item is outside the scene or its group is locked."));
        }
        if (!row.visible()&&!include_hidden) || (row.locked()&&!allow_locked) {
            return Err(InteractionError(
                "Hidden or locked items cannot be edited in Preview.",
            ));
        }
        let source = model
            .source(row.source())
            .ok_or(InteractionError("The selected source no longer exists."))?;
        let basis = model
            .source_video_dimensions(source.kind())
            .map_err(invalid)?
            .ok_or(InteractionError("This source has no video geometry."))?;
        let (actual, capture_epoch) = if matches!(
            source.kind(),
            SourceKind::DisplayCapture { .. }
                | SourceKind::WindowCapture { .. }
                | SourceKind::VideoCapture { .. }
                | SourceKind::MediaSource { .. } | SourceKind::NdiSource {..} | SourceKind::GameCapture {..} | SourceKind::Browser {..}
        ) {
            if capture.session() != session.identity() {
                return Err(InteractionError(
                    "Capture belongs to another scene collection.",
                ));
            }
            (
                capture
                    .frame(source.id())
                    .filter(|frame| frame.binding.matches_source(source.kind()))
                    .map(|frame| (frame.image.width(), frame.image.height()))
                    .unwrap_or(basis),
                Some(capture.epoch()),
            )
        } else {
            (basis, None)
        };
        let root_raw=(model.video().width(),model.video().height());
        let authored_frame=model.effective_filtered_video_dimensions(source,basis,Some(root_raw)).map_err(invalid)?;
        let actual=model.effective_filtered_video_dimensions(source,actual,Some(root_raw)).map_err(invalid)?;
        let transform = row.transform().clone();
        let root_mapping=model.scene_source(scene).map_or(Ok(skylive_core::VideoFilterMapping::identity(root_raw)),|source|model.effective_video_filter_mapping(source,root_raw,Some(root_raw))).map_err(invalid)?;
        if !root_mapping.is_affine(){return Err(InteractionError("Affine Preview editing is unavailable while the scene uses nonlinear scaling or animated Scroll. Edit individual source properties or disable that scene filter."));}
        let root_extent=root_mapping.dimensions();
        let mut ancestors=Vec::new();let mut owner=container;
        while owner!=scene {
            if ancestors.len()>=skylive_core::MAX_SCENE_DEPTH {return Err(InteractionError("Group nesting limit exceeded."));}
            let (parent,group_item)=model.group_parent(owner).ok_or(InteractionError("Group parent is unavailable."))?;
            let (_,wrapper)=model.item_container(group_item).ok_or(InteractionError("Group wrapper is unavailable."))?;
            if (!wrapper.visible()&&!include_hidden)||(wrapper.locked()&&!allow_locked){return Err(InteractionError("Hidden or locked group cannot be edited."));}
            let group_source=model.source(wrapper.source()).ok_or(InteractionError("Group source is unavailable."))?;
            let SourceKind::Group{group}=group_source.kind() else{return Err(InteractionError("Invalid group source."));};
            if group_source.video_filters().iter().any(|filter|filter.enabled){return Err(InteractionError("Disable group filters before editing nested items in Preview."));}
            let (w,h)=group.basis();
            let geo=wrapper.transform().geometry_for_frame(w,h,w,h).map_err(invalid)?;
            let uv=geo.uv;let ux=uv[2]-uv[0];let uy=uv[3]-uv[1];
            if ux<=0.0||uy<=0.0{return Err(InteractionError("Group crop has no editable area."));}
            if uv != [0.0,0.0,1.0,1.0] {return Err(InteractionError("Nested Preview editing is unavailable while an ancestor group is cropped."));}
            let x=[geo.axis_x[0]/(ux*f64::from(w)),geo.axis_x[1]/(ux*f64::from(w))];
            let y=[geo.axis_y[0]/(uy*f64::from(h)),geo.axis_y[1]/(uy*f64::from(h))];
            let origin=[geo.origin[0]-geo.axis_x[0]*uv[0]/ux-geo.axis_y[0]*uv[1]/uy,geo.origin[1]-geo.axis_x[1]*uv[0]/ux-geo.axis_y[1]*uv[1]/uy];
            ancestors.push((origin,x,y));owner=parent;
        }
        let mut geometry=transform.geometry_for_frame(basis.0,basis.1,actual.0,actual.1).map_err(invalid)?;
        for (o,x,y) in &ancestors {
            geometry.origin=[o[0]+x[0]*geometry.origin[0]+y[0]*geometry.origin[1],o[1]+x[1]*geometry.origin[0]+y[1]*geometry.origin[1]];
            geometry.axis_x=[x[0]*geometry.axis_x[0]+y[0]*geometry.axis_x[1],x[1]*geometry.axis_x[0]+y[1]*geometry.axis_x[1]];
            geometry.axis_y=[x[0]*geometry.axis_y[0]+y[0]*geometry.axis_y[1],x[1]*geometry.axis_y[0]+y[1]*geometry.axis_y[1]];
        }
        let geometry=root_mapping.map_geometry(geometry).map_err(invalid)?;
        Ok(Self {
            session: session.identity(),
            revision: model.revision(),
            scene,
            container,
            item,
            transform,
            geometry,
            source: source.id(),
            definition:session.source_definition_epoch(source.id()).ok_or(InteractionError("The source definition identity is unavailable. Reopen the source before editing."))?,
            basis,
            actual,
            authored_frame,
            root_mapping,root_canvas:root_raw,root_extent,
            capture_epoch,
            auto_text:auto.to_vec(),
            ancestors,
        })
    }
    pub fn validate(
        &self,
        session: &Session,
        capture: &CaptureSnapshot,
    ) -> Result<(), InteractionError> {
        if session.identity() != self.session || session.snapshot().revision() != self.revision {
            return Err(InteractionError(
                "Preview changed during the edit; the gesture was cancelled.",
            ));
        }
        let now = Self::read_impl(session, capture, self.scene, self.item,false,false,&self.auto_text,None)?;
        if now.source != self.source || now.container != self.container || now.ancestors != self.ancestors
            || now.definition != self.definition
            || now.transform != self.transform
            || now.actual != self.actual
            || now.capture_epoch != self.capture_epoch
            || now.root_canvas!=self.root_canvas||now.root_mapping!=self.root_mapping||now.authored_frame!=self.authored_frame
        {
            return Err(InteractionError(
                "Source geometry changed during the edit; the gesture was cancelled.",
            ));
        }
        Ok(())
    }
    fn base_geometry(&self,transform:&Transform)->Result<ItemGeometry,InteractionError>{transform.geometry_for_frame(self.basis.0,self.basis.1,self.actual.0,self.actual.1).map_err(invalid)}
    pub fn geometry_for(&self, transform: &Transform) -> Result<ItemGeometry, InteractionError> {
        let mut geometry=self.base_geometry(transform)?;
        for (o,x,y) in &self.ancestors {
            geometry.origin=[o[0]+x[0]*geometry.origin[0]+y[0]*geometry.origin[1],o[1]+x[1]*geometry.origin[0]+y[1]*geometry.origin[1]];
            geometry.axis_x=[x[0]*geometry.axis_x[0]+y[0]*geometry.axis_x[1],x[1]*geometry.axis_x[0]+y[1]*geometry.axis_x[1]];
            geometry.axis_y=[x[0]*geometry.axis_y[0]+y[0]*geometry.axis_y[1],x[1]*geometry.axis_y[0]+y[1]*geometry.axis_y[1]];
        }
        self.root_mapping.map_geometry(geometry).map_err(invalid)
    }
    pub fn command(
        &self,
        session: &Session,
        capture: &CaptureSnapshot,
        transform: Transform,
    ) -> Result<Command, InteractionError> {
        Self::atomic_command(session,capture,&[(self,transform)])
    }
    /// Validate and map the complete selection before constructing one
    /// command. Auto-text projections are round-tripped through the same
    /// group normalization used by the authored command.
    pub fn atomic_command(session:&Session,capture:&CaptureSnapshot,edits:&[(&Target,Transform)])->Result<Command,InteractionError>{
        let Some((first,_))=edits.first() else{return Err(InteractionError("No preview items were edited."));};
        if edits.len()>skylive_core::MAX_SOURCES{return Err(InteractionError("Too many preview items were edited."));}
        let mut seen=std::collections::HashSet::new();let mut mapped=Vec::with_capacity(edits.len());
        for (target,transform) in edits {
            target.validate(session,capture)?;
            if target.scene!=first.scene||target.container!=first.container||!seen.insert(target.item)
                ||target.auto_text.len()!=first.auto_text.len()
                ||!target.auto_text.iter().zip(&first.auto_text).all(|(a,b)|a.same(b)){
                return Err(InteractionError("Preview selection changed during editing."));
            }
            target.geometry_for(transform)?;
            mapped.push((target.item,target.authored_transform(session,capture,transform)?));
        }
        if !first.auto_text.is_empty(){first.validate_auto_mapping(session,capture,edits,&mapped)?;}
        if mapped.len()==1{let (item,transform)=mapped.pop().ok_or(InteractionError("No preview items were edited."))?;
            Ok(Command::SetTransform{scene:first.container,item,transform})
        }else{Ok(Command::SetTransforms{scene:first.container,transforms:mapped})}
    }
    fn validate_auto_mapping(&self,session:&Session,capture:&CaptureSnapshot,edits:&[(&Target,Transform)],mapped:&[(Id,Transform)])->Result<(),InteractionError>{
        let authored=session.snapshot();
        let scene_items=authored.scenes().iter().chain(authored.groups()).filter(|container|authored.root_scene_for_container(container.id())==Some(self.scene))
            .try_fold(0usize,|sum,container|sum.checked_add(container.items().len()))
            .ok_or(InteractionError("Preview geometry verification exceeds its work budget."))?;
        if scene_items>skylive_core::MAX_ITEMS||self.auto_text.len()>64{return Err(InteractionError("This automatic-text scene exceeds the interactive geometry verification budget."));}
        let candidate=authored.snapshot_with_item_transforms_preview(self.container,mapped).map_err(invalid)?;
        let before=crate::text::auto_worker::project_all(session.identity(),&authored,&self.auto_text).map_err(invalid)?;
        let after=crate::text::auto_worker::project_all(session.identity(),&candidate,&self.auto_text).map_err(invalid)?;
        let mut ancestors=std::collections::HashSet::new();
        for (target,_) in edits {
            let mut container=target.container;
            while container!=target.scene {
                let (parent,item)=authored.group_parent(container).ok_or(InteractionError("The group hierarchy changed."))?;
                if !ancestors.insert(item){break;}container=parent;
            }
        }
        let close=|a:f64,b:f64|a.is_finite()&&b.is_finite()&&(a-b).abs()<=0.02+1e-6*a.abs().max(b.abs());
        let equivalent=|a:ItemGeometry,b:ItemGeometry|a.origin.into_iter().zip(b.origin).all(|(a,b)|close(a,b))
            &&a.axis_x.into_iter().zip(b.axis_x).all(|(a,b)|close(a,b))
            &&a.axis_y.into_iter().zip(b.axis_y).all(|(a,b)|close(a,b))
            &&a.uv.into_iter().zip(b.uv).all(|(a,b)|(a-b).abs()<=1e-6);
        // The check is bounded by the document's MAX_ITEMS and depth limits.
        // Include hidden and locked siblings so a later visibility change does
        // not expose a silent geometry jump after group normalization.
        let mut checked_edits=0usize;
        for container in before.scenes().iter().chain(before.groups()) {
            if before.root_scene_for_container(container.id())!=Some(self.scene){continue;}
            for item in container.items(){
                if ancestors.contains(&item.id()){continue;}
                let old=Self::read_impl(session,capture,self.scene,item.id(),true,true,&[],Some(&before));
                let new=Self::read_impl(session,capture,self.scene,item.id(),true,true,&[],Some(&after));
                let desired=edits.iter().find(|(target,_)|target.item==item.id());
                match (old,new,desired){
                    (Ok(old),Ok(new),Some((target,transform)))=>{
                        if !equivalent(old.geometry,target.geometry)||!equivalent(new.geometry,target.geometry_for(transform)?){return Err(InteractionError("The edited group cannot be mapped without changing its geometry."));}
                        checked_edits+=1;
                    },
                    (Ok(old),Ok(new),None)=>if !equivalent(old.geometry,new.geometry){return Err(InteractionError("The edit would move another source after group normalization."));},
                    (Err(_),Err(_),None)=>{
                        let source=before.source(item.source()).ok_or(InteractionError("A scene source disappeared during geometry verification."))?;
                        if before.source_video_dimensions(source.kind()).map_err(invalid)?.is_some(){
                            return Err(InteractionError("A source in this scene cannot be verified safely."));
                        }
                    },
                    _=>return Err(InteractionError("The edit cannot be verified against the current scene geometry.")),
                }
            }
        }
        if checked_edits!=edits.len(){return Err(InteractionError("Not every edited source could be verified."));}
        if self.auto_text.iter().any(|snapshot|!snapshot.is_live()){return Err(InteractionError("Automatic text changed during the edit."));}
        Ok(())
    }
    /// Map a projected edit back through its ancestor coordinate systems. A
    /// transform requiring shear is not representable and is rejected. The
    /// caller must additionally run `validate_auto_mapping` over the complete
    /// atomic command before committing it.
    pub fn authored_transform(&self,session:&Session,capture:&CaptureSnapshot,transform:&Transform)->Result<Transform,InteractionError>{
        if self.auto_text.is_empty(){return Ok(transform.clone());}
        let model=session.snapshot();
        let (container,item)=model.item_container(self.item).ok_or(InteractionError("The edited source no longer exists."))?;
        if container!=self.container || item.source()!=self.source{return Err(InteractionError("The edited source changed."));}
        let original=item.transform();
        let authored=Self::read_impl(session,capture,self.scene,self.item,false,false,&[],None)?;
        if authored.ancestors.len()!=self.ancestors.len(){return Err(InteractionError("The group hierarchy changed."));}
        let map_vector=|mut vector:[f64;2]|->Result<[f64;2],InteractionError>{
            for (_,x,y) in &self.ancestors {vector=[x[0]*vector[0]+y[0]*vector[1],x[1]*vector[0]+y[1]*vector[1]];}
            for (_,x,y) in authored.ancestors.iter().rev(){
                let det=x[0]*y[1]-x[1]*y[0];
                if !det.is_finite()||det.abs()<1e-12{return Err(InteractionError("The authored group transform is singular."));}
                vector=[(y[1]*vector[0]-y[0]*vector[1])/det,(-x[1]*vector[0]+x[0]*vector[1])/det];
            }
            Ok(vector)
        };
        let axes=|value:&Transform|->Result<([f64;2],[f64;2]),InteractionError>{
            let angle=f64::from(value.rotation_degrees).to_radians();let (sin,cos)=angle.sin_cos();
            let fx=if value.flip_x{-1.0}else{1.0};let fy=if value.flip_y{-1.0}else{1.0};
            Ok((map_vector([cos*f64::from(value.width)*fx,sin*f64::from(value.width)*fx])?,
                map_vector([-sin*f64::from(value.height)*fy,cos*f64::from(value.height)*fy])?))
        };
        let (base_x,base_y)=axes(&self.transform)?;let (x,y)=axes(transform)?;
        let (base_w,base_h)=(base_x[0].hypot(base_x[1]),base_y[0].hypot(base_y[1]));
        let (w,h)=(x[0].hypot(x[1]),y[0].hypot(y[1]));
        if !w.is_finite()||!h.is_finite()||base_w<=1e-12||base_h<=1e-12||w<=1e-12||h<=1e-12
            ||(x[0]*y[0]+x[1]*y[1]).abs()>1e-8*w*h{return Err(InteractionError("The edited transform would require shear."));}
        let sign=if transform.flip_x{-1.0}else{1.0};
        let base_sign=if self.transform.flip_x{-1.0}else{1.0};
        let base_angle=(base_x[1]*base_sign).atan2(base_x[0]*base_sign).to_degrees();
        let base_flip_y=(base_x[0]*base_y[1]-base_x[1]*base_y[0]<0.0)^self.transform.flip_x;
        if ((base_angle-f64::from(original.rotation_degrees)+180.0).rem_euclid(360.0)-180.0).abs()>0.001
            || base_flip_y!=original.flip_y || self.transform.flip_x!=original.flip_x {
            return Err(InteractionError("The projected group orientation cannot be mapped safely."));
        }
        let delta=map_vector([f64::from(transform.x)-f64::from(self.transform.x),f64::from(transform.y)-f64::from(self.transform.y)])?;
        let width=w*f64::from(original.width)/base_w;let height=h*f64::from(original.height)/base_h;
        if !width.is_finite()||!height.is_finite()||!delta[0].is_finite()||!delta[1].is_finite()||width>f64::from(f32::MAX)||height>f64::from(f32::MAX){return Err(InteractionError("The edited scale is out of range."));}
        let mut result=transform.clone();
        result.x=(f64::from(original.x)+delta[0]) as f32;result.y=(f64::from(original.y)+delta[1]) as f32;
        result.width=width as f32;result.height=height as f32;
        result.rotation_degrees=(x[1]*sign).atan2(x[0]*sign).to_degrees() as f32;
        result.flip_y=(x[0]*y[1]-x[1]*y[0]<0.0)^result.flip_x;
        Ok(result)
    }
    pub fn moved(&self, delta: [f64; 2]) -> Result<Transform, InteractionError> {
        let mut delta=self.root_mapping.inverse_vector(delta).map_err(invalid)?;
        for (_,x,y) in self.ancestors.iter().rev() {
            let determinant=x[0]*y[1]-x[1]*y[0];
            if !determinant.is_finite()||determinant.abs()<1e-12{return Err(InteractionError("Group transform is not invertible."));}
            delta=[(y[1]*delta[0]-y[0]*delta[1])/determinant,(-x[1]*delta[0]+x[0]*delta[1])/determinant];
        }
        let mut result = self.transform.clone();
        result.x = (f64::from(result.x) + delta[0]) as f32;
        result.y = (f64::from(result.y) + delta[1]) as f32;
        self.geometry_for(&result)?;
        Ok(result)
    }
    /// Uniformly scale a source in canvas space around a shared anchor.
    /// Uniform scaling commutes with affine parent groups without introducing
    /// a shear that the authored transform cannot represent.
    pub fn scaled_about(&self,anchor:[f64;2],factor:f64)->Result<Transform,InteractionError> {
        if !anchor.into_iter().all(f64::is_finite)||!factor.is_finite()||factor<=0.0 {
            return Err(InteractionError("Group scale requires a positive finite factor."));
        }
        let mut scaled=self.transform.clone();
        scaled.width=(f64::from(scaled.width)*factor) as f32;
        scaled.height=(f64::from(scaled.height)*factor) as f32;
        let scaled_geometry=self.geometry_for(&scaled)?;
        let desired=[anchor[0]+factor*(self.geometry.origin[0]-anchor[0]),anchor[1]+factor*(self.geometry.origin[1]-anchor[1])];
        let shifted=self.moved([desired[0]-scaled_geometry.origin[0],desired[1]-scaled_geometry.origin[1]])?;
        scaled.x=shifted.x;scaled.y=shifted.y;
        self.geometry_for(&scaled)?;
        Ok(scaled)
    }
    /// Canvas rotations are representable only when the parent-to-canvas
    /// linear transform is a similarity; otherwise it would introduce shear.
    fn parent_rotation_sign(&self)->Result<f64,InteractionError> {
        let mut x=self.transform.clone();x.x+=1.0;
        let mut y=self.transform.clone();y.y+=1.0;
        let gx=self.geometry_for(&x)?.origin;let gy=self.geometry_for(&y)?.origin;
        let a=[gx[0]-self.geometry.origin[0],gx[1]-self.geometry.origin[1]];
        let b=[gy[0]-self.geometry.origin[0],gy[1]-self.geometry.origin[1]];
        let a2=a[0]*a[0]+a[1]*a[1];let b2=b[0]*b[0]+b[1]*b[1];let scale=a2.max(b2);
        if !scale.is_finite()||scale<1e-12||(a2-b2).abs()>scale*1e-5||(a[0]*b[0]+a[1]*b[1]).abs()>scale*1e-5 {
            return Err(InteractionError("Group rotation requires uniformly scaled parent groups; this transform would introduce shear."));
        }
        Ok(if a[0]*b[1]-a[1]*b[0]<0.0 {-1.0}else{1.0})
    }
    pub fn can_rotate_group(&self)->bool {self.parent_rotation_sign().is_ok()}
    pub fn rotated_about(&self,anchor:[f64;2],degrees:f64)->Result<Transform,InteractionError>{
        if !anchor.into_iter().all(f64::is_finite)||!degrees.is_finite(){return Err(InteractionError("Group rotation requires finite coordinates."));}
        let sign=self.parent_rotation_sign()?;let radians=degrees.to_radians();let (sin,cos)=radians.sin_cos();
        let x=self.geometry.origin[0]-anchor[0];let y=self.geometry.origin[1]-anchor[1];
        let destination=[anchor[0]+cos*x-sin*y,anchor[1]+sin*x+cos*y];
        let mut result=self.moved([destination[0]-self.geometry.origin[0],destination[1]-self.geometry.origin[1]])?;
        result.rotation_degrees=(f64::from(result.rotation_degrees)+sign*degrees).rem_euclid(360.0) as f32;
        self.geometry_for(&result)?;
        Ok(result)
    }
    /// Snap a uniform group resize by changing its common scale, keeping the
    /// opposite handle fixed. Only the dragged axes can supply a constraint.
    pub fn group_scale_snapped(&self,session:&Session,capture:&CaptureSnapshot,geometry:ItemGeometry,handle:Handle,selected:&[Id],factor:f64,display_scale:[f64;2],control:bool)->Result<(f64,[Option<f64>;2]),InteractionError>{
        self.validate(session,capture)?;
        if !factor.is_finite()||factor<=0.0||!display_scale.into_iter().all(|v|v.is_finite()&&v>0.0){return Err(InteractionError("Invalid group scale or display scale."));}
        let model=session.snapshot();let settings=model.preferences().preview_snapping();
        if control||!settings.enabled()||settings.distance_tenths()==0{return Ok((factor,[None;2]));}
        let distance=[settings.distance()/display_scale[0],settings.distance()/display_scale[1]];
        if !distance.into_iter().all(f64::is_finite){return Err(InteractionError("Preview snapping display scale is outside the valid range."));}
        let unit=handle.unit();let anchor=point(geometry,[1.0-unit[0],1.0-unit[1]]);
        let moving=point(geometry,unit);let extent=[f64::from(self.root_extent.0),f64::from(self.root_extent.1)];
        let mut best=None::<(f64,usize,f64)>;
        let mut consider=|axis:usize,guide:f64,original:f64| {
            if unit[axis]==0.5{return;}
            let leverage=original-anchor[axis];
            if !leverage.is_finite()||leverage.abs()<1e-12||!guide.is_finite(){return;}
            let current=anchor[axis]+factor*leverage;let correction=guide-current;
            if correction.abs()>=distance[axis]{return;}
            let next=factor+correction/leverage;
            if !next.is_finite()||next<=0.0001{return;}
            if best.as_ref().is_none_or(|(old,_,_)|(next-factor).abs()<(*old-factor).abs()) {best=Some((next,axis,guide));}
        };
        let center=point(geometry,[0.5,0.5]);
        for axis in 0..2 {
            if settings.screen_edges(){consider(axis,0.0,moving[axis]);consider(axis,extent[axis],moving[axis]);}
            if settings.canvas_center(){consider(axis,extent[axis]*0.5,center[axis]);}
        }
        if settings.source_edges() {
            let ids:std::collections::HashSet<Id>=selected.iter().copied().chain(std::iter::once(self.item)).collect();
            let container=model.scene_container(self.container).ok_or(InteractionError("The selected container no longer exists."))?;
            for row in container.items().iter().filter(|row|row.visible()&&!ids.contains(&row.id())) {
                let Ok(peer)=Self::read_impl(session,capture,self.scene,row.id(),true,false,&self.auto_text,None) else{continue;};
                let b=peer.geometry.bounds();
                for (axis,min,max) in [(0,b.left,b.right),(1,b.top,b.bottom)] {
                    consider(axis,min,moving[axis]);consider(axis,max,moving[axis]);consider(axis,(min+max)*0.5,moving[axis]);
                }
            }
        }
        let mut guides=[None;2];
        if let Some((next,axis,guide))=best {guides[axis]=Some(guide);Ok((next,guides))}else{Ok((factor,guides))}
    }
    /// Pointer movement through the shared OBS32 alignment policy. Scale is
    /// display points per rendered canvas unit; keyboard nudges use moved().
    pub fn moved_snapped(&self,session:&Session,capture:&CaptureSnapshot,delta:[f64;2],display_scale:[f64;2],control:bool)->Result<Transform,InteractionError>{
        self.validate(session,capture)?;
        if !delta.into_iter().all(f64::is_finite)||!display_scale.into_iter().all(|value|value.is_finite()&&value>0.0){return Err(InteractionError("Preview snapping requires finite movement and positive display scales."));}
        // A click must never author a transform merely because it is near an edge.
        if delta==[0.0;2]{return Ok(self.transform.clone());}
        let model=session.snapshot();let settings=model.preferences().preview_snapping();
        if control||!settings.enabled()||settings.distance_tenths()==0{return self.moved(delta);}
        let distance=[settings.distance()/display_scale[0],settings.distance()/display_scale[1]];
        if !distance.into_iter().all(f64::is_finite){return Err(InteractionError("Preview snapping display scale is outside the valid range."));}
        let moved=self.moved(delta)?;let bounds=self.geometry_for(&moved)?.bounds();
        const EPSILON:f64=0.0001;
        let min=[bounds.left,bounds.top];let max=[bounds.right,bounds.bottom];
        let extent=[f64::from(self.root_extent.0),f64::from(self.root_extent.1)];
        let mut offset=canvas_snap_offset(bounds,extent,settings,distance);
        if settings.source_edges()&&offset.iter().any(|value|value.abs()<EPSILON){
            let scene=model.scene(self.scene).ok_or(InteractionError("The selected scene no longer exists."))?;
            // Model validation limits this walk to4096 siblings/8 filters each.
            // Borrow metadata: no per-peer Target/SourceKind/path/CSS clones.
            for item in scene.items().iter().filter(|item|item.id()!=self.item){
                let source=model.source(item.source()).ok_or(InteractionError("A snapping source no longer exists."))?;
                let Some(basis)=model.source_video_dimensions(source.kind()).map_err(invalid)? else{continue;};
                let actual=if capture.session()==session.identity(){capture.frame(source.id()).filter(|frame|frame.binding.matches_source(source.kind())).map_or(basis,|frame|(frame.image.width(),frame.image.height()))}else{basis};
                let actual=model.effective_filtered_video_dimensions(source,actual,Some(self.root_canvas)).map_err(invalid)?;
                let peer=self.root_mapping.map_geometry(item.transform().geometry_for_frame(basis.0,basis.1,actual.0,actual.1).map_err(invalid)?).map_err(invalid)?.bounds();
                let peer_min=[peer.left,peer.top];let peer_max=[peer.right,peer.bottom];
                for axis in 0..2{let other=1-axis;
                    if offset[axis].abs()<EPSILON&&min[other]<peer_max[other]&&max[other]>peer_min[other]{
                        for correction in [peer_min[axis]-max[axis],peer_max[axis]-min[axis]]{
                            if offset[axis].abs()<EPSILON&&correction.abs()<distance[axis]{offset[axis]=correction;}
                        }
                    }
                }
                if offset.iter().all(|value|value.abs()>=EPSILON){break;}
            }
        }
        self.moved([delta[0]+offset[0],delta[1]+offset[1]])
    }
    /// Snap the union of a selected group, never an individual member, and
    /// exclude the moving members from source-edge candidates.
    pub fn moved_group_snapped(&self,session:&Session,capture:&CaptureSnapshot,group:&[Target],selected:&[Id],delta:[f64;2],display_scale:[f64;2],control:bool)->Result<(Transform,[Option<f64>;2]),InteractionError>{
        self.validate(session,capture)?;
        if !delta.into_iter().all(f64::is_finite)||!display_scale.into_iter().all(|v|v.is_finite()&&v>0.0){return Err(InteractionError("Preview snapping requires finite movement and positive display scales."));}
        if delta==[0.0;2]{return Ok((self.transform.clone(),[None;2]));}
        for member in group {member.validate(session,capture)?;if member.container!=self.container||member.scene!=self.scene{return Err(InteractionError("Selected items must share a container."));}}
        let model=session.snapshot();let settings=model.preferences().preview_snapping();
        if control||!settings.enabled()||settings.distance_tenths()==0{return self.moved(delta).map(|t|(t,[None;2]));}
        let distance=[settings.distance()/display_scale[0],settings.distance()/display_scale[1]];
        if !distance.into_iter().all(f64::is_finite){return Err(InteractionError("Preview snapping display scale is outside the valid range."));}
        let mut bounds=self.geometry_for(&self.moved(delta)?)?.bounds();
        for member in group {
            let peer=member.geometry_for(&member.moved(delta)?)?.bounds();
            bounds.left=bounds.left.min(peer.left);bounds.top=bounds.top.min(peer.top);
            bounds.right=bounds.right.max(peer.right);bounds.bottom=bounds.bottom.max(peer.bottom);
        }
        let extent=[f64::from(self.root_extent.0),f64::from(self.root_extent.1)];
        let mut offset=canvas_snap_offset(bounds,extent,settings,distance);
        let mut guides=[None;2];
        let min=[bounds.left,bounds.top];let max=[bounds.right,bounds.bottom];
        for axis in 0..2 {
            if offset[axis].abs()>0.0001 {
                let center=(min[axis]+max[axis])*0.5+offset[axis];
                guides[axis]=if settings.canvas_center()&&(center-extent[axis]*0.5).abs()<0.0001 {Some(extent[axis]*0.5)}
                    else if (min[axis]+offset[axis]).abs()<0.0001 {Some(0.0)}
                    else if (max[axis]+offset[axis]-extent[axis]).abs()<0.0001 {Some(extent[axis])}else{None};
            }
        }
        const EPSILON:f64=0.0001;
        if settings.source_edges()&&offset.iter().any(|v|v.abs()<EPSILON){
            let container=model.scene_container(self.container).ok_or(InteractionError("The selected container no longer exists."))?;
            let mut moving=std::collections::HashSet::with_capacity(selected.len()+1);
            moving.insert(self.item);moving.extend(selected.iter().copied());
            for row in container.items().iter().filter(|row|row.visible()&&!moving.contains(&row.id())) {
                // OBS uses visible locked sources as snap targets. Unsupported
                // group filters and non-affine geometry remain unavailable.
                let Ok(peer)=Target::read_impl(session,capture,self.scene,row.id(),true,false,&self.auto_text,None) else{continue;};
                let b=peer.geometry.bounds();
                let moving_min=[bounds.left,bounds.top];let moving_max=[bounds.right,bounds.bottom];
                let peer_min=[b.left,b.top];let peer_max=[b.right,b.bottom];
                for axis in 0..2 {
                    if offset[axis].abs()>=EPSILON {continue;}
                    let mc=(moving_min[axis]+moving_max[axis])*0.5;let pc=(peer_min[axis]+peer_max[axis])*0.5;
                    let mw=moving_max[axis]-moving_min[axis];let pw=peer_max[axis]-peer_min[axis];
                    let diff=mc-pc;
                    let (mut moving_edge,mut peer_edge)=if diff<0.0 {(moving_min[axis],peer_min[axis])}else{(moving_max[axis],peer_max[axis])};
                    if diff.abs()>(mw+pw-2.0*distance[axis])*0.5 {
                        moving_edge=if diff<0.0 {moving_max[axis]}else{moving_min[axis]};
                    } else if mw>pw {
                        peer_edge=if diff>0.0 {peer_min[axis]}else{peer_max[axis]};
                        moving_edge=if diff>0.0 {moving_min[axis]}else{moving_max[axis]};
                    }
                    for (correction,guide) in [(peer_edge-moving_edge,peer_edge),(pc-mc,pc)] {
                        if correction.abs()<distance[axis] {offset[axis]=correction;guides[axis]=Some(guide);break;}
                    }
                }
                if offset.iter().all(|v|v.abs()>=EPSILON){break;}
            }
        }
        self.moved([delta[0]+offset[0],delta[1]+offset[1]]).map(|t|(t,guides))
    }
    /// OBS32 handle snapping happens before aspect correction, on dragged
    /// sides only. Alt crop and Ctrl preserve the existing unsnapped path.
    #[allow(clippy::too_many_arguments)]
    pub fn resized_snapped(&self,session:&Session,capture:&CaptureSnapshot,handle:Handle,delta:[f64;2],free_aspect:bool,crop:bool,display_scale:[f64;2],control:bool)->Result<Transform,InteractionError>{
        self.validate(session,capture)?;
        if !delta.into_iter().all(f64::is_finite)||!display_scale.into_iter().all(|value|value.is_finite()&&value>0.0){return Err(InteractionError("Preview snapping requires finite movement and positive display scales."));}
        if delta==[0.0;2]{return Ok(self.transform.clone());}
        let settings=session.snapshot().preferences().preview_snapping();
        if control||crop||!settings.enabled()||settings.distance_tenths()==0{return self.resized(handle,delta,free_aspect,crop);}
        let distance=[settings.distance()/display_scale[0],settings.distance()/display_scale[1]];
        if !distance.into_iter().all(f64::is_finite){return Err(InteractionError("Preview snapping display scale is outside the valid range."));}
        let candidate=self.resized(handle,delta,true,false)?;
        let bounds=self.geometry_for(&candidate)?.bounds();
        let offset=canvas_snap_offset(bounds,[f64::from(self.root_extent.0),f64::from(self.root_extent.1)],settings,distance);
        self.resized(handle,[delta[0]+offset[0],delta[1]+offset[1]],free_aspect,false)
    }
    /// Rotate about the visible box center with OBS's15-degree Shift snap and
    /// five-degree ordinary snap. Control disables ordinary snapping.
    pub fn rotated(&self, start: [f64; 2], current: [f64; 2], shift: bool, control: bool) -> Result<Transform, InteractionError> {
        if start==current {return Ok(self.transform.clone());}
        if !start.into_iter().chain(current).all(f64::is_finite) {
            return Err(InteractionError("Rotation pointer coordinates must be finite."));
        }
        let current=self.root_mapping.inverse_point(current).map_err(invalid)?;
        let center=point(self.base_geometry(&self.transform)?,[0.5,0.5]);
        let vector=[current[0]-center[0],current[1]-center[1]];
        if vector[0].hypot(vector[1])<f64::EPSILON {return Ok(self.transform.clone());}
        let mut angle=vector[1].atan2(vector[0]).to_degrees()+90.0;
        if shift {angle=(angle/15.0).round()*15.0;}
        else if !control {
            for candidate in [f64::from(self.transform.rotation_degrees),-90.0,-45.0,0.0,45.0,90.0,135.0,180.0,225.0,270.0,315.0] {
                let delta=(angle-candidate+180.0).rem_euclid(360.0)-180.0;
                if delta.abs()<5.0 {angle-=delta;}
            }
        }
        let mut result=self.transform.clone();
        result.rotation_degrees=angle as f32;
        let rotated_center=point(self.base_geometry(&result)?,[0.5,0.5]);
        result.x=(f64::from(result.x)+center[0]-rotated_center[0]) as f32;
        result.y=(f64::from(result.y)+center[1]-rotated_center[1]) as f32;
        self.geometry_for(&result)?;
        Ok(result)
    }
    pub fn resized(
        &self,
        handle: Handle,
        delta: [f64; 2],
        free_aspect: bool,
        crop: bool,
    ) -> Result<Transform, InteractionError> {
        let local = inverse_vector(self.geometry, delta)?;
        let geometry=self.base_geometry(&self.transform)?;
        let mut result = self.transform.clone();
        let remaining = [
            self.actual.0 - self.transform.crop.left - self.transform.crop.right,
            self.actual.1 - self.transform.crop.top - self.transform.crop.bottom,
        ];
        if crop {
            let horizontal = (local[0] * f64::from(remaining[0])).round() as i64;
            let vertical = (local[1] * f64::from(remaining[1])).round() as i64;
            let original = self.transform.crop;
            // Item crop addresses the filtered frame, including added padding.
            // The raw binding is only the scale basis, not the crop limit.
            let max_w = self.actual.0.min(self.authored_frame.0);
            let max_h = self.actual.1.min(self.authored_frame.1);
            if handle.left() {
                result.crop.left = (i64::from(original.left) + horizontal)
                    .clamp(0, i64::from(max_w - original.right - 1))
                    as u32;
            }
            if handle.right() {
                result.crop.right = (i64::from(original.right) - horizontal)
                    .clamp(0, i64::from(max_w - original.left - 1))
                    as u32;
            }
            if handle.top() {
                result.crop.top = (i64::from(original.top) + vertical)
                    .clamp(0, i64::from(max_h - original.bottom - 1))
                    as u32;
            }
            if handle.bottom() {
                result.crop.bottom = (i64::from(original.bottom) - vertical)
                    .clamp(0, i64::from(max_h - original.top - 1))
                    as u32;
            }
            let x = f64::from(result.crop.left) - f64::from(original.left);
            let y = f64::from(result.crop.top) - f64::from(original.top);
            result.x = (f64::from(result.x)
                + geometry.axis_x[0] * x / f64::from(remaining[0])
                + geometry.axis_y[0] * y / f64::from(remaining[1]))
                as f32;
            result.y = (f64::from(result.y)
                + geometry.axis_x[1] * x / f64::from(remaining[0])
                + geometry.axis_y[1] * y / f64::from(remaining[1]))
                as f32;
        } else {
            let mut sx = if handle.left() {
                1.0 - local[0]
            } else if handle.right() {
                1.0 + local[0]
            } else {
                1.0
            };
            let mut sy = if handle.top() {
                1.0 - local[1]
            } else if handle.bottom() {
                1.0 + local[1]
            } else {
                1.0
            };
            sx = sx.max(1.0 / f64::from(remaining[0]));
            sy = sy.max(1.0 / f64::from(remaining[1]));
            if !free_aspect {
                let width = geometry.axis_x[0].hypot(geometry.axis_x[1]);
                let height = geometry.axis_y[0].hypot(geometry.axis_y[1]);
                let aspect = f64::from(remaining[0]) / f64::from(remaining[1]);
                if handle.unit()[0] == 0.5 {
                    sx = height * sy * aspect / width;
                } else if handle.unit()[1] == 0.5 || width * sx > height * sy * aspect {
                    sy = width * sx / aspect / height;
                } else {
                    sx = height * sy * aspect / width;
                }
            }
            result.width = (f64::from(result.width) * sx) as f32;
            result.height = (f64::from(result.height) * sy) as f32;
            let dx = if handle.left() { 1.0 - sx } else { 0.0 };
            let dy = if handle.top() { 1.0 - sy } else { 0.0 };
            result.x = (f64::from(result.x)
                + geometry.axis_x[0] * dx
                + geometry.axis_y[0] * dy) as f32;
            result.y = (f64::from(result.y)
                + geometry.axis_x[1] * dx
                + geometry.axis_y[1] * dy) as f32;
        }
        self.geometry_for(&result)?;
        Ok(result)
    }
}

fn canvas_snap_offset(bounds:skylive_core::ItemBounds,extent:[f64;2],settings:skylive_core::PreviewSnapping,distance:[f64;2])->[f64;2]{
    let mut offset=[0.0f64;2];let min=[bounds.left,bounds.top];let max=[bounds.right,bounds.bottom];
    for axis in 0..2{
        if settings.screen_edges(){
            if min[axis].abs()<distance[axis]{offset[axis]=-min[axis];}
            if offset[axis].abs()<0.0001&&(extent[axis]-max[axis]).abs()<distance[axis]{offset[axis]=extent[axis]-max[axis];}
        }
        let center=(min[axis]+max[axis])/2.0;
        if settings.canvas_center()&&(extent[axis]-(max[axis]-min[axis])).abs()>distance[axis]
            &&(extent[axis]/2.0-center).abs()<distance[axis]{offset[axis]=extent[axis]/2.0-center;}
    }offset
}
pub fn point(geometry: ItemGeometry, unit: [f64; 2]) -> [f64; 2] {
    [
        geometry.origin[0] + geometry.axis_x[0] * unit[0] + geometry.axis_y[0] * unit[1],
        geometry.origin[1] + geometry.axis_x[1] * unit[0] + geometry.axis_y[1] * unit[1],
    ]
}
fn inverse_vector(geometry: ItemGeometry, delta: [f64; 2]) -> Result<[f64; 2], InteractionError> {
    let det = geometry.axis_x[0] * geometry.axis_y[1] - geometry.axis_x[1] * geometry.axis_y[0];
    if !det.is_finite() || det.abs() < f64::MIN_POSITIVE {
        return Err(InteractionError(
            "The selected item has degenerate geometry.",
        ));
    }
    Ok([
        (delta[0] * geometry.axis_y[1] - delta[1] * geometry.axis_y[0]) / det,
        (geometry.axis_x[0] * delta[1] - geometry.axis_x[1] * delta[0]) / det,
    ])
}
pub fn pick(
    session: &Session,
    capture: &CaptureSnapshot,
    scene: Id,
    canvas: [f64; 2],
) -> Option<Target> {
    let model = session.snapshot();
    model
        .scene(scene)?
        .items()
        .iter()
        .rev()
        .filter_map(|item| Target::read(session, capture, scene, item.id()).ok())
        .find(|target| target.contains(canvas))
}
/// Pick only against the geometry generation used to draw the displayed pane.
pub fn pick_for_view(session:&Session,capture:&CaptureSnapshot,scene:Id,canvas:[f64;2],view:&crate::preview::AutoTextView)->Option<Target>{
    let model=session.snapshot();
    model.scene(scene)?.items().iter().rev()
        .filter_map(|item|Target::read_for_view(session,capture,scene,item.id(),view).ok())
        .find(|target|target.contains(canvas))
}

/// Select the next editable overlapping item beneath the current selection,
/// wrapping to the top. Hidden and locked items remain ineligible.
pub fn pick_below(session:&Session,capture:&CaptureSnapshot,scene:Id,canvas:[f64;2],selected:Id)->Option<Target>{
    let model=session.snapshot();
    let mut first=None;
    let mut seen=false;
    for item in model.scene(scene)?.items().iter().rev(){
        let Ok(target)=Target::read(session,capture,scene,item.id()) else {continue;};
        if !target.contains(canvas){continue;}
        if seen{return Some(target);}
        seen=target.item==selected;
        if first.is_none(){first=Some(target);}
    }
    first
}
pub fn pick_below_for_view(session:&Session,capture:&CaptureSnapshot,scene:Id,canvas:[f64;2],selected:Id,view:&crate::preview::AutoTextView)->Option<Target>{
    let model=session.snapshot();let mut first=None;let mut seen=false;
    for item in model.scene(scene)?.items().iter().rev(){
        let Ok(target)=Target::read_for_view(session,capture,scene,item.id(),view) else{continue;};
        if !target.contains(canvas){continue;}
        if seen{return Some(target);}seen=target.item==selected;if first.is_none(){first=Some(target);}
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Session, CaptureSnapshot, Id, Id) {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Foreground".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap();
        let item = session.snapshot().scene(scene).unwrap().items()[0].id();
        let capture = CaptureSnapshot::empty(session.identity());
        (session, capture, scene, item)
    }
    #[test]
    fn affine_hit_testing_uses_cropped_rotated_flipped_geometry_and_topmost_editable_item() {
        let (mut session, capture, scene, item) = fixture();
        let mut transform = Transform::full(100, 60);
        transform.x = 200.0;
        transform.y = 90.0;
        transform.rotation_degrees = 37.0;
        transform.flip_x = true;
        transform.crop.left = 100;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let target = Target::read(&session, &capture, scene, item).unwrap();
        assert_eq!(
            pick(
                &session,
                &capture,
                scene,
                point(target.geometry, [0.5, 0.5])
            )
            .unwrap()
            .item,
            item
        );
        assert!(
            pick(
                &session,
                &capture,
                scene,
                point(target.geometry, [-0.2, 0.5])
            )
            .is_none()
        );
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Top".into(),
                rgba: [0, 255, 0, 255],
            })
            .unwrap();
        let top = session.snapshot().scene(scene).unwrap().items()[1].id();
        assert_eq!(
            pick(&session, &capture, scene, [250.0, 150.0])
                .unwrap()
                .item,
            top
        );
        session
            .apply(Command::SetLocked {
                scene,
                item: top,
                locked: true,
            })
            .unwrap();
        assert!(Target::read(&session, &capture, scene, top).is_err());
        session
            .apply(Command::SetVisible {
                scene,
                item,
                visible: false,
            })
            .unwrap();
        assert!(pick(&session, &capture, scene, [250.0, 150.0]).is_none());
    }
    #[test]
    fn move_resize_and_crop_are_candidates_then_one_expected_revision_edit() {
        let (mut session, capture, scene, item) = fixture();
        let target = Target::read(&session, &capture, scene, item).unwrap();
        let before = session.snapshot();
        let candidate = target
            .resized(Handle::TopLeft, [192.0, 108.0], true, true)
            .unwrap();
        assert_eq!((candidate.crop.left, candidate.crop.top), (192, 108));
        assert_eq!((candidate.x, candidate.y), (192.0, 108.0));
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let command = target
            .command(&session, &capture, candidate.clone())
            .unwrap();
        session.apply_at(Some(target.revision), command).unwrap();
        assert_eq!(session.snapshot().revision(), target.revision + 1);
        assert!(target.command(&session, &capture, candidate).is_err());
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            &target.transform
        );
        let target = Target::read(&session, &capture, scene, item).unwrap();
        let stretched = target
            .resized(Handle::BottomRight, [192.0, 0.0], true, false)
            .unwrap();
        assert_eq!(stretched.width, 2112.0);
        assert_eq!(stretched.height, 1080.0);
        let aspect = target
            .resized(Handle::BottomRight, [192.0, 0.0], false, false)
            .unwrap();
        assert_eq!(aspect.width, 2112.0);
        assert_eq!(aspect.height, 1188.0);
        assert_eq!(target.moved([1.0, -10.0]).unwrap().y, -10.0);
    }
    #[test]
    fn live_frame_content_can_advance_but_dimension_or_activation_changes_cancel() {
        use crate::capture::{
            CaptureBackend, CaptureCapabilities, CaptureError, CaptureLimits, CaptureWorker,
            DisplayInfo,
        };
        use skylive_core::{DisplayCaptureBinding, DisplayTarget};
        use skylive_media::{MediaTime, RgbaFrame};
        use std::sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        };
        struct Fake(Arc<AtomicU32>);
        impl CaptureBackend for Fake {
            fn capabilities(&self) -> CaptureCapabilities {
                CaptureCapabilities {
                    available: true,
                    backend: "injected-preview-test",
                    cursor: true,
                    hdr: false,
                    note: None,
                }
            }
            fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
                Ok(vec![])
            }
            fn capture(
                &mut self,
                _: &DisplayCaptureBinding,
                _: CaptureLimits,
            ) -> Result<RgbaFrame, CaptureError> {
                RgbaFrame::solid(self.0.load(Ordering::SeqCst), 2, [255, 0, 0, 255])
                    .map_err(|_| CaptureError::InvalidFrame("fixture"))
            }
        }
        let mut session = Session::new();
        let scene = session.preview();
        let binding = DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: "injected".into(),
            },
            true,
            2,
            2,
        )
        .unwrap();
        session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Live".into(),
                binding,
            })
            .unwrap();
        let item = session.snapshot().scene(scene).unwrap().items()[0].id();
        let width = Arc::new(AtomicU32::new(4));
        let mut worker = CaptureWorker::new(Box::new(Fake(width.clone()))).unwrap();
        worker
            .synchronize(&session, &[crate::transition::FrameScene::Live(scene)])
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        fn take(
            worker: &mut CaptureWorker,
            session: &Session,
            now: MediaTime,
            deadline: std::time::Instant,
        ) {
            assert!(worker.request_frame(session, now).unwrap());
            while worker.poll(session, now).unwrap().is_none() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
        take(&mut worker, &session, MediaTime(0), deadline);
        let target = Target::read(&session, &worker.snapshot(), scene, item).unwrap();
        assert_eq!(target.geometry.axis_x, [4.0, 0.0]);
        take(&mut worker, &session, MediaTime(100_000_000), deadline);
        assert!(
            target.validate(&session, &worker.snapshot()).is_ok(),
            "new content alone is not a new geometry"
        );
        width.store(6, Ordering::SeqCst);
        take(&mut worker, &session, MediaTime(200_000_000), deadline);
        assert!(target.validate(&session, &worker.snapshot()).is_err());
        let target = Target::read(&session, &worker.snapshot(), scene, item).unwrap();
        worker.synchronize(&session, &[]).unwrap();
        assert!(target.validate(&session, &worker.snapshot()).is_err());
        worker.close();
    }
}
