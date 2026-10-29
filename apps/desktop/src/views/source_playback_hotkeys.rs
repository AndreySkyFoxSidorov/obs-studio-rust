//! Source-bound shortcut draft. Never registers keys or changes playback.
use eframe::egui;
use skylive_core::{Id,SourceKind,StudioSnapshot,SourcePlaybackAction as Operation,SourcePlaybackBinding,SourcePlaybackHotkeys};
use std::sync::atomic::{AtomicU64,Ordering};
static NEXT:AtomicU64=AtomicU64::new(1);
pub struct Draft{
    pub session:u64,pub revision:u64,pub source:Id,pub token:u64,pub name:String,
    baseline:SourcePlaybackHotkeys,bindings:Vec<SourcePlaybackBinding>,slideshow:bool,
    listening:Option<(Operation,usize)>,pub capture_ready:bool,pub available:bool,
    pub current:bool,pub status:String,pub detail:Option<String>,error:Option<String>,
}
impl Draft{
    pub fn capture(session:u64,model:&StudioSnapshot,source:Id)->Result<Self,String>{
        let value=model.source(source).filter(|value|eligible(value.kind())).ok_or("Choose a Media Source or Slideshow for playback shortcuts.")?;
        let token=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|value|value.checked_add(1)).map_err(|_|"Source shortcut editor identity exhausted.")?;
        Ok(Self{session,revision:model.revision(),source,token,name:value.name().to_owned(),baseline:value.playback_hotkeys().clone(),bindings:value.playback_hotkeys().bindings().to_vec(),slideshow:matches!(value.kind(),SourceKind::ImageSlideShow{..}),listening:None,capture_ready:false,available:false,current:true,status:"Waiting for global shortcut suspension…".into(),detail:None,error:None})
    }
    pub fn fresh(&self,session:u64,model:&StudioSnapshot)->bool{
        self.session==session&&self.revision==model.revision()&&self.token!=0
            &&model.source(self.source).is_some_and(|source|source.playback_hotkeys()==&self.baseline&&eligible(source.kind())&&matches!(source.kind(),SourceKind::ImageSlideShow{..})==self.slideshow)
    }
    pub fn is_listening(&self)->bool{self.listening.is_some()}
    pub fn prepared(&self,model:&StudioSnapshot)->Result<SourcePlaybackHotkeys,String>{
        if !self.current||!self.fresh(self.session,model){return Err("This source shortcut draft is obsolete. Cancel and reopen it.".into());}
        if self.is_listening(){return Err("Finish or cancel the shortcut capture before applying.".into());}
        let bindings=SourcePlaybackHotkeys::new(self.bindings.clone()).map_err(|error|error.to_string())?;
        model.check_source_playback_hotkeys(self.source,&bindings).map_err(|error|error.to_string())?;Ok(bindings)
    }
    pub fn intent(&self,model:&StudioSnapshot)->Result<super::Intent,String>{Ok(super::Intent::ApplySourcePlaybackHotkeys{session:self.session,revision:self.revision,source:self.source,token:self.token,bindings:self.prepared(model)?})}
    fn assign(&mut self,operation:Operation,slot:usize,chord:skylive_core::HotkeyChord)->Result<(),String>{
        let mut candidate=self.bindings.clone();
        if let Some(index)=candidate.iter().enumerate().filter(|(_,binding)|binding.operation==operation).nth(slot).map(|(index,_)|index){candidate.remove(index);}
        candidate.push(SourcePlaybackBinding{operation,chord});
        let candidate=SourcePlaybackHotkeys::new(candidate).map_err(|error|error.to_string())?;self.bindings=candidate.bindings().to_vec();Ok(())
    }
}
pub fn eligible(kind:&SourceKind)->bool{matches!(kind,SourceKind::ImageSlideShow{..}|SourceKind::MediaSource{..})}
pub fn open_button(ui:&mut egui::Ui,session:u64,model:&StudioSnapshot,source:Id,intents:&mut Vec<super::Intent>){
    if !model.source(source).is_some_and(|source|eligible(source.kind())){return;}
    if ui.button(crate::i18n::tr(ui,"Playback Hotkeys…")).on_hover_text(crate::i18n::tr(ui,"Assign Windows global playback shortcuts to this exact source, independently of the selected scene item.")).clicked(){
        match Draft::capture(session,model,source){Ok(draft)=>intents.push(super::Intent::OpenDialog(super::dialogs::Dialog::SourcePlaybackHotkeys(draft))),Err(error)=>intents.push(super::Intent::DialogError(error))};ui.close_menu();
    }
}
pub fn settings_sources(ui:&mut egui::Ui,model:&StudioSnapshot,scope:Option<(u64,u64)>,enabled:bool,intents:&mut Vec<super::Intent>){
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Source Playback Shortcuts")).show(ui,|ui|{
        ui.label(crate::i18n::tr(ui,"Apply or Cancel application shortcut changes before opening a source editor."));
        ui.add_enabled_ui(enabled&&scope.is_some_and(|(_,revision)|revision==model.revision()),|ui|{
            egui::ScrollArea::vertical().id_salt("source-shortcut-targets").max_height(180.0).show(ui,|ui|{
                let mut found=false;
                for source in model.sources().iter().filter(|source|eligible(source.kind())){found=true;ui.push_id(source.id(),|ui|{ui.horizontal_wrapped(|ui|{ui.label(source.name()).on_hover_text(source.name());open_button(ui,scope.map_or(0,|scope|scope.0),model,source.id(),intents);});});}
                if !found{ui.label(crate::i18n::tr(ui,"Add a Media Source or Slideshow to configure source playback shortcuts."));}
            });
        });
    }).header_response.on_hover_text(crate::i18n::tr(ui,"Assign Windows global playback shortcuts to this exact source, independently of the selected scene item."));
}
pub fn editor(ui:&mut egui::Ui,model:&StudioSnapshot,draft:&mut Draft){
    ui.label(crate::i18n::tf(ui,"Source: {name}",&[("{name}",draft.name.clone())]));
    ui.label(crate::i18n::tr(ui,"These Windows global shortcuts always address this source. They also work while another app has focus."));
    ui.label(crate::i18n::tr(ui,&draft.status));if let Some(detail)=&draft.detail{ui.add(egui::Label::new(crate::i18n::tr(ui,detail)).wrap());}
    if !draft.available{ui.label(crate::i18n::tr(ui,"Global shortcuts are unavailable on this platform. Saved assignments are retained."));}
    let fresh=draft.current&&draft.fresh(draft.session,model);
    if !fresh{draft.listening=None;}
    if !fresh{ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This source shortcut draft is obsolete. Cancel and reopen it."));}
    if let Some((operation,slot))=draft.listening{
        let pressed=ui.input_mut(|input|{let index=input.events.iter().position(|event|matches!(event,egui::Event::Key{pressed:true,repeat:false,key,..}if *key==egui::Key::Escape||draft.capture_ready))?;match input.events.remove(index){egui::Event::Key{key,modifiers,..}=>Some((key,modifiers)),_=>None}});
        if let Some((key,modifiers))=pressed{if key==egui::Key::Escape{draft.listening=None;draft.error=None;}else{match crate::shortcuts::chord(key,modifiers).and_then(|chord|draft.assign(operation,slot,chord)){Ok(())=>{draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error)}}}
    }
    if fresh && draft.available && draft.capture_ready {
        if let Some((operation,slot))=draft.listening {
            if let Some(chord)=super::hotkeys::keypad_picker(ui) {
                match draft.assign(operation,slot,chord){Ok(())=>{draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error)}
            }
        }
    }
    if let Some(error)=&draft.error{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
    if !draft.is_listening(){if let Err(error)=draft.prepared(model){ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,error)).color(ui.visuals().warn_fg_color)).wrap());}}
    let operations:&[Operation]=if draft.slideshow{&Operation::SLIDESHOW}else{&Operation::MEDIA};
    for &operation in operations{ui.push_id(operation,|ui|{
        if ui.available_width()>=580.0{
            ui.horizontal(|ui|{
                let label=ui.add_sized([150.0,28.0],egui::Label::new(crate::i18n::tr(ui,operation.label())).truncate()).on_hover_text(crate::i18n::tr(ui,operation.label()));
                for slot in 0..2{binding_slot(ui,draft,operation,slot,fresh,label.id);}
            });
        }else{
            ui.group(|ui|{
                let label=ui.label(crate::i18n::tr(ui,operation.label())).on_hover_text(crate::i18n::tr(ui,operation.label()));
                for slot in 0..2{ui.horizontal(|ui|binding_slot(ui,draft,operation,slot,fresh,label.id));}
            });
        }
    });}
    ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Hotkeys.WindowsLimits","Ctrl, Alt and Shift are supported. Windows-key combinations and punctuation are unavailable."));
    ui.label(crate::i18n::tr(ui,"Apply saves these source bindings; Cancel leaves them unchanged. Playback position is never saved by this editor."));
}

