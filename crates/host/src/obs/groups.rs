//! Exact OBS32 private Group source graph; strict metadata, no arbitrary passthrough.
use super::*;
use skylive_core::{GroupTree,GroupTreeEntry,GroupTreeItem};
// libobs/obs-canvas.c24: primary canvas is a static identity, not a guess.
const MAIN_CANVAS_UUID:&str="6c69626f-6273-4c00-9d88-c5136d61696e";
pub(super) fn main_canvas(source:&Map<String,Value>,path:&str)->Result<(),ObsError>{
    if let Some(value)=source.get("canvas_uuid"){
        let value=value.as_str().ok_or_else(||issue(path,"invalid","Canvas UUID must be a string"))?;
        if !value.is_empty()&&value!=MAIN_CANVAS_UUID{return Err(issue(path,"unsupported_canvas","Only the OBS32 main canvas is represented by this importer"));}
    }Ok(())
}

pub(super) fn parse_settings(settings:&Map<String,Value>,path:&str)->Result<(u32,u32),ObsError>{
    known(settings,&["items","id_counter","custom_size","cx","cy"],path)?;
    neutral(settings,"custom_size",json!(true),path)?;
    if settings.get("id_counter").is_some_and(|value|value.as_i64().is_none_or(|value|value<0)){return Err(issue(path,"invalid","Group id_counter must be nonnegative"));}
    let dimension=|key:&str|->Result<u32,ObsError>{match settings.get(key){None=>Ok(0),Some(value)=>value.as_u64().and_then(|value|u32::try_from(value).ok()).ok_or_else(||issue(path,"invalid","Group raster dimensions must be unsigned 32-bit integers"))}};
    let(width,height)=(dimension("cx")?,dimension("cy")?);
    if (width==0)!=(height==0)||width>8192||height>8192||u64::from(width)*u64::from(height)>skylive_core::MAX_DISPLAY_PIXELS{return Err(issue(path,"group_canvas","Group raster dimensions exceed the represented source bounds"));}
    Ok((width,height))
}
pub(super) fn preflight(sources:&[InputSource],active_items:usize)->Result<(),ObsError>{
    // Transport admission before linked resource I/O; Core revalidates the
    // same authored-name contract when the candidate is published.
    for source in sources{if source.name.trim().is_empty()||source.name.len()>256||source.name.chars().any(char::is_control){return Err(issue(&source.path,"invalid","name must contain 1–256 UTF-8 bytes and no controls"));}}
    let providers=sources.iter().filter(|source|!source.kind.container()).count();
    if active_items.checked_add(providers).is_none_or(|count|count>skylive_core::MAX_ITEMS){return Err(issue("$","limit","OBS import temporary and active items exceed the Core item budget"));}
    let mut parents=vec![None;sources.len()];
    for(index,source)in sources.iter().enumerate(){if let Some(items)=source.kind.items(){for item in items{
        if matches!(sources[item.source].kind,InputKind::Group{..}){
            if parents[item.source].replace(index).is_some(){return Err(issue(&sources[item.source].path,"group_alias","A private Group must have exactly one owning item"));}
        }
    }}}
    for(index,source)in sources.iter().enumerate(){if !matches!(source.kind,InputKind::Group{..}){continue;}
        let mut parent=index;let mut seen=HashSet::new();
        loop {
            if !seen.insert(parent)||seen.len()>skylive_core::MAX_SCENE_DEPTH{return Err(issue(&source.path,"group_depth","Group ownership contains a cycle or exceeds depth 16"));}
            parent=parents[parent].ok_or_else(||issue(&source.path,"group_alias","A private Group must have exactly one owning item"))?;
            if matches!(sources[parent].kind,InputKind::Scene{..}){break;}
        }
    }Ok(())
}
fn apply(session:&mut Session,command:Command,path:&str)->Result<skylive_core::Change,ObsError>{session.apply(command).map_err(|error|issue(path,"engine_validation",error.to_string()))}
fn item_settings(input:&InputItem)->GroupTreeItem{GroupTreeItem{transform:input.transform.clone(),visible:input.visible,locked:input.locked,compositing:input.compositing,color_tag:input.color_tag}}
fn tree(index:usize,input:&InputItem,sources:&[InputSource],mapping:&HashMap<usize,Id>)->Result<GroupTree,ObsError>{
    let source=&sources[index];let InputKind::Group{items,width,height}=&source.kind else{return Err(issue(&source.path,"invalid_reference","Group definition missing"));};
    let mut children=Vec::new();children.try_reserve_exact(items.len()).map_err(|_|issue(&source.path,"limit","Cannot allocate bounded Group import metadata."))?;
    for item in items {let target=if matches!(sources[item.source].kind,InputKind::Group{..}){
        GroupTreeEntry::Group{group:tree(item.source,item,sources,mapping)?}
    }else if matches!(sources[item.source].kind,InputKind::Scene{..}){
        GroupTreeEntry::Scene{scene:*mapping.get(&item.source).ok_or_else(||issue(&source.path,"invalid_reference","Scene identity missing"))?,item:item_settings(item)}
    }else{
        GroupTreeEntry::Existing{source:*mapping.get(&item.source).ok_or_else(||issue(&source.path,"invalid_reference","Prepared source identity missing"))?,item:item_settings(item)}
    };children.push(target);}
    Ok(GroupTree{name:source.name.clone(),width:*width,height:*height,filters:source.filters.iter().map(|filter|filter.filter.clone()).collect(),item:item_settings(input),children})
}
fn retain_filters(owner:&skylive_core::Source,input:&InputSource,identities:&mut ObsIdentities)->Result<(),ObsError>{
    if owner.video_filters().len()!=input.filters.len(){return Err(issue(&input.path,"engine_validation","Group item order receipt disagrees with imported definition"));}
    for(filter,raw)in owner.video_filters().iter().zip(&input.filters){identities.filters.insert(filter.id().ok_or_else(||issue(&input.path,"engine_validation","New Group filter identity missing"))?,raw.raw.clone());}Ok(())
}
fn retain_group(index:usize,item:Id,snapshot:&StudioSnapshot,sources:&[InputSource],mapping:&mut HashMap<usize,Id>,identities:&mut ObsIdentities)->Result<(),ObsError>{
    let(_,wrapper)=snapshot.item_container(item).ok_or_else(||issue("$","engine_validation","New Group item missing"))?;
    let owner=snapshot.source(wrapper.source()).ok_or_else(||issue("$","engine_validation","New Group source missing"))?;
    let SourceKind::Group{group}=owner.kind()else{return Err(issue("$","engine_validation","New source is not a Group"));};
    let input=&sources[index];
    let InputKind::Group{width,height,..}=&input.kind else{return Err(issue(&input.path,"invalid_reference","Group definition missing"));};
    let expected_raster=if *width==0{None}else{Some((*width,*height))};
    if owner.name()!=input.name||snapshot.group_native_dimensions(group.scene()).map_err(|error|issue(&input.path,"group_canvas",error.to_string()))?!=expected_raster{
        return Err(issue(&input.path,"group_canvas","Declared Group dimensions disagree with its derived raster."));
    }
    mapping.insert(index,owner.id());identities.sources.insert(owner.id(),input.raw.clone());retain_filters(owner,input,identities)?;
    let actual=snapshot.scene_container(group.scene()).ok_or_else(||issue(&input.path,"engine_validation","New Group container missing"))?;
    let expected=input.kind.items().ok_or_else(||issue(&input.path,"invalid_reference","Group definition missing"))?;
    if actual.items().len()!=expected.len(){return Err(issue(&input.path,"engine_validation","Group item order receipt disagrees with imported definition"));}
    for(item,raw)in actual.items().iter().zip(expected){
        if matches!(sources[raw.source].kind,InputKind::Group{..}){retain_group(raw.source,item.id(),snapshot,sources,mapping,identities)?;}
        else {
            let expected=*mapping.get(&raw.source).ok_or_else(||issue(&input.path,"invalid_reference","Prepared source identity missing"))?;
            let owner=snapshot.source(item.source()).ok_or_else(||issue(&input.path,"invalid_reference","Prepared source identity missing"))?;
            let matches=if matches!(sources[raw.source].kind,InputKind::Scene{..}){matches!(owner.kind(),SourceKind::Scene{scene}if *scene==expected)}else{owner.id()==expected};
            if !matches{return Err(issue(&input.path,"engine_validation","Group item order receipt disagrees with imported definition"));}
        }
        identities.items.insert(item.id(),raw.raw.clone());
    }
    Ok(())
}
fn create_leaf(session:&mut Session,scene:Id,source:&InputSource,identities:&mut ObsIdentities)->Result<(Id,Id),ObsError>{
    let(command,color_size,gain,muted)=match &source.kind{
        InputKind::Text{settings,gain,muted,..}=>(Command::AddTextSource{scene,name:source.name.clone(),settings:settings.clone()},None,*gain,*muted),
        InputKind::Wasapi{binding,gain,muted}=>(Command::AddAudioCaptureSource{scene,name:source.name.clone(),binding:binding.clone()},None,*gain,*muted),
        InputKind::Media{input,gain,muted}=>(Command::AddMediaSource{scene,name:source.name.clone(),media:input.settings().cloned().ok_or_else(||issue(&source.path,"invalid_reference","Media Source was not prepared"))?},None,*gain,*muted),
        InputKind::Browser{browser,gain,muted}=>(Command::AddBrowserSource{scene,name:source.name.clone(),browser:browser.clone()},None,*gain,*muted),
        InputKind::Window{binding,gain,muted}=>(Command::AddWindowCaptureSource{scene,name:source.name.clone(),binding:binding.clone()},None,*gain,*muted),
        InputKind::Camera{binding,gain,muted}=>(Command::AddVideoCaptureSource{scene,name:source.name.clone(),binding:binding.clone()},None,*gain,*muted),
        InputKind::Display{binding,gain,muted}=>(Command::AddDisplayCaptureSource{scene,name:source.name.clone(),binding:binding.clone()},None,*gain,*muted),
        InputKind::Color{rgba,width,height,gain,muted}=>(Command::AddColorSource{scene,name:source.name.clone(),rgba:*rgba},Some((*width,*height)),*gain,*muted),
        InputKind::SlideShow{input,gain,muted}=>(Command::AddImageSlideShow{scene,name:source.name.clone(),settings:input.settings().cloned().ok_or_else(||issue(&source.path,"invalid_reference","Slideshow was not prepared"))?},None,*gain,*muted),
        InputKind::Image{asset:Some(asset),settings,gain,muted,..}=>(Command::ImageSource{command:skylive_core::ImageSourceCommand::Add{scene,name:source.name.clone(),asset:asset.clone(),settings:*settings}},None,*gain,*muted),
        _=>return Err(issue(&source.path,"invalid_reference","Source was not prepared")),
    };
    let item=apply(session,command,&source.path)?.created.ok_or_else(||issue(&source.path,"engine_validation","Source command returned no item identity"))?;
    let id=session.snapshot().item_container(item).ok_or_else(||issue(&source.path,"engine_validation","New source item missing"))?.1.source();
    configure_source(session,id,source,identities,gain,muted,color_size)?;
    Ok((id,item))
}
fn configure_source(session:&mut Session,id:Id,source:&InputSource,identities:&mut ObsIdentities,gain:f32,muted:bool,color_size:Option<(u32,u32)>)->Result<(),ObsError>{
    identities.sources.insert(id,source.raw.clone());
    if !source.filters.is_empty(){
        apply(session,Command::SetSourceVideoFilters{source:id,filters:source.filters.iter().map(|filter|filter.filter.clone()).collect()},&source.path)?;
        let snapshot=session.snapshot();retain_filters(snapshot.source(id).ok_or_else(||issue(&source.path,"engine_validation","New filter owner missing"))?,source,identities)?;
    }
    if let Some((width,height))=color_size{apply(session,Command::SetColorSize{source:id,width,height},&source.path)?;}
    apply(session,Command::SetAudio{source:id,gain,muted},&source.path)?;
    if !source.audio_tracks.is_all(){apply(session,Command::SetSourceAudioTracks{source:id,tracks:source.audio_tracks},&source.path)?;}
    if !source.monitoring.is_off(){apply(session,Command::SetSourceAudioMonitoring{source:id,mode:source.monitoring},&source.path)?;}
    if source.sync_offset_ms!=0||source.force_mono||source.balance!=0.5||!source.push_audio.is_default(){let tracks=session.snapshot().source(id).ok_or_else(||issue(&source.path,"engine_validation","New source missing"))?.audio_properties().audio_tracks();let properties=skylive_core::SourceAudioProperties::new(source.force_mono,source.balance).and_then(|properties|properties.with_sync_offset_ms(source.sync_offset_ms)).map_err(|error|issue(&source.path,"engine_validation",error.to_string()))?.with_audio_tracks(tracks).with_push(source.push_audio);apply(session,Command::SetSourceAudioProperties{source:id,properties},&source.path)?;}
    Ok(())
}
pub(super) fn create_global(session:&mut Session,slot:skylive_core::AudioSlot,source:&InputSource,identities:&mut ObsIdentities)->Result<Id,ObsError>{
    let InputKind::Wasapi{binding,gain,muted}=&source.kind else{return Err(issue(&source.path,"global_audio_provider","Global audio requires a supported Windows WASAPI source"));};
    if binding.flow()!=slot.flow(){return Err(issue(&source.path,"global_audio_flow","Global audio slot and source flow differ"));}
    let id=apply(session,Command::AddGlobalAudioCaptureSource{slot,name:source.name.clone(),binding:binding.clone()},&source.path)?.created.ok_or_else(||issue(&source.path,"engine_validation","Global audio command returned no source identity"))?;
    configure_source(session,id,source,identities,*gain,*muted,None)?;Ok(id)
}
pub(super) fn bootstrap(session:&mut Session,sources:&[InputSource],order:&[usize],mapping:&mut HashMap<usize,Id>,identities:&mut ObsIdentities,audio_only:&[bool])->Result<(),ObsError>{
    // Root Scene filters must exist before computing a Group containing that
    // Scene alias: filtered dimensions are not the unfiltered canvas metadata.
    for &index in order{
        let scene=mapping[&index];identities.sources.insert(scene,sources[index].raw.clone());
        if !sources[index].filters.is_empty(){apply(session,Command::SetSceneVideoFilters{scene,filters:sources[index].filters.iter().map(|filter|filter.filter.clone()).collect()},&sources[index].path)?;}
        let snapshot=session.snapshot();if let Some(owner)=snapshot.scene_source(scene){identities.sources.insert(owner.id(),sources[index].raw.clone());retain_filters(owner,&sources[index],identities)?;}
    }
    let temporary_scene=*mapping.get(&order[0]).ok_or_else(||issue("$","invalid_reference","First scene identity missing"))?;
    let mut temporary=Vec::new();temporary.try_reserve_exact(sources.len()).map_err(|_|issue("$","limit","Cannot allocate bounded Group import metadata."))?;
    for(index,source)in sources.iter().enumerate(){if source.kind.container()||mapping.contains_key(&index){continue;}let(id,item)=create_leaf(session,temporary_scene,source,identities)?;mapping.insert(index,id);temporary.push(item);}
    for &index in order{
        let scene=mapping[&index];let items=sources[index].kind.items().ok_or_else(||issue(&sources[index].path,"invalid_reference","Scene definition missing"))?;
        for input in items {
            let group=matches!(sources[input.source].kind,InputKind::Group{..});
            let command=if group{Command::AddGroupTree{scene,tree:tree(input.source,input,sources,mapping)?}}
                else if matches!(sources[input.source].kind,InputKind::Scene{..}){Command::AddSceneSource{scene,target:mapping[&input.source]}}
                else{Command::AddExistingSource{scene,source:mapping[&input.source]}};
            let item=apply(session,command,&sources[index].path)?.created.ok_or_else(||issue("$","engine_validation","Source command returned no item identity"))?;
            if group {retain_group(input.source,item,&session.snapshot(),sources,mapping,identities)?;}else{
                if !audio_only[input.source]{apply(session,Command::SetTransform{scene,item,transform:input.transform.clone()},&sources[index].path)?;}
                apply(session,Command::SetItemCompositing{scene,item,settings:input.compositing},&sources[index].path)?;
                apply(session,Command::SetSceneItemColorTag{scene,item,tag:input.color_tag},&sources[index].path)?;
                apply(session,Command::SetVisible{scene,item,visible:input.visible},&sources[index].path)?;
                apply(session,Command::SetLocked{scene,item,locked:input.locked},&sources[index].path)?;
            }
            identities.items.insert(item,input.raw.clone());
        }
    }
    // Every prepared provider now has a real reference. Removing recorded
    // private staging items cannot prune or retag its actual source identity.
    for item in temporary{apply(session,Command::RemoveItem{scene:temporary_scene,item},"$")?;}
    for &index in order{let snapshot=session.snapshot();if let Some(owner)=snapshot.scene_source(mapping[&index]){identities.sources.insert(owner.id(),sources[index].raw.clone());}}
    Ok(())
}

