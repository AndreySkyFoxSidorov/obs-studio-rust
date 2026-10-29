//! One URL-only widget reused by Stream Settings and Setup. No service I/O.
use eframe::egui;
use skylive_core::stream_service_presets::{self as catalog,ServicePreset};
pub struct Selection {service:Option<usize>,query:String,server_query:String}
impl Selection {
    pub fn from_url(url:&str)->Self {Self {service:catalog::services().iter().position(|s|s.servers.iter().any(|v|v.url==url)),query:String::new(),server_query:String::new()}}
    pub fn custom(&mut self){self.service=None;}
}
fn caption(value:&str)->String {if value.chars().count()<=48 {value.to_owned()}else{value.chars().take(47).chain(std::iter::once('…')).collect()}}
pub fn show(ui:&mut egui::Ui,selection:&mut Selection,url:&mut String){
    ui.push_id("stream-service-presets",|ui|{
        let selected=selection.service.and_then(|index|catalog::services().get(index));
        let label=ui.label(crate::i18n::tr(ui,"Streaming Service"));
        let heading=selected.map_or_else(||crate::i18n::tr(ui,"Custom RTMP / RTMPS"),|s|caption(s.name));
        let mut choice=None;
        egui::ComboBox::from_id_salt("service").width(ui.available_width()).height(280.0).selected_text(heading).show_ui(ui,|ui|{
            let search=ui.label(crate::i18n::tr(ui,"Search services"));
            ui.add(egui::TextEdit::singleline(&mut selection.query).char_limit(64).desired_width(ui.available_width())).labelled_by(search.id)
                .on_hover_text(crate::i18n::tr(ui,"Search historical service names, aliases or server locations. No network lookup is performed."));
            if ui.selectable_label(selected.is_none(),crate::i18n::tr(ui,"Custom RTMP / RTMPS"))
                .on_hover_text(crate::i18n::tr(ui,"Keep the current URL and edit it directly. The stream key and encoder settings are preserved.")).clicked(){choice=Some(None);ui.close_menu();}
            ui.separator();
            let query=selection.query.trim().to_lowercase();let mut found=false;
            for (index,service) in catalog::services().iter().enumerate().filter(|(_,s)|s.matches_folded(&query)) {
                found=true;
                if ui.add(egui::Button::new(service.name).selected(selection.service==Some(index)).truncate())
                    .on_hover_text(format!("{}\n{}",service.name,crate::i18n::tr(ui,"Choose a historical service address. Only the draft server URL changes; applying settings and starting the stream remain separate actions.")))
                    .clicked(){choice=Some(Some(index));ui.close_menu();}
            }
            if !found {ui.label(crate::i18n::tr(ui,"No matching services."));}
        }).response.labelled_by(label.id)
            .on_hover_text(selected.map_or_else(||crate::i18n::tr(ui,"Custom RTMP / RTMPS"),|s|s.name.to_owned()));
        if let Some(choice)=choice {
            selection.service=choice;selection.server_query.clear();
            if let Some(service)=choice.and_then(|index|catalog::services().get(index)) {
                if !service.servers.iter().any(|server|server.url==url.as_str()) {if let Some(server)=service.servers.first(){*url=server.url.to_owned();}}
            }
        }
        if let Some(service)=selection.service.and_then(|index|catalog::services().get(index)) {servers(ui,selection,service,url);}
        ui.small(crate::i18n::tr(ui,"Service presets come from OBS 32.2.2. Addresses are historical; availability and account access are not checked."));
        ui.small(crate::i18n::tr(ui,"Preset selection preserves the stream key, encoder, bitrates and keyframe interval. Service recommendations, OAuth and automatic ingest discovery are not applied."));
    });
}
fn servers(ui:&mut egui::Ui,selection:&mut Selection,service:&ServicePreset,url:&mut String){
    let current=service.servers.iter().find(|server|server.url==url.as_str());
    let label=ui.label(crate::i18n::tr(ui,"Streaming Server"));
    let heading=current.map_or_else(||crate::i18n::tr(ui,"Custom server URL"),|s|caption(s.name));
    egui::ComboBox::from_id_salt("server").width(ui.available_width()).height(280.0).selected_text(heading).show_ui(ui,|ui|{
        let search=ui.label(crate::i18n::tr(ui,"Search servers"));
        ui.add(egui::TextEdit::singleline(&mut selection.server_query).char_limit(64).desired_width(ui.available_width())).labelled_by(search.id)
            .on_hover_text(crate::i18n::tr(ui,"Filter the selected service's exact server names or URLs."));
        let query=selection.server_query.trim().to_lowercase();let mut found=false;
        for server in service.servers.iter().filter(|server|server.matches_folded(&query)){
            found=true;
            if ui.add(egui::Button::new(server.name).selected(server.url==url.as_str()).truncate()).on_hover_text(format!("{}\n{}\n{}",server.name,server.url,crate::i18n::tr(ui,"Use this exact historical ingest URL, including its application path and query. This does not connect or change the stream key.")))
                .clicked(){*url=server.url.to_owned();ui.close_menu();}
        }
        if !found {ui.label(crate::i18n::tr(ui,"No matching servers."));}
    }).response.labelled_by(label.id).on_hover_text(current.map_or(url.as_str(),|s|s.url));
}