fn binding_slot(ui:&mut egui::Ui,draft:&mut Draft,operation:Operation,slot:usize,fresh:bool,label:egui::Id){
    ui.push_id(slot,|ui|{
        let binding=draft.bindings.iter().filter(|binding|binding.operation==operation).nth(slot);let assigned=binding.is_some();
        let caption=if draft.listening==Some((operation,slot)){crate::i18n::tr(ui,"Press a shortcut…")}else{binding.map(|binding|super::hotkeys::global_label(ui,&binding.chord)).unwrap_or_else(||crate::i18n::tr(ui,"Unassigned"))};
        let response=ui.add_enabled_ui(fresh&&draft.available&&draft.capture_ready,|ui|ui.add_sized([140.0,28.0],egui::Button::new(&caption).truncate().selected(draft.listening==Some((operation,slot))))).inner.labelled_by(label)
            .on_hover_text(crate::i18n::tr(ui,"Assign a shortcut to this action. Escape cancels capture.")).on_hover_text(&caption)
            .on_disabled_hover_text(crate::i18n::tr(ui,"Wait until configured shortcut input is suspended before capturing a shortcut."));
        if response.clicked(){draft.listening=Some((operation,slot));draft.error=None;response.request_focus();}
        if ui.add_enabled_ui(fresh&&assigned,|ui|ui.add_sized([56.0,28.0],egui::Button::new(crate::i18n::tr(ui,"Clear")).truncate())).inner.labelled_by(label)
            .on_hover_text(crate::i18n::tr(ui,"Remove this shortcut assignment.")).on_disabled_hover_text(crate::i18n::tr(ui,"Remove this shortcut assignment.")).clicked(){
            if let Some(index)=draft.bindings.iter().enumerate().filter(|(_,binding)|binding.operation==operation).nth(slot).map(|(index,_)|index){draft.bindings.remove(index);}
            draft.listening=None;draft.error=None;
        }
    });
}
