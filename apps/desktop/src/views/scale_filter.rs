//! Presentation only: the command owner publishes the complete filter draft.
use eframe::egui;
use skylive_core::{ScaleFilterTarget as Target,ScaleFilterSampling as Sampling,StudioSnapshot};
fn caption(ui:&egui::Ui,target:Target)->String{match target{
    Target::None=>crate::i18n::tr_key_or(ui,"Module.obs-filters.None","None"),
    Target::Canvas=>crate::i18n::tr_key_or(ui,"Module.obs-filters.Base.Canvas","Base (Canvas) Resolution"),
    Target::Resolution{width,height}=>format!("{width} × {height}"),
    Target::Aspect{width,height}=>format!("{width}:{height}"),
}}
fn sampling_caption(ui:&egui::Ui,value:Sampling)->String{
    let key=match value{Sampling::Point=>"Module.obs-filters.ScaleFiltering.Point",Sampling::Bilinear=>"Module.obs-filters.ScaleFiltering.Bilinear",Sampling::Bicubic=>"Module.obs-filters.ScaleFiltering.Bicubic",Sampling::Lanczos=>"Module.obs-filters.ScaleFiltering.Lanczos",Sampling::Area=>"Module.obs-filters.ScaleFiltering.Area"};
    crate::i18n::tr_key_or(ui,key,value.label())
}
pub(super) fn editor(ui:&mut egui::Ui,model:&StudioSnapshot,target:&mut Target,sampling:&mut Sampling,undistort:&mut bool){
    let canvas=(model.video().width(),model.video().height());
    let label=ui.label(crate::i18n::tr_key_or(ui,"Module.obs-filters.ScaleFiltering","Scale Filtering"));
    egui::ComboBox::from_id_salt("scale-filter-sampling").width(ui.available_width()).selected_text(sampling_caption(ui,*sampling)).show_ui(ui,|ui|{
        for value in Sampling::ALL{ui.selectable_value(sampling,value,sampling_caption(ui,value))
            .on_hover_text(crate::i18n::tr(ui,"Choose the reconstruction method used by this source filter."));}
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the reconstruction method used by this source filter."));
    let label=ui.label(crate::i18n::tr(ui,"Resolution"));
    egui::ComboBox::from_id_salt("scale-filter-resolution").width(ui.available_width()).selected_text(caption(ui,*target)).show_ui(ui,|ui|{
        for value in [Target::None,Target::Canvas,Target::Aspect{width:16,height:9},Target::Aspect{width:16,height:10},Target::Aspect{width:4,height:3},Target::Aspect{width:1,height:1}]{
            ui.selectable_value(target,value,caption(ui,value)).on_hover_text(crate::i18n::tr(ui,"Select no scaling, the current canvas size, or an aspect ratio that expands one source axis."));
        }
        ui.separator();
        for divisor in [1.0f64,1.25,1.0/0.75,1.5,1.0/0.6,1.75,2.0,2.25,2.5,2.75,3.0]{
            let value=Target::Resolution{width:(f64::from(canvas.0)/divisor) as u32,height:(f64::from(canvas.1)/divisor) as u32};
            ui.selectable_value(target,value,caption(ui,value)).on_hover_text(crate::i18n::tr(ui,"Set an explicit source-filter size derived from the current base canvas."));
        }
        ui.separator();
        if ui.button(crate::i18n::tr(ui,"Custom pixel size")).on_hover_text(crate::i18n::tr(ui,"Enter an independent width and height in pixels for this source filter.")).clicked(){
            if !matches!(target,Target::Resolution{..}){*target=Target::Resolution{width:canvas.0,height:canvas.1};}ui.close_menu();
        }
        if ui.button(crate::i18n::tr(ui,"Custom aspect ratio")).on_hover_text(crate::i18n::tr(ui,"Enter a ratio. The filter expands width or height while retaining the other source dimension.")).clicked(){
            if !matches!(target,Target::Aspect{..}){*target=Target::Aspect{width:16,height:9};}ui.close_menu();
        }
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set the size or aspect ratio produced before the remaining source filters."));
    let aspect=matches!(target,Target::Aspect{..});
    if let Target::Resolution{width,height}|Target::Aspect{width,height}=target{
        egui::Grid::new("scale-filter-dimensions").num_columns(2).show(ui,|ui|{
            for(index,value)in [width,height].into_iter().enumerate(){
                let text=match(aspect,index){(true,0)=>"Ratio width",(true,_)=>"Ratio height",(false,0)=>"Width",_=>"Height"};
                let label=ui.label(crate::i18n::tr(ui,text));let mut editor=egui::DragValue::new(value).range(1..=8192).clamp_existing_to_range(false);
                if !aspect{editor=editor.suffix(" px");}
                ui.add(editor).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Enter a positive dimension or ratio term from 1 to 8192."));ui.end_row();
            }
        });
    }
    if sampling.allows_undistort(){
        ui.checkbox(undistort,crate::i18n::tr_key_or(ui,"Module.obs-filters.UndistortCenter","Undistort center of image when scaling from ultrawide"))
            .on_hover_text(crate::i18n::tr(ui,"Reduce stretching near the image center when changing aspect ratio with Bicubic or Lanczos."));
    }
}