pub(super) fn encode_container(collection:&StudioSnapshot,scene:&skylive_core::Scene,mut object:Map<String,Value>,identities:Option<&ObsIdentities>,uuids:&HashMap<Id,String>,group:bool)->Result<Map<String,Value>,ObsError>{
        let mut items = Vec::new();
        let old_settings = object
            .get("settings")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut counter = old_settings
            .get("id_counter")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        // Reserve imported IDs first so a newly added item cannot collide.
        for item in scene.items() {
            if let Some(id) = identities
                .and_then(|ids| ids.items.get(&item.id()))
                .and_then(|map| map.get("id"))
                .and_then(Value::as_i64)
            {
                counter = counter.max(id);
            }
        }
        let mut used_ids=HashSet::new();
        for item in scene.items() {
            if item.transform().opacity != 1.0 {
                return Err(issue(
                    "$.sources.settings.items.opacity",
                    "unsupported",
                    "OBS scene items have no opacity field; per-item opacity export is unsupported",
                ));
            }
            let source = collection
                .source(item.source())
                .ok_or_else(|| issue("$", "invalid_reference", "source missing"))?;
            let dimensions = collection
                .source_video_dimensions(source.kind())
                .map_err(|error| {
                    issue(
                        "$.sources.settings.items",
                        "invalid_reference",
                        error.to_string(),
                    )
                })?;
            let audio_only=matches!(source.kind(),SourceKind::MediaSource{media}if media.has_audio()&&media.video_dimensions().is_none())||matches!(source.kind(),SourceKind::AudioCapture{..});
            if dimensions.is_none()&&!audio_only{return Err(issue("$.sources.settings.items","unsupported","nonvisual source item"));}
            let mut value = identities
                .and_then(|ids| ids.items.get(&item.id()))
                .cloned()
                .unwrap_or_default();
            coordinates::absolute_export(&mut value);
            value.insert("group_item_backup".into(),json!(false));
            if !matches!(source.kind(),SourceKind::Group{..}){if let Some(settings)=value.get_mut("private_settings").and_then(Value::as_object_mut){settings.remove("collapsed");}}
            let retained=value.get("id").and_then(Value::as_i64).filter(|id|*id>0&&!used_ids.contains(id));
            let id=match retained{Some(id)=>id,None=>{counter=counter.checked_add(1).ok_or_else(||issue("$","limit","OBS item IDs exhausted"))?;counter}};
            // Moving an item between containers can collide with another
            // original per-scene ID. Preserve unambiguous IDs; issue a fresh
            // local counter for the collision rather than exporting duplicates.
            used_ids.insert(id);
            let transform = item.transform();
            if audio_only && (*transform!=Transform::full(1,1)||item.compositing()!=ItemCompositing::default()) {
                return Err(issue("$.sources.settings.items","media_video_absent","Audio-only Media Sources cannot carry video filters or nonneutral video geometry"));
            }
            value.insert("id".into(), json!(id));
            value.insert("name".into(), json!(source.name()));
            value.insert("source_uuid".into(), json!(uuids[&source.id()]));
            value.insert("visible".into(), json!(item.visible()));
            value.insert("locked".into(), json!(item.locked()));
            value.insert("blend_type".into(),json!(item.compositing().blend_mode.obs_name()));
            value.insert("blend_method".into(),json!(item.compositing().blending_method.obs_name()));
            value.insert("scale_filter".into(),json!(item.compositing().scale_filter.obs_name()));
            item_color_tags::encode(&mut value,item.color_tag());
            value.insert("pos".into(), json!({"x":transform.x,"y":transform.y}));
            value.insert("rot".into(), json!(transform.rotation_degrees));
            value.insert("crop_left".into(), json!(transform.crop.left));
            value.insert("crop_top".into(), json!(transform.crop.top));
            value.insert("crop_right".into(), json!(transform.crop.right));
            value.insert("crop_bottom".into(), json!(transform.crop.bottom));
            if let Some((width,height))=dimensions {
                value.insert("scale".into(),json!({"x":f64::from(transform.width)/f64::from(width)*if transform.flip_x{-1.0}else{1.0},"y":f64::from(transform.height)/f64::from(height)*if transform.flip_y{-1.0}else{1.0}}));
            } else {
                // Audio-only items have no video transform. Emit the same neutral
                // inert fields accepted on import, never a fabricated pixel size.
                value.insert("pos".into(),json!({"x":0.0,"y":0.0}));value.insert("rot".into(),json!(0.0));
                for field in ["crop_left","crop_top","crop_right","crop_bottom"]{value.insert(field.into(),json!(0));}
                value.insert("scale".into(),json!({"x":1.0,"y":1.0}));
            }
            value.insert("align".into(), json!(5));
            items.push(Value::Object(value));
        }
        let mut settings = Map::new();
        settings.insert("custom_size".into(),json!(group));
        if group{let dimensions=collection.group_native_dimensions(scene.id()).map_err(|error|issue("$.groups.settings","group_canvas",error.to_string()))?.unwrap_or((0,0));settings.insert("cx".into(),json!(dimensions.0));settings.insert("cy".into(),json!(dimensions.1));}
        settings.insert("items".into(), json!(items));
        settings.insert("id_counter".into(), json!(counter));
        object.insert("settings".into(), Value::Object(settings));
        object.insert("canvas_uuid".into(),json!(MAIN_CANVAS_UUID));
        object.insert("id".into(), json!(if group{"group"}else{"scene"}));
        object.insert("versioned_id".into(), json!(if group{"group"}else{"scene"}));

        Ok(object)
}
