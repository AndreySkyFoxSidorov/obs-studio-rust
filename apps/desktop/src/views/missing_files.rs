//! Bounded staged paths. No filesystem work or canonical publication from egui.
use eframe::egui;
use skylive_core::{ResourceSlot,ResourceRelink,ResourceRelinks,MAX_RELINK_METADATA,MAX_RESOURCE_RELINKS};
use skylive_host::missing_files::{Choice,Page,State};
use super::Intent;
#[derive(Clone,Copy,Debug)]pub enum Action{Scan(usize),Select(ResourceSlot),Prepare,Clear,Unstage,Done,Cancel}
pub struct Ready{pub choice:Choice,pub prepared:ResourceRelink}
pub struct Panel{pub token:u64,pub session:u64,pub revision:u64,pub generation:u64,pub page:Option<Page>,pub selected:Option<ResourceSlot>,pub path:String,pub ready:Vec<Ready>,pub error:Option<String>,pub missing_only:bool}
impl Panel{
 pub fn new(token:u64,session:u64,revision:u64)->Self{Self{token,session,revision,generation:0,page:None,selected:None,path:String::new(),ready:Vec::new(),error:None,missing_only:true}}
 pub fn changed(&mut self)->Result<(),String>{self.generation=match self.generation.checked_add(1){Some(next)=>next,None=>{self.ready.clear();self.selected=None;return Err("Missing Files draft identity exhausted".into());}};if let Some(slot)=self.selected{self.ready.retain(|r|r.choice.slot!=slot);}self.error=None;Ok(())}
 pub fn select(&mut self,slot:ResourceSlot){if !self.page.as_ref().is_some_and(|p|p.rows.iter().any(|r|r.slot==slot)){return;}self.selected=Some(slot);self.path=self.ready.iter().find(|r|r.choice.slot==slot).map(|r|r.choice.path.clone().unwrap_or_default()).or_else(||self.page.as_ref()?.rows.iter().find(|r|r.slot==slot).map(|r|r.path.clone())).unwrap_or_default();self.error=None;}
 pub fn choice(&self)->Option<Choice>{Some(Choice{slot:self.selected?,path:Some(self.path.clone())})}
 pub fn retain(&mut self,choice:Choice,prepared:ResourceRelink)->Result<(),String>{let bytes=self.ready.iter().filter(|r|r.choice.slot!=choice.slot).fold(0usize,|n,r|n.saturating_add(r.prepared.metadata_bytes()).saturating_add(r.choice.path.as_ref().map_or(0,String::len)).saturating_add(std::mem::size_of::<Ready>()));if bytes.saturating_add(prepared.metadata_bytes()).saturating_add(choice.path.as_ref().map_or(0,String::len)).saturating_add(std::mem::size_of::<Ready>())>MAX_RELINK_METADATA||(!self.ready.iter().any(|r|r.choice.slot==choice.slot)&&self.ready.len()>=MAX_RESOURCE_RELINKS){return Err("Prepared replacements exceed 64 rows or 8 MiB; finish this batch before preparing more".into());}self.ready.retain(|r|r.choice.slot!=choice.slot);self.ready.push(Ready{choice,prepared});self.error=None;Ok(())}
 pub fn batch(&self)->ResourceRelinks{ResourceRelinks{rows:self.ready.iter().map(|r|r.prepared.clone()).collect()}}
 pub fn show(&mut self,ctx:&egui::Context,intents:&mut Vec<Intent>,busy:bool,fresh:bool){self.show_mode(ctx,intents,busy,fresh,None)}
 pub fn show_pending(&mut self,ctx:&egui::Context,intents:&mut Vec<Intent>,busy:bool,fresh:bool,label:&str){self.show_mode(ctx,intents,busy,fresh,Some(label))}
 fn show_mode(&mut self,ctx:&egui::Context,intents:&mut Vec<Intent>,busy:bool,fresh:bool,target:Option<&str>){let pending=target.is_some();let mut select=None;let mut edited=false;let event=|token,action|route(token,action,pending);
  let browser_html=self.selected.is_some_and(|slot|matches!(slot,ResourceSlot::BrowserHtml{..}));
  let text_file=self.selected.is_some_and(|slot|matches!(slot,ResourceSlot::TextFile{..}));
  let response=egui::Modal::new(egui::Id::new(("missing-files",pending,self.token))).show(ctx,|ui|{
   ui.set_width(960.0_f32.min((ctx.screen_rect().width()-48.0).max(180.0)));ui.heading(crate::i18n::tr(ui,"Missing Files"));if let Some(path)=target{let name=std::path::Path::new(path).file_name().and_then(|s|s.to_str()).unwrap_or(path);ui.label(name).on_hover_text(path);}
   ui.label(crate::i18n::tr(ui,if pending{"Replacements affect only the pending collection. Your current Program stays active until opening completes; Cancel discards every pending change."}else{"Choose replacements, prepare them, then commit all prepared rows with Done. Cancel keeps the project unchanged."}));
   ui.label(crate::i18n::tr(ui,"Presence is checked without decoding or validating existing file contents. File names are never substituted automatically."));
   if !fresh{ui.colored_label(egui::Color32::LIGHT_RED,crate::i18n::tr(ui,"The project changed. Cancel and reopen Missing Files before applying replacements."));}
   ui.checkbox(&mut self.missing_only,crate::i18n::tr(ui,"Show missing or unreadable paths only")).on_hover_text(crate::i18n::tr(ui,"Disable to inspect and explicitly replace an existing resource too."));
   egui::ScrollArea::both().id_salt("missing-files-table").max_height((ctx.screen_rect().height()-if browser_html||text_file{490.0}else{430.0}).max(70.0)).show(ui,|ui|{egui::Grid::new("missing-files-rows").striped(true).num_columns(4).show(ui,|ui|{
    for title in ["Source","Original path","State","Replacement"]{ui.strong(crate::i18n::tr(ui,title));}ui.end_row();
    if let Some(page)=&self.page{for row in &page.rows{if self.missing_only&&row.state==State::Present&&!self.ready.iter().any(|r|r.choice.slot==row.slot){continue;}let ready=self.ready.iter().find(|r|r.choice.slot==row.slot);
     if ui.add_enabled(!busy&&fresh,egui::Button::new(&row.owner).selected(self.selected==Some(row.slot))).on_hover_text(crate::i18n::tr(ui,"Select this exact source or filter resource; equal names do not merge rows.")).clicked(){select=Some(row.slot);}
     let label=std::path::Path::new(&row.path).file_name().and_then(|n|n.to_str()).unwrap_or(&row.path);ui.label(label).on_hover_text(&row.path);
     ui.label(crate::i18n::tr(ui,if ready.is_some(){"Prepared (not applied)"}else{row.state.label()})).on_hover_text(row.detail.as_deref().unwrap_or(&row.path));
     ui.label(ready.map(|r|r.choice.path.as_deref().unwrap_or("—")).unwrap_or("—"));ui.end_row();
    }}
   });});
   ui.horizontal_wrapped(|ui|{if let Some(page)=&self.page{if ui.add_enabled(!busy&&fresh&&page.start>0,egui::Button::new(crate::i18n::tr(ui,"Previous page"))).clicked(){intents.push(event(self.token,Action::Scan(page.start.saturating_sub(skylive_host::missing_files::PAGE_SLOTS))));}if ui.add_enabled(!busy&&fresh&&page.next.is_some(),egui::Button::new(crate::i18n::tr(ui,"Next page"))).clicked(){if let Some(next)=page.next{intents.push(event(self.token,Action::Scan(next)));}}ui.label(crate::i18n::tf(ui,"References: {count}; prepared: {ready}",&[("{count}",page.total.to_string()),("{ready}",self.ready.len().to_string())]));}if ui.add_enabled(!busy&&fresh,egui::Button::new(crate::i18n::tr(ui,"Refresh page"))).on_hover_text(crate::i18n::tr(ui,"Check this page again without changing any authored resource.")).clicked(){intents.push(event(self.token,Action::Scan(self.page.as_ref().map_or(0,|p|p.start))));}});
   ui.separator();ui.label(crate::i18n::tr(ui,"Replacement path"));
   if browser_html{ui.label(crate::i18n::tr(ui,"Local Browser HTML can be relinked without changing its CSS, viewport, frame rate or audio settings. The page and its linked resources are not parsed or started during preparation."));}
   if text_file{ui.label(crate::i18n::tr(ui,"Text replacement keeps the appearance and chat-log settings. Full text is limited to 1 MiB; chat-log mode reads the last 1 MiB. The renderer checks UTF-8 and text limits."));}
   edited=ui.add_enabled(!busy&&fresh&&self.selected.is_some(),egui::TextEdit::singleline(&mut self.path).desired_width(f32::INFINITY).char_limit(4096)).on_hover_text(crate::i18n::tr(ui,"Use an absolute file path, or an absolute folder path for a slideshow directory entry.")).changed();
   ui.horizontal_wrapped(|ui|{
    if let Some(intent)=crate::native_browse::button(ui,"Browse…",if pending{crate::native_browse::Target::PendingFiles}else{crate::native_browse::Target::MissingFiles},!busy&&fresh&&self.selected.is_some()){intents.push(intent);}
    if ui.add_enabled(!busy&&fresh&&self.selected.is_some()&&!self.path.is_empty(),egui::Button::new(crate::i18n::tr(ui,"Prepare replacement"))).on_hover_text(crate::i18n::tr(ui,if browser_html{"Check that this local HTML path is a regular readable file. HTML content is not parsed, executed or loaded into a browser."}else if text_file{"Check that this is a regular readable file. Text contents are read and validated during rendering."}else{"Read and validate this replacement on the file worker. The project is still unchanged."})).clicked(){intents.push(event(self.token,Action::Prepare));}
    if ui.add_enabled(!busy&&fresh&&self.selected.is_some_and(ResourceSlot::optional),egui::Button::new(crate::i18n::tr(ui,"Clear optional resource"))).on_hover_text(crate::i18n::tr(ui,"Stage an empty Image Mask or LUT resource. Required source paths cannot be cleared.")).clicked(){intents.push(event(self.token,Action::Clear));}
    if ui.add_enabled(!busy&&fresh&&self.selected.is_some_and(|slot|self.ready.iter().any(|r|r.choice.slot==slot)),egui::Button::new(crate::i18n::tr(ui,"Discard this prepared replacement"))).on_hover_text(crate::i18n::tr(ui,"Remove only this staged replacement; the project and other prepared rows stay unchanged.")).clicked(){intents.push(event(self.token,Action::Unstage));}
   });
   if busy{ui.spinner();ui.label(crate::i18n::tr(ui,"Preparing file operation…"));}if let Some(error)=&self.error{ui.colored_label(egui::Color32::LIGHT_RED,crate::i18n::tr(ui,error));}
   let buttons=[super::modal_footer::Action::new(if pending{"Continue opening"}else{"Done"},!busy&&fresh&&(pending||!self.ready.is_empty()),if pending{"Apply this batch only to the unpublished collection, check its remaining paths, and continue strict opening. The current project is retained."}else{"Apply every prepared replacement as one undoable project edit. No document file is saved."}),super::modal_footer::Action::new("Cancel",true,"Discard all unpublished replacements. An admitted file operation retires on its worker.")];if let Some(index)=super::modal_footer::show(ui,&buttons){intents.push(event(self.token,if index==0{Action::Done}else{Action::Cancel}));}
  });
  if edited{if let Err(e)=self.changed(){self.error=Some(e);}}if let Some(slot)=select{intents.push(event(self.token,Action::Select(slot)));}if response.should_close(){intents.push(event(self.token,Action::Cancel));}
 }
}

fn route(token:u64,action:Action,pending:bool)->Intent{if pending{Intent::RepairFiles(token,action)}else{Intent::MissingFiles(token,action)}}
