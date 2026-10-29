//! Stable Scene/Group reference scale, interpreted under an explicit raw canvas.
//! Materialized f32 extents are redundant canonical data, never the next rebase input.
use super::*;

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="CanvasBasisData")]
pub(super) struct CanvasBasis {width:u32,height:u32}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanvasBasisData {width:u32,height:u32}
impl TryFrom<CanvasBasisData> for CanvasBasis {
    type Error=Error;
    fn try_from(value:CanvasBasisData)->Result<Self,Error>{Self::new(value.width,value.height)}
}
impl CanvasBasis {
    fn new(width:u32,height:u32)->Result<Self,Error>{
        VideoSettings::new(width,height,1,1)?;Ok(Self{width,height})
    }
    fn dimensions(self)->(u32,u32){(self.width,self.height)}
}

#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
#[serde(try_from="ScaleAnchorData")]
pub(super) struct ScaleAnchor {width:f32,height:f32,basis_width:u32,basis_height:u32}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleAnchorData {width:f32,height:f32,basis_width:u32,basis_height:u32}
impl TryFrom<ScaleAnchorData> for ScaleAnchor {
    type Error=Error;
    fn try_from(value:ScaleAnchorData)->Result<Self,Error>{Self::new(value.width,value.height,(value.basis_width,value.basis_height))}
}
impl ScaleAnchor {
    pub(super) fn new(width:f32,height:f32,basis:(u32,u32))->Result<Self,Error>{
        validate_raw_basis(basis)?;
        let mut transform=Transform::full(basis.0,basis.1);transform.width=width;transform.height=height;
        transform.geometry(basis.0,basis.1)?;
        Ok(Self{width,height,basis_width:basis.0,basis_height:basis.1})
    }
    pub(super) fn extents(self,basis:(u32,u32))->Result<(f32,f32),Error>{
        validate_raw_basis(basis)?;
        let extent=if basis==(self.basis_width,self.basis_height){(self.width,self.height)}else{
            ((f64::from(self.width)*f64::from(basis.0)/f64::from(self.basis_width)) as f32,
             (f64::from(self.height)*f64::from(basis.1)/f64::from(self.basis_height)) as f32)
        };
        // The original stable anchor remains unchanged even if this context is
        // rejected because its materialized extent exceeds authored limits.
        let mut transform=Transform::full(basis.0,basis.1);transform.width=extent.0;transform.height=extent.1;
        transform.geometry(basis.0,basis.1)?;Ok(extent)
    }
    pub(super) fn agrees(self,transform:&Transform,basis:(u32,u32))->Result<bool,Error>{
        Ok(self.extents(basis)?==(transform.width,transform.height))
    }
}
fn validate_raw_basis(basis:(u32,u32))->Result<(),Error>{
    if basis.0==0||basis.1==0||basis.0>8192||basis.1>8192||u64::from(basis.0)*u64::from(basis.1)>MAX_DISPLAY_PIXELS{
        return Err(Error::Invalid("Invalid Scene or Group scale anchor basis."));
    }Ok(())
}
#[derive(Clone,Copy)]
struct ReferenceBasis {source:Id,basis:(u32,u32),scene:bool}
#[derive(Clone,Copy)]
struct PreviousExtent {item:Id,source:Id,width:f32,height:f32}
impl Collection {
    /// Storage checks this before legacy context adoption can add metadata.
    pub fn has_scene_canvas_metadata(&self)->bool{
        self.scene_canvas_basis.is_some()||self.scenes.iter().chain(&self.groups).any(|scene|scene.items.iter().any(|item|item.scale_anchor.is_some()))
    }
    fn check_scale_entity_limits(&self)->Result<(),Error>{
        if self.sources.len()>MAX_SOURCES||self.scenes.len()>MAX_SCENES||self.groups.len()>MAX_SOURCES
            ||self.scenes.iter().chain(&self.groups).try_fold(0usize,|sum,scene|sum.checked_add(scene.items.len())).is_none_or(|count|count>MAX_ITEMS){
            return Err(Error::Limit("collection entity budget"));
        }Ok(())
    }
    fn reference_bases(&self,canvas:Option<CanvasBasis>)->Result<Vec<ReferenceBasis>,Error>{
        self.check_scale_entity_limits()?;
        let mut rows=Vec::new();rows.try_reserve_exact(self.sources.len()).map_err(|_|Error::Limit("Cannot allocate bounded Scene scale metadata."))?;
        for source in &self.sources {
            let value=match source.kind(){
                SourceKind::Scene{..}=>canvas.map(|canvas|ReferenceBasis{source:source.id(),basis:canvas.dimensions(),scene:true}),
                SourceKind::Group{group}=>Some(ReferenceBasis{source:source.id(),basis:group.basis(),scene:false}),
                _=>None,
            };
            if let Some(value)=value{rows.push(value);}
        }
        rows.sort_unstable_by_key(|value|value.source);Ok(rows)
    }
    pub(super) fn validate_scene_canvas_metadata(&self)->Result<(),Error>{
        self.check_scale_entity_limits()?;
        if let Some(basis)=self.scene_canvas_basis{CanvasBasis::new(basis.width,basis.height)?;}
        let rows=self.reference_bases(self.scene_canvas_basis)?;
        for scene in self.scenes.iter().chain(&self.groups){for item in &scene.items{if let Some(anchor)=item.scale_anchor{
            if self.scene_canvas_basis.is_none(){return Err(Error::Invalid("Scene scale anchors require an explicit interpretation canvas."));}
            let basis=rows.binary_search_by_key(&item.source,|row|row.source).ok().map(|index|rows[index].basis)
                .ok_or(Error::Invalid("Only Scene and Group references may have scale anchors."))?;
            if !anchor.agrees(&item.transform,basis)?{return Err(Error::Invalid("Scene scale anchor does not match its stored extent and basis."));}
        }}}
        Ok(())
    }
}
impl StudioSnapshot {
    pub(super) fn validate_current_scene_canvas(&self)->Result<(),Error>{
        if let Some(basis)=self.scene_canvas_basis {
            if basis.dimensions()!=(self.video().width(),self.video().height()){
                return Err(Error::Invalid("Scene interpretation canvas does not match the selected profile."));
            }
            let rows=self.reference_bases(Some(basis))?;
            for scene in self.scenes.iter().chain(&self.groups){for item in &scene.items{
                if rows.binary_search_by_key(&item.source,|row|row.source).is_ok()&&item.scale_anchor.is_none(){
                    return Err(Error::Invalid("Materialized Scene and Group references require stable scale anchors."));
                }
            }}
        }Ok(())
    }
    /// Candidate only. Existing explicit metadata is checked against its OLD
    /// saved context before adopting the actual selected-profile context.
    pub(super) fn materialize_scene_canvas(&mut self)->Result<(),Error>{
        self.profiles.validate()?;self.collection.validate()?;
        self.prepare_scale_anchors(None)?;
        if self.rebase_scene_canvas()?{self.normalize_groups(None)?;}
        Ok(())
    }
    pub(super) fn scale_anchor_for(&self,source:&Source,transform:&Transform)->Result<Option<ScaleAnchor>,Error>{
        let basis=match source.kind(){SourceKind::Scene{..}=>(self.video().width(),self.video().height()),SourceKind::Group{group}=>group.basis(),_=>return Ok(None)};
        ScaleAnchor::new(transform.width,transform.height,basis).map(Some)
    }
    /// Called once after an authored command, before automatic representation
    /// changes. Only a genuine extent/source change or a new reference captures
    /// a new anchor; context-only copied anchors can retain their original scale.
    pub(super) fn prepare_scale_anchors(&mut self,previous:Option<&StudioSnapshot>)->Result<(),Error>{
        self.profiles.validate()?;
        let canvas=self.scene_canvas_basis.unwrap_or(CanvasBasis::new(self.video().width(),self.video().height())?);
        let rows=self.reference_bases(Some(canvas))?;
        let mut old=Vec::new();
        if let Some(previous)=previous {
            previous.check_scale_entity_limits()?;
            let count=previous.scenes.iter().chain(&previous.groups).map(|scene|scene.items.len()).sum();
            old.try_reserve_exact(count).map_err(|_|Error::Limit("Cannot allocate bounded Scene scale metadata."))?;
            for scene in previous.scenes.iter().chain(&previous.groups){for item in &scene.items{
                old.push(PreviousExtent{item:item.id,source:item.source,width:item.transform.width,height:item.transform.height});
            }}old.sort_unstable_by_key(|value|value.item);
        }
        for scene in self.collection.scenes.iter_mut().chain(&mut self.collection.groups){for item in &mut scene.items{
            let Some(row)=rows.binary_search_by_key(&item.source,|row|row.source).ok().map(|index|rows[index])else{
                if item.scale_anchor.is_some(){return Err(Error::Invalid("Only Scene and Group references may have scale anchors."));}continue;
            };
            let previous=old.binary_search_by_key(&item.id,|value|value.item).ok().map(|index|old[index]);
            let authored_change=previous.is_some_and(|old|old.source!=item.source||old.width!=item.transform.width||old.height!=item.transform.height);
            let retained=match item.scale_anchor{Some(anchor)if !authored_change&&anchor.agrees(&item.transform,row.basis)?=>Some(anchor),_=>None};
            item.scale_anchor=match retained{Some(anchor)=>Some(anchor),None=>Some(ScaleAnchor::new(item.transform.width,item.transform.height,row.basis)?)};
        }}
        Ok(())
    }
    /// Re-encode directly from immutable authoring anchors, never current f32
    /// materialization. None is legacy adoption, not a historical repair.
    pub(super) fn rebase_scene_canvas(&mut self)->Result<bool,Error>{
        let next=CanvasBasis::new(self.video().width(),self.video().height())?;
        let previous=self.scene_canvas_basis;
        if previous==Some(next){return Ok(false);}
        let rows=self.reference_bases(Some(next))?;
        for scene in self.collection.scenes.iter_mut().chain(&mut self.collection.groups){for item in &mut scene.items{
            if let Some(row)=rows.binary_search_by_key(&item.source,|row|row.source).ok().map(|index|rows[index]).filter(|row|row.scene){
                let anchor=item.scale_anchor.ok_or(Error::Invalid("Materialized Scene and Group references require stable scale anchors."))?;
                let (width,height)=anchor.extents(row.basis)?;item.transform.width=width;item.transform.height=height;
            }
        }}
        self.collection.scene_canvas_basis=Some(next);Ok(previous.is_some_and(|old|old!=next))
    }
    /// Scene history deliberately excludes these live/nonhistorical overlays.
    fn preserve_history_overlays(&mut self,live:&StudioSnapshot){
        for source in &mut self.collection.sources{if let Some(current)=live.collection.sources.iter().find(|value|value.id==source.id){source.playback_hotkeys=current.playback_hotkeys.clone();source.audio_hotkeys=current.audio_hotkeys.clone();}}
        self.collection.saved_projectors=live.collection.saved_projectors.clone();
        let tags:std::collections::HashMap<_,_>=live.scenes.iter().chain(&live.groups).flat_map(|scene|scene.items.iter().map(|item|(item.id,item.color_tag))).collect();
        for scene in self.collection.scenes.iter_mut().chain(&mut self.collection.groups){for item in &mut scene.items{if let Some(tag)=tags.get(&item.id){item.color_tag=*tag;}}}
        for scene in &mut self.collection.scenes{if let Some(current)=live.collection.scenes.iter().find(|value|value.id==scene.id){scene.show_in_multiview=current.show_in_multiview;}}
        self.collection.preserve_live_mixer_state(&live.collection);
    }
}
impl Session {
    /// Prepare bounded history traversal without touching either stack. A later
    /// failed candidate leaves skipped entries present for an explicit retry.
    pub(super) fn prepare_scene_history(&self,candidate:&mut StudioSnapshot,redo:bool)->Result<usize,Error>{
        let history=if redo{&self.redo}else{&self.undo};
        if history.is_empty(){return Err(Error::EmptyHistory);}
        for (index,entry) in history.iter().rev().enumerate(){
            candidate.collection=(**entry).clone();candidate.preserve_history_overlays(&self.current);
            candidate.next_id=candidate.next_id.max(self.high_water);
            candidate.next_quick_id=candidate.next_quick_id.max(self.quick_high_water);
            candidate.next_transition_preset_id=candidate.next_transition_preset_id.max(self.preset_high_water);
            candidate.materialize_scene_canvas()?;crate::image_source::rebind_history_images(candidate,&self.image_refresh_overrides)?;crate::slideshow_entries::rebind_history_slideshows(candidate,&self.slideshow_refresh_overrides)?;candidate.validate()?;
            if candidate.collection!=self.current.collection{return Ok(index+1);}
        }
        Ok(history.len())
    }
    pub(super) fn consume_scene_history(&mut self,redo:bool,count:usize){
        let history=if redo{&mut self.redo}else{&mut self.undo};for _ in 0..count{history.pop_back();}
    }
}
