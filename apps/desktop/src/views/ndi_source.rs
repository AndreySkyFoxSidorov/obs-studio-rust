//! Discovery is explicit; Apply probes real incoming video before authoring.
use eframe::egui;
use skylive_core::{Command,Id,NdiBandwidth,NdiConnection,NdiSender,NdiSource};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum NdiEdit {Add {scene:Id},Rebind {source:Id}}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct NdiPrepare {pub token:u64,pub session:u64,pub revision:u64,pub edit:NdiEdit,pub name:String,pub connection:NdiConnection}
impl NdiPrepare {pub fn command(&self,ndi:NdiSource)->Command {match self.edit {NdiEdit::Add {scene}=>Command::AddNdiSource {scene,name:self.name.clone(),ndi},NdiEdit::Rebind {source}=>Command::SetNdiSource {source,ndi}}}}
pub struct NdiDraft {
    pub token:u64,pub session:u64,pub revision:u64,pub edit:NdiEdit,pub name:String,
    pub senders:Vec<NdiSender>,pub selected:Option<NdiSender>,pub bandwidth:NdiBandwidth,pub audio:bool,pub local:bool,
    pub loading:bool,pub error:Option<String>,pub basis:Option<(u32,u32)>,pub observed:Option<skylive_host::ndi::VideoMetadata>,
}
impl NdiDraft {
    pub fn add(scene:Id,name:String)->Self {Self {token:0,session:0,revision:0,edit:NdiEdit::Add {scene},name,senders:Vec::new(),selected:None,bandwidth:NdiBandwidth::Highest,audio:true,local:true,loading:false,error:None,basis:None,observed:None}}
    pub fn rebind(source:Id,ndi:&NdiSource)->Self {let mut draft=Self::add(source,String::new());draft.edit=NdiEdit::Rebind {source};draft.selected=Some(ndi.connection().sender.clone());draft.bandwidth=ndi.connection().bandwidth;draft.audio=ndi.audio();draft.basis=Some((ndi.width(),ndi.height()));draft}
    pub fn title(&self)->&'static str {if matches!(self.edit,NdiEdit::Add {..}){"Add NDI Source"}else{"Properties: NDI Source"}}
    pub fn request(&self)->Result<NdiPrepare,&'static str>{
        if self.loading{return Err("Wait for NDI discovery or source preparation to finish.".into());}
        if matches!(self.edit,NdiEdit::Add {..})&&(self.name.trim().is_empty()||self.name.len()>256||self.name.chars().any(char::is_control)){return Err("Source name requires 1..256 UTF-8 bytes without controls.".into());}
        let connection=NdiConnection {sender:self.selected.clone().ok_or("Search and choose an actual NDI sender first.")?,bandwidth:self.bandwidth,audio:self.audio};
        Ok(NdiPrepare {token:self.token,session:self.session,revision:self.revision,edit:self.edit,name:self.name.clone(),connection})
    }
    pub fn matches(&self,request:&NdiPrepare)->bool {self.token==request.token&&self.session==request.session&&self.revision==request.revision&&self.edit==request.edit&&self.name==request.name&&self.selected.as_ref()==Some(&request.connection.sender)&&self.bandwidth==request.connection.bandwidth&&self.audio==request.connection.audio}
}
pub fn editor(ui:&mut egui::Ui,draft:&mut NdiDraft,intents:&mut Vec<super::Intent>){
    ui.label(crate::i18n::tr(ui,"NDI receive uses the separately installed official NDI6 Runtime (Windows x64)."));
    ui.add_enabled_ui(!draft.loading,|ui|{
        if matches!(draft.edit,NdiEdit::Add {..}) {let label=ui.label(crate::i18n::tr(ui,"Name"));ui.add(egui::TextEdit::singleline(&mut draft.name).char_limit(256).desired_width(ui.available_width())).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Name this NDI source in the scene."));}
        ui.horizontal_wrapped(|ui|{
            ui.checkbox(&mut draft.local,crate::i18n::tr(ui,"Include local senders")).on_hover_text(crate::i18n::tr(ui,"Include NDI senders on this computer in the next search."));
            if ui.button(crate::i18n::tr(ui,"Search NDI senders" )).on_hover_text(crate::i18n::tr(ui,"Search the local NDI discovery network for up to one second. Nothing is installed or downloaded.")).clicked(){intents.push(super::Intent::DiscoverNdi {token:draft.token,session:draft.session,revision:draft.revision,local:draft.local});}
        });
        let label=ui.label(crate::i18n::tr(ui,"NDI sender"));
        egui::ComboBox::from_id_salt("ndi-sender").width(ui.available_width()).truncate().selected_text(draft.selected.as_ref().map_or_else(||crate::i18n::tr(ui,"Choose a sender"),|s|s.name.clone())).show_ui(ui,|ui|{
            for sender in &draft.senders {ui.selectable_value(&mut draft.selected,Some(sender.clone()),&sender.name).on_hover_text(sender.url.as_deref().unwrap_or(&sender.name));}
        }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Select a discovered sender. The saved sender remains selected while disconnected."));
        if let Some(selected)=&draft.selected {if !draft.senders.contains(selected){ui.small(crate::i18n::tr(ui,"This sender was not found in the latest search."));}}
        if draft.senders.is_empty(){ui.small(crate::i18n::tr(ui,"No senders listed. Search again after starting an NDI sender and checking the network."));}
        let label=ui.label(crate::i18n::tr(ui,"Bandwidth"));
        egui::ComboBox::from_id_salt("ndi-bandwidth").selected_text(crate::i18n::tr(ui,match draft.bandwidth {NdiBandwidth::Highest=>"Highest (full resolution)",NdiBandwidth::Lowest=>"Lowest (sender low bandwidth stream)"})).show_ui(ui,|ui|{
            ui.selectable_value(&mut draft.bandwidth,NdiBandwidth::Highest,crate::i18n::tr(ui,"Highest (full resolution)"));ui.selectable_value(&mut draft.bandwidth,NdiBandwidth::Lowest,crate::i18n::tr(ui,"Lowest (sender low bandwidth stream)"));
        }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Requests the SDK's actual high or low bandwidth stream; resolution and frame rate come from the sender."));
        ui.checkbox(&mut draft.audio,crate::i18n::tr(ui,"Receive audio")).on_hover_text(crate::i18n::tr(ui,"Route received PCM through this source's mixer, filters and explicitly selected monitoring mode."));
    });
    if let Some(video)=&draft.observed {ui.small(crate::i18n::tf(ui,"Received NDI video: {width} × {height}, {fps_num}/{fps_den} fps",&[("{width}",video.width.to_string()),("{height}",video.height.to_string()),("{fps_num}",video.fps_num.to_string()),("{fps_den}",video.fps_den.to_string())]));}
    if let Some((w,h))=draft.basis {ui.small(crate::i18n::tf(ui,"Last prepared NDI video size: {width} × {height}",&[("{width}",w.to_string()),("{height}",h.to_string())]));}
    ui.small(crate::i18n::tr(ui,"A new or changed sender requires an actual progressive RGB video frame. Existing sender settings can be applied offline; video resumes when the sender is available."));
    if draft.loading{ui.horizontal_wrapped(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,"Waiting for NDI discovery or video…"));});}
    if let Some(error)=&draft.error{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tf(ui,"NDI receive error: {error}",&[("{error}",error.clone())]));}
}
