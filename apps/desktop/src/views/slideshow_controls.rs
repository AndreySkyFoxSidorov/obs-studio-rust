//! Metadata-only source transport actions shared by toolbar and context menu.
use eframe::egui;
use skylive_core::{Id,SourceKind,StudioSnapshot};
use skylive_host::image_slideshow::transport::{Control,Target};
use super::Intent;
pub fn show(ui:&mut egui::Ui,session:u64,model:&StudioSnapshot,source:Id,intents:&mut Vec<Intent>){
    let Some(SourceKind::ImageSlideShow{settings})=model.source(source).map(|s|s.kind())else{return;};
    ui.horizontal_wrapped(|ui|buttons(ui,session,model.revision(),source,settings,intents));
}
pub fn menu(ui:&mut egui::Ui,session:u64,model:&StudioSnapshot,source:Id,intents:&mut Vec<Intent>){
    let Some(SourceKind::ImageSlideShow{settings})=model.source(source).map(|s|s.kind())else{return;};
    ui.menu_button(crate::i18n::tr(ui,"Slideshow Playback"),|ui|buttons(ui,session,model.revision(),source,settings,intents)).response.on_hover_text(crate::i18n::tr(ui,"Control the shared slideshow without changing scene history. The source must be visible in an active output."));
}
fn buttons(ui:&mut egui::Ui,session:u64,revision:u64,source:Id,settings:&skylive_core::ImageSlideShow,intents:&mut Vec<Intent>){
    let target=Target{session,revision,source};
    for(label,operation,hint)in[
        ("Previous in Playlist",Control::Previous,"Show the previous slide, wrapping at the first. A running transition must finish first; the automatic interval is retained."),
        ("Play",Control::Play,"Play this slideshow automatically. Playing a stopped manual slideshow retains its manual mode, as in OBS."),
        ("Pause",Control::Pause,"Pause automatic slide changes and enable runtime manual mode. The current transition and GIF animation can continue."),
        ("Restart",Control::Restart,"Return to the first slide and reset the interval. Restart retains the current runtime manual mode."),
        ("Stop",Control::Stop,"Reset the slideshow to the first index and transition its picture to transparency."),
        ("Next in Playlist",Control::Next,"Show the next slide, wrapping at the last. A running transition must finish first; the automatic interval is retained."),
    ]{let hint=if settings.randomize(){match(operation,settings.random_compatibility()){
        (Control::Previous,skylive_core::SlideShowRandomCompatibility::BufferedV2)=>"Show the buffered previous random slide and extend the far history edge. Busy transitions ignore this command.",
        (Control::Next,skylive_core::SlideShowRandomCompatibility::BufferedV2)=>"Show the buffered next random slide and extend the far future edge. Busy transitions ignore this command.",
        (Control::Previous|Control::Next,skylive_core::SlideShowRandomCompatibility::LegacyV1)=>"Choose another random slide, excluding the current one. Legacy Previous uses the same random rule as Next.",
        (Control::Restart,skylive_core::SlideShowRandomCompatibility::BufferedV2)=>"Rebuild the random start and five-slide history buffers using the owned RNG. Keep the runtime manual mode.",
        (Control::Stop,skylive_core::SlideShowRandomCompatibility::BufferedV2)=>"Rebuild the random buffers and transition to transparency. Playing again reveals the prepared random start.",_=>hint,
    }}else{hint};if ui.button(crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,hint)).clicked(){intents.push(Intent::ControlSlideshow{target,operation:Some(operation)});}}
    if ui.button(crate::i18n::tr(ui,"Slideshow Status")).on_hover_text(crate::i18n::tr(ui,"Read the actual slideshow index, playback state and runtime manual mode.")).clicked(){intents.push(Intent::ControlSlideshow{target,operation:None});}
}
