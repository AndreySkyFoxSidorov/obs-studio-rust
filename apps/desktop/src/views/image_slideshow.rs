//! Playlist properties are a staged draft; file preparation and commit are
//! coordinated by the app and the existing one-operation filesystem owner.
use super::Intent;
use eframe::egui;
use skylive_core::{Command,Id,ImageAsset,ImageSlideShow,Session,SlideTransition,SourceKind,MAX_SLIDES};
use skylive_core::{SlideShowEntry,SlideShowEntryKind};
#[derive(Clone,Copy,PartialEq,Eq)]
pub enum Edit {Add {scene:Id},Rebind {source:Id}}
pub struct SlideShowDraft {
    pub options:super::slideshow_options::Draft,
    pub edit:Edit,pub name:String,pub paths:Vec<String>,pub kinds:Vec<SlideShowEntryKind>,pub duration_ms:u32,
    pub transition:SlideTransition,pub transition_ms:u32,pub looping:bool,pub shuffle:bool,pub randomize:bool,pub random_compatibility:skylive_core::SlideShowRandomCompatibility,
    pub hide_when_done:bool,pub loading:bool,pub token:u64,
    session:u64,revision:u64,new_path:String,new_kind:SlideShowEntryKind,images:Vec<ImageAsset>,prepared_entries:Vec<SlideShowEntry>,prepared_counts:Vec<u16>,
}
#[derive(Clone)]
pub struct Submission {
    pub token:u64,pub session:u64,pub revision:u64,pub entries:Vec<SlideShowEntry>,
    edit:Edit,name:String,duration_ms:u32,transition:SlideTransition,transition_ms:u32,
    looping:bool,shuffle:bool,hide_when_done:bool,canvas:skylive_core::SlideShowCanvas,behavior:skylive_core::SlideShowBehavior,mode:skylive_core::SlideShowMode,randomize:bool,random_compatibility:skylive_core::SlideShowRandomCompatibility,
}
impl SlideShowDraft {
    pub fn add(scene:Id,name:String)->Self {Self {options:super::slideshow_options::Draft::modern(),edit:Edit::Add {scene},name,paths:vec![],kinds:vec![],duration_ms:2000,transition:SlideTransition::Fade,transition_ms:700,looping:true,shuffle:false,randomize:false,random_compatibility:skylive_core::SlideShowRandomCompatibility::BufferedV2,hide_when_done:false,loading:false,token:0,session:0,revision:0,new_path:String::new(),new_kind:SlideShowEntryKind::File,images:vec![],prepared_entries:vec![],prepared_counts:vec![]}}
    pub fn rebind(source:Id,settings:&ImageSlideShow)->Self {
        let mut draft=Self::add(source,String::new());draft.edit=Edit::Rebind {source};draft.options=super::slideshow_options::Draft::from_settings(settings);
        draft.prepared_entries=settings.authored_entries();draft.paths=draft.prepared_entries.iter().map(|entry|entry.path().to_owned()).collect();draft.kinds=draft.prepared_entries.iter().map(SlideShowEntry::kind).collect();draft.prepared_counts=if settings.entries().is_empty(){vec![1;draft.paths.len()]}else{settings.entry_image_counts().to_vec()};
        draft.images=settings.images().to_vec();draft.duration_ms=settings.duration_ms();draft.transition=settings.transition();draft.transition_ms=settings.transition_ms();draft.looping=settings.looping();draft.shuffle=settings.shuffle();draft.randomize=settings.randomize();draft.random_compatibility=settings.random_compatibility();draft.hide_when_done=settings.hide_when_done();draft
    }
    pub fn title(&self)->&'static str {match self.edit {Edit::Add {..}=>"Image Slide Show",Edit::Rebind {..}=>"Properties: Image Slide Show"}}
    pub fn bind(&mut self,session:&Session,token:u64)->Result<(),String>{
        if session.is_closed(){return Err("Slideshow document is closed".into());}
        let model=session.snapshot();
        match self.edit {
            Edit::Add {scene} if model.scene(scene).is_none()=>return Err("Slideshow target scene is unavailable".into()),
            Edit::Rebind {source} if !model.source(source).is_some_and(|s|matches!(s.kind(),SourceKind::ImageSlideShow {..}))=>return Err("Slideshow source is unavailable".into()),
            _=>{},
        }
        self.session=session.identity();self.revision=model.revision();self.token=token;Ok(())
    }
    pub fn current(&self,session:&Session)->bool {!session.is_closed()&&self.session==session.identity()&&self.revision==session.snapshot().revision()}
    pub fn request(&self)->Result<Submission,String>{
        if self.loading{return Err("Slideshow image preparation is already active".into());}
        ImageSlideShow::validate_timing(self.duration_ms,self.transition,self.transition_ms).map_err(|e|e.to_string())?;
        let random_compatibility=if self.randomize{self.random_compatibility}else{skylive_core::SlideShowRandomCompatibility::BufferedV2};
        ImageSlideShow::validate_randomization(self.shuffle,self.randomize,random_compatibility).map_err(|e|e.to_string())?;
        let canvas=self.options.canvas()?;
        if self.paths.len()>MAX_SLIDES{return Err("Select at most 256 file or folder entries".into());}
        if self.kinds.len()!=self.paths.len(){return Err("Slideshow entry kinds are incomplete".into());}
        let entries=self.paths.iter().zip(&self.kinds).map(|(path,kind)|SlideShowEntry::new(*kind,path.clone()).map_err(|error|error.to_string())).collect::<Result<Vec<_>,_>>()?;
        if self.paths.iter().any(|p|p.trim().is_empty()||p.len()>4096||p.chars().any(char::is_control)){return Err("Each image needs a valid path of 1..=4096 bytes without controls".into());}
        Ok(Submission {token:self.token,session:self.session,revision:self.revision,entries,edit:self.edit,name:self.name.clone(),duration_ms:self.duration_ms,transition:self.transition,transition_ms:self.transition_ms,looping:self.looping,shuffle:self.shuffle,hide_when_done:self.hide_when_done,canvas,behavior:self.options.behavior,mode:self.options.playback_mode,randomize:self.randomize,random_compatibility})
    }
}
impl Submission {
    pub fn matches(&self,draft:&SlideShowDraft)->bool{
        let compatibility=if draft.randomize{draft.random_compatibility}else{skylive_core::SlideShowRandomCompatibility::BufferedV2};
        self.token==draft.token&&self.session==draft.session&&self.revision==draft.revision
            &&self.edit==draft.edit&&self.name==draft.name&&self.duration_ms==draft.duration_ms
            &&self.transition==draft.transition&&self.transition_ms==draft.transition_ms
            &&self.looping==draft.looping&&self.shuffle==draft.shuffle&&self.hide_when_done==draft.hide_when_done
            &&Some(self.canvas)==draft.options.canvas().ok()&&self.behavior==draft.options.behavior
            &&self.mode==draft.options.playback_mode&&self.randomize==draft.randomize&&self.random_compatibility==compatibility
            &&self.entries.len()==draft.paths.len()&&draft.kinds.len()==draft.paths.len()
            &&self.entries.iter().zip(draft.paths.iter().zip(&draft.kinds)).all(|(entry,(path,kind))|entry.path()==path&&entry.kind()==*kind)
    }
    pub fn command(self,session:&Session,prepared:skylive_host::image_slideshow::directories::Prepared)->Result<(Command,Option<Id>),String>{
        if session.is_closed()||self.session!=session.identity()||self.revision!=session.snapshot().revision(){return Err("Slideshow preparation is stale; current source was retained".into());}
        if prepared.inputs!=self.entries{return Err("Slideshow preparation does not match the captured file and folder entries".into());}
        let skylive_host::image_slideshow::directories::Prepared{images,entries,counts,..}=prepared;
        let settings=ImageSlideShow::new_with_options(images,self.duration_ms,self.transition,self.transition_ms,self.looping,self.shuffle,self.hide_when_done,self.canvas,self.behavior).and_then(|settings|settings.with_mode(self.mode)).and_then(|settings|settings.with_randomization(self.randomize,self.random_compatibility)).and_then(|settings|settings.with_entries(entries,counts)).map_err(|e|e.to_string())?;
        Ok(match self.edit {Edit::Add {scene}=>(Command::AddImageSlideShow {scene,name:self.name,settings},None),Edit::Rebind {source}=>(Command::SetImageSlideShow {source,settings},Some(source))})
    }
}
pub fn editor(ui:&mut egui::Ui,draft:&mut SlideShowDraft,intents:&mut Vec<Intent>){
    if matches!(draft.edit,Edit::Add {..}) {
        ui.label(crate::i18n::tr(ui,"Name"));ui.add(egui::TextEdit::singleline(&mut draft.name).char_limit(256)).on_hover_text(crate::i18n::tr(ui,"Name of this image slideshow source"));
    }
    draft.kinds.resize(draft.paths.len(),SlideShowEntryKind::File);
    ui.label(crate::i18n::tr(ui,"Image Files and Folders"));
    let count=draft.paths.len();
    let mut remove=None;let mut movement=None;
    let drag_id=ui.make_persistent_id(("slideshow-entry-drag",draft.token));
    let mut row_rects=Vec::with_capacity(count);
    egui::ScrollArea::vertical().id_salt("slideshow-images").max_height(230.0).show(ui,|ui| {
        for (index,path) in draft.paths.iter_mut().enumerate(){
            ui.push_id(index,|ui| {let row=ui.horizontal(|ui| {
                let drag=ui.add(egui::Label::new("⠿").sense(egui::Sense::drag()))
                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Slideshow.DragReorder","Drag to reorder this file or folder"));
                if drag.drag_started(){ui.ctx().data_mut(|data|data.insert_temp(drag_id,index));}
                ui.label(format!("{}",index+1));
                egui::ComboBox::from_id_salt(("slideshow-entry-kind",index)).selected_text(crate::i18n::tr(ui,if draft.kinds[index]==SlideShowEntryKind::File{"File"}else{"Folder"})).show_ui(ui,|ui|{for(kind,label)in[(SlideShowEntryKind::File,"File"),(SlideShowEntryKind::Directory,"Folder")]{ui.selectable_value(&mut draft.kinds[index],kind,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"A folder contributes supported images from its immediate children, in native enumeration order. Subfolders are not included."));}}).response.on_hover_text(crate::i18n::tr(ui,"Choose File or Folder; Apply validates the actual path type on the background owner."));
                ui.add_sized([ui.available_width().max(150.0)-120.0,24.0],egui::TextEdit::singleline(path).char_limit(4096)).on_hover_text(crate::i18n::tr(ui,"Exact file or folder path; Apply enumerates folders and prepares every resolved image"));
                if ui.add_enabled(index>0,egui::Button::new("↑")).on_hover_text(crate::i18n::tr(ui,"Move entry earlier")).on_disabled_hover_text(crate::i18n::tr(ui,"This image is first")).clicked(){movement=Some((index,index-1));}
                if ui.add_enabled(index+1<count,egui::Button::new("↓")).on_hover_text(crate::i18n::tr(ui,"Move entry later")).on_disabled_hover_text(crate::i18n::tr(ui,"This image is last")).clicked(){movement=Some((index,index+1));}
                if ui.button("×").on_hover_text(crate::i18n::tr(ui,"Remove this file or folder entry from the draft playlist")).clicked(){remove=Some(index);}
            });row_rects.push(row.response.rect.intersect(ui.clip_rect()));
            if let Some(image)=draft.images.iter().find(|image|image.path()==path.as_str()){ui.small(format!("{} × {}",image.width(),image.height()));}
            });
        }
    });
    if let Some(index)=remove{draft.paths.remove(index);draft.kinds.remove(index);}
    if let Some((from,to))=movement.filter(|(from,to)|*from<draft.paths.len()&&*to<draft.paths.len()){draft.paths.swap(from,to);draft.kinds.swap(from,to);}
    if ui.input(|input|input.pointer.primary_released()) {
        let from=ui.ctx().data_mut(|data|data.remove_temp::<usize>(drag_id));
        let position=ui.input(|input|input.pointer.interact_pos());
        if remove.is_none()&&movement.is_none(){
            if let (Some(from),Some(position))=(from,position){
                if let Some(to)=row_rects.iter().position(|rect|rect.contains(position)){
                    if from<draft.paths.len()&&to<draft.paths.len()&&from!=to {
                        let path=draft.paths.remove(from);let kind=draft.kinds.remove(from);
                        draft.paths.insert(to,path);draft.kinds.insert(to,kind);
                    }
                }
            }
        }
    }
    ui.horizontal_wrapped(|ui| {
        for(kind,label)in[(SlideShowEntryKind::File,"File"),(SlideShowEntryKind::Directory,"Folder")]{ui.selectable_value(&mut draft.new_kind,kind,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"Select the type of the entry to add; no filesystem access occurs until Apply."));}
        ui.add(egui::TextEdit::singleline(&mut draft.new_path).char_limit(4096).hint_text(crate::i18n::tr(ui,"Image file or folder path"))).on_hover_text(crate::i18n::tr(ui,"Enter a supported image file or a folder path; folders are not scanned recursively"));
        if ui.add_enabled(draft.paths.len()<MAX_SLIDES&&!draft.new_path.trim().is_empty(),egui::Button::new(crate::i18n::tr(ui,"Add"))).on_hover_text(crate::i18n::tr(ui,"Append this typed file or folder entry to the draft playlist")).on_disabled_hover_text(crate::i18n::tr(ui,"Enter a path; maximum 256 entries and 256 resolved images")).clicked(){draft.paths.push(std::mem::take(&mut draft.new_path));draft.kinds.push(draft.new_kind);}
    });
    let prepared_current=draft.paths.len()==draft.prepared_entries.len()&&draft.paths.iter().zip(&draft.kinds).zip(&draft.prepared_entries).all(|((path,kind),entry)|path==entry.path()&&*kind==entry.kind());
    if prepared_current&&!draft.images.is_empty(){ui.small(crate::i18n::tf(ui,"Prepared playlist: {images} images from {entries} file/folder entries",&[("{images}",draft.images.len().to_string()),("{entries}",draft.prepared_counts.len().to_string())]));}
    ui.small(crate::i18n::tr_key_or(ui,"SkyLive.Slideshow.AutoFolderRefresh","Folder contents are checked in the background and refreshed when they change. Close this window to apply automatic updates."));
    let raw=if prepared_current&&!draft.images.is_empty(){Some((draft.images.iter().map(ImageAsset::width).max().unwrap_or(1),draft.images.iter().map(ImageAsset::height).max().unwrap_or(1)))}else{None};
    super::slideshow_options::editor(ui,&mut draft.options,raw);
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(crate::i18n::tr(ui,"Transition"));
        for transition in SlideTransition::ALL{let hint=match transition{SlideTransition::Cut=>"Switch images immediately",SlideTransition::Fade=>"Crossfade images with their transparency preserved",SlideTransition::Swipe=>"Wipe the outgoing image to the left to reveal the next image, using the stock slideshow Swipe transition.",SlideTransition::Slide=>"Slide both images to the left using the stock slideshow transition and easing."};ui.selectable_value(&mut draft.transition,transition,crate::i18n::tr(ui,transition.label())).on_hover_text(crate::i18n::tr(ui,hint));}
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(crate::i18n::tr(ui,"Time Between Slides"));
        let min=if draft.transition==SlideTransition::Cut {50}else{100};draft.duration_ms=draft.duration_ms.max(min);
        ui.add(egui::DragValue::new(&mut draft.duration_ms).range(min..=3_600_000).suffix(" ms")).on_hover_text(crate::i18n::tr(ui,"Visible hold time. Fade, Swipe and Slide add their transition duration to the interval; subsequent transitions begin at the interval boundary."));
        ui.add_enabled_ui(draft.transition!=SlideTransition::Cut,|ui|{let label=ui.label(crate::i18n::tr(ui,"Transition Speed"));ui.add(egui::DragValue::new(&mut draft.transition_ms).range(0..=3_600_000).suffix(" ms")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Transition duration from 0 to 3600000 milliseconds. Zero switches immediately; Cut retains this value for the next animated transition.")).on_disabled_hover_text(crate::i18n::tr(ui,"Cut switches immediately. The saved transition speed is retained for Fade, Swipe or Slide."));});
    });
    ui.horizontal_wrapped(|ui| {
        let modern_random=draft.randomize&&draft.random_compatibility==skylive_core::SlideShowRandomCompatibility::BufferedV2;
        ui.add_enabled(!modern_random,egui::Checkbox::new(&mut draft.looping,crate::i18n::tr(ui,"Loop"))).on_hover_text(crate::i18n::tr(ui,"Restart the playlist after its last image")).on_disabled_hover_text(crate::i18n::tr(ui,"Modern random playback continues indefinitely; Loop and Hide are retained but not used."));
        let mut order=if draft.randomize{2}else if draft.shuffle{1}else{0};let label=ui.label(crate::i18n::tr(ui,"Slideshow Order"));egui::ComboBox::from_id_salt("slideshow-order").selected_text(crate::i18n::tr(ui,match order{2=>"Randomize Playback",1=>"Stable shuffled order",_=>"In file order"})).show_ui(ui,|ui|{for(value,text,hint)in[(0,"In file order","Advance through the explicit image list."),(1,"Stable shuffled order","Use the existing deterministic permutation; each image appears once per loop."),(2,"Randomize Playback","Use source-owned random choices without adjacent repeats. Modern playback retains five previous and five next indices; it ignores Loop and Hide.")]{ui.selectable_value(&mut order,value,crate::i18n::tr(ui,text)).on_hover_text(crate::i18n::tr(ui,hint));}}).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Random playback and the saved stable permutation are distinct. Changing order requires Apply; transport and render never reroll the setting."));draft.randomize=order==2;draft.shuffle=order==1;
        if draft.randomize&&draft.random_compatibility==skylive_core::SlideShowRandomCompatibility::LegacyV1{ui.small(crate::i18n::tr(ui,"Legacy OBS random compatibility: Previous rerolls; without Loop, the final file index can end playback. The imported policy is retained."));}
        ui.add_enabled(!draft.looping&&!modern_random,egui::Checkbox::new(&mut draft.hide_when_done,crate::i18n::tr(ui,"Hide when slideshow is done"))).on_hover_text(crate::i18n::tr(ui,"After the last interval, use the selected transition to become transparent. Cut or zero speed hides immediately.")).on_disabled_hover_text(crate::i18n::tr(ui,if modern_random{"Modern random playback continues indefinitely; Loop and Hide are retained but not used."}else{"Disable Loop to use this option"}));
    });
    if ui.add_enabled(!draft.loading,egui::Button::new(crate::i18n::tr(ui,"Refresh and Apply Playlist"))).on_hover_text(crate::i18n::tr(ui,"Prepare this captured draft again, including changed folder contents, and apply it atomically. Cancel or a stale receipt keeps the current source.")).on_disabled_hover_text(crate::i18n::tr(ui,"Wait for the current slideshow preparation to retire.")).clicked(){match draft.request(){Ok(request)=>intents.push(Intent::PrepareSlideShow(request)),Err(error)=>intents.push(Intent::DialogError(crate::i18n::tr(ui,error)))}}
    ui.small(crate::i18n::tr(ui,"Images use centered aspect fit in the selected canvas. Playback follows the selected visibility behavior. Use the source playback controls to change slides or playback state. Folders expand their immediate supported files when applied or loaded."));
}
