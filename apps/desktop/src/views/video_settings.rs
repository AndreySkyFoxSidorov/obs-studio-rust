//! Presentation draft for one atomic canvas/FPS/output-format command.
use eframe::egui;
use skylive_core::{OutputResolution,OutputVideoSettings,ScaleFiltering,VideoSettings};
#[derive(Clone,Debug)]
pub struct OutputDraft {pub match_canvas:bool,pub width:u32,pub height:u32,pub filter:ScaleFiltering}
impl OutputDraft {
    pub fn new(settings:OutputVideoSettings,canvas:&VideoSettings)->Self {
        let (match_canvas,width,height)=match settings.resolution(){OutputResolution::MatchCanvas=>(true,canvas.width(),canvas.height()),OutputResolution::Custom {width,height}=>(false,width,height)};
        Self {match_canvas,width,height,filter:settings.scale_filter()}
    }
    pub fn settings(&self)->Result<OutputVideoSettings,String>{
        OutputVideoSettings::new(if self.match_canvas {OutputResolution::MatchCanvas}else{OutputResolution::Custom {width:self.width,height:self.height}},self.filter).map_err(|error|error.to_string())
    }
    pub fn unchanged(&self,settings:OutputVideoSettings)->bool{self.settings().is_ok_and(|value|value==settings)}
}
fn resolution(ui:&mut egui::Ui,id:&str,width:&mut u32,height:&mut u32,hint:&str){
    ui.horizontal(|ui|{
        ui.add(egui::DragValue::new(width).range(0..=4096).speed(1.0)).on_hover_text(crate::i18n::tr(ui,hint));
        ui.label("×");ui.add(egui::DragValue::new(height).range(0..=4096).speed(1.0)).on_hover_text(crate::i18n::tr(ui,hint));
        egui::ComboBox::from_id_salt(id).selected_text(crate::i18n::tr(ui,"Presets")).show_ui(ui,|ui|{
            for (w,h) in [(1280,720),(1920,1080),(2560,1440),(3840,2160),(4096,2160),(1024,768),(720,1280),(1080,1920)]{
                if ui.selectable_label((*width,*height)==(w,h),format!("{w}×{h}")).on_hover_text(crate::i18n::tr(ui,hint)).clicked(){*width=w;*height=h;ui.close_menu();}
            }
        }).response.on_hover_text(crate::i18n::tr(ui,hint));
    });
}
pub fn editor(ui:&mut egui::Ui,width:&mut u32,height:&mut u32,fps_num:&mut u32,fps_den:&mut u32,output:&mut OutputDraft){
    egui::Grid::new("video-resolution-fields").num_columns(2).spacing([16.0,10.0]).show(ui,|ui|{
        ui.label(crate::i18n::tr(ui,"Base (Canvas) Resolution"));
        resolution(ui,"base-video-resolution",width,height,"Set the authored canvas size. Source positions and capture-device formats remain in their existing spaces.");ui.end_row();
        ui.label(crate::i18n::tr(ui,"Output (Scaled) Resolution"));
        ui.vertical(|ui|{
            ui.checkbox(&mut output.match_canvas,crate::i18n::tr(ui,"Match Canvas")).on_hover_text(crate::i18n::tr(ui,"Use the canvas dimensions for encoded output without a final resize."));
            if output.match_canvas {ui.label(format!("{}×{}",*width,*height));}
            else {resolution(ui,"scaled-video-resolution",&mut output.width,&mut output.height,"Set the actual recording, streaming and replay frame size. The complete canvas is resampled after compositing.");}
        });ui.end_row();
        ui.label(crate::i18n::tr(ui,"Downscale Filter"));
        let changes_size=!output.match_canvas&&(output.width,output.height)!=(*width,*height);
        ui.add_enabled_ui(changes_size,|ui|{
            egui::ComboBox::from_id_salt("program-output-scaler").selected_text(crate::i18n::tr(ui,output.filter.label())).show_ui(ui,|ui|{
                for value in [ScaleFiltering::Disabled,ScaleFiltering::Point,ScaleFiltering::Bilinear,ScaleFiltering::Bicubic,ScaleFiltering::Lanczos,ScaleFiltering::Area]{
                    ui.selectable_value(&mut output.filter,value,crate::i18n::tr(ui,value.label())).on_hover_text(crate::i18n::tr(ui,"Apply this CPU reconstruction filter once to the complete Program frame, after sources, filters and transitions."));
                }
            }).response.on_hover_text(crate::i18n::tr(ui,"Apply this CPU reconstruction filter once to the complete Program frame, after sources, filters and transitions."));
        }).response.on_disabled_hover_text(crate::i18n::tr(ui,"Matching dimensions bypass the scaler and preserve the original frame pixels."));ui.end_row();
        ui.label(crate::i18n::tr(ui,"Common FPS Values"));
        let rate=if *fps_den==0 {crate::i18n::tr(ui,"Custom")}else{format!("{:.3}",f64::from(*fps_num)/f64::from(*fps_den))};
        egui::ComboBox::from_id_salt("video-common-fps").selected_text(rate).show_ui(ui,|ui|{
            for (num,den,label) in [(24,1,"24"),(25,1,"25"),(30,1,"30"),(48,1,"48"),(50,1,"50"),(60,1,"60"),(120,1,"120"),(240,1,"240"),(24000,1001,"23.976 (24000/1001)"),(30000,1001,"29.970 (30000/1001)"),(60000,1001,"59.940 (60000/1001)")] {
                if ui.selectable_label((*fps_num,*fps_den)==(num,den),label).clicked(){*fps_num=num;*fps_den=den;}
            }
        }).response.on_hover_text(crate::i18n::tr(ui,"Choose a common exact frame rate, or edit the rational numerator and denominator below."));ui.end_row();
        ui.label(crate::i18n::tr(ui,"FPS Numerator"));ui.add(egui::DragValue::new(fps_num).speed(1.0)).on_hover_text(crate::i18n::tr(ui,"Set the exact frame-rate numerator. Canvas and output share the same frame rate."));ui.end_row();
        ui.label(crate::i18n::tr(ui,"FPS Denominator"));ui.add(egui::DragValue::new(fps_den).speed(1.0)).on_hover_text(crate::i18n::tr(ui,"Set 1001 for rates such as 30000/1001; no automatic FPS reduction is applied."));ui.end_row();
    });
    ui.separator();
    ui.label(crate::i18n::tr(ui,"Video settings apply atomically to the selected profile. Stop active outputs before applying format changes."));
    ui.label(crate::i18n::tr(ui,"Recording and streaming require encoder-supported output dimensions, including even sizes for NV12. Unsupported formats report an error instead of reducing resolution or FPS."));
    ui.label(crate::i18n::tr(ui,"Preview remains adaptive. Virtual camera applies the global output scale before fitting its separately selected camera format."));
}
