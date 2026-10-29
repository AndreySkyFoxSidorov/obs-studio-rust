//! Presentation only. Native process scheduling is owned by host, not egui.
use eframe::egui;
use skylive_core::{ProcessPriority,Command,StudioSnapshot,ProfileId};
#[derive(Clone)]pub struct Draft{pub priority:ProcessPriority,pub profile:ProfileId,pub profile_name:String,pub reconnect:super::stream_reconnect::Draft,pub busy:bool,pub last:Option<skylive_host::process_priority::Receipt>}
impl Draft{
    pub fn new(model:&StudioSnapshot)->Self{Self{priority:model.preferences().process_priority(),profile:model.profiles().selected_id(),profile_name:model.profiles().selected().name().into(),reconnect:super::stream_reconnect::Draft::new(model.profiles().selected().streaming().reconnect()),busy:false,last:None}}
    pub fn unchanged(&self,model:&StudioSnapshot)->bool{self.priority==model.preferences().process_priority()&&self.profile==model.profiles().selected_id()&&self.reconnect.unchanged(model.profiles().selected().streaming().reconnect())}
    pub fn command(&self)->Result<Command,String>{if self.busy{return Err("Wait for the process priority result.".into());}Ok(Command::ConfigureAdvancedSettings{profile:self.profile,priority:self.priority,reconnect:self.reconnect.settings()?})}
}
pub fn label(priority:ProcessPriority)->&'static str{match priority{ProcessPriority::High=>"High",ProcessPriority::AboveNormal=>"Above Normal",ProcessPriority::Normal=>"Normal",ProcessPriority::BelowNormal=>"Below Normal",ProcessPriority::Idle=>"Idle"}}
fn priority_text(ui:&egui::Ui,priority:ProcessPriority)->String{let key=match priority{ProcessPriority::High=>"Basic.Settings.Advanced.General.ProcessPriority.High",ProcessPriority::AboveNormal=>"Basic.Settings.Advanced.General.ProcessPriority.AboveNormal",ProcessPriority::Normal=>"Basic.Settings.Advanced.General.ProcessPriority.Normal",ProcessPriority::BelowNormal=>"Basic.Settings.Advanced.General.ProcessPriority.BelowNormal",ProcessPriority::Idle=>"Basic.Settings.Advanced.General.ProcessPriority.Idle"};crate::i18n::tr_key_or(ui,key,label(priority))}
fn class_label(ui:&egui::Ui,value:u32)->String{let priority=match value{128=>Some(ProcessPriority::High),32768=>Some(ProcessPriority::AboveNormal),32=>Some(ProcessPriority::Normal),16384=>Some(ProcessPriority::BelowNormal),64=>Some(ProcessPriority::Idle),_=>None};if let Some(priority)=priority{priority_text(ui,priority)}else if value==256{crate::i18n::tr(ui,"Realtime (external)")}else{value.to_string()}}
pub fn show(ui:&mut egui::Ui,draft:&mut Draft){
    ui.strong(crate::i18n::tr(ui,"General"));let title=ui.label(crate::i18n::tr_key_or(ui,"Basic.Settings.Advanced.General.ProcessPriority","Process Priority"));
    ui.add_enabled_ui(skylive_host::process_priority::available()&&!draft.busy,|ui|{
        egui::ComboBox::from_id_salt("advanced-process-priority").selected_text(priority_text(ui,draft.priority)).show_ui(ui,|ui|{
            for priority in [ProcessPriority::High,ProcessPriority::AboveNormal,ProcessPriority::Normal,ProcessPriority::BelowNormal,ProcessPriority::Idle]{ui.selectable_value(&mut draft.priority,priority,priority_text(ui,priority)).on_hover_text(crate::i18n::tr(ui,"Choose the Windows scheduling priority for this SkyLive process. Apply requests a real change and readback; opening this page changes nothing."));}
        }).response.labelled_by(title.id).on_hover_text(crate::i18n::tr(ui,"Choose the Windows scheduling priority for this SkyLive process. Apply requests a real change and readback; opening this page changes nothing."));
    }).response.on_disabled_hover_text(crate::i18n::tr(ui,if draft.busy{"Wait for the process priority result."}else{"Process priority control is available only on Windows."}));
    ui.label(crate::i18n::tr(ui,"Normal is the default. A saved nondefault priority is applied once after preferences load on the next normal startup."));
    if draft.busy{ui.label(crate::i18n::tr(ui,"Applying process priority…"));}
    if let Some(receipt)=&draft.last{
        if let Some(value)=receipt.readback{ui.label(crate::i18n::tf(ui,"Last priority readback: {priority} (class {class})",&[("{priority}",class_label(ui,value)),("{class}",value.to_string())])).on_hover_text(crate::i18n::tr(ui,"This is the last confirmed Windows readback, not a continuous query. Another program may change the priority later."));}
        if let Some(error)=&receipt.error{ui.colored_label(egui::Color32::LIGHT_RED,error);}
    }else{ui.label(crate::i18n::tr(ui,"No priority request has completed in this session."));}
    ui.small(crate::i18n::tr(ui,"Closing restores the captured original priority only while our applied class is still present. An external change is kept. Process priority is separate from scene Undo."));
    ui.label(crate::i18n::tf(ui,"Profile: {name}",&[("{name}",draft.profile_name.clone())]));
    ui.add_enabled_ui(!draft.busy,|ui|super::stream_reconnect::show(ui,&mut draft.reconnect));
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Additional Advanced settings")).show(ui,|ui|{ui.label(crate::i18n::tr(ui,"Other Advanced sections have not been ported. No inactive options are applied by this page."));});
}
