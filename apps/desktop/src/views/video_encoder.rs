//! Probe receipts and editable choice are presentation state, not authoring data.
use eframe::egui;
use skylive_core::VideoEncoder;
#[derive(Default)]
pub struct Draft {pub selection:VideoEncoder,pub catalog:Option<Result<skylive_host::native_h264::NvencCatalog,String>>,pub request:bool,pub probing:bool}
impl Draft {pub fn new(selection:VideoEncoder)->Self{Self {selection,..Default::default()}}}
pub fn editor(ui:&mut egui::Ui,draft:&mut Draft){
    let software=if cfg!(target_os="windows"){"Software H.264"}else{"System H.264"};
    let label=ui.label(crate::i18n::tr(ui,"Video Encoder"));
    let selected=match draft.selection {VideoEncoder::Software=>crate::i18n::tr(ui,software),VideoEncoder::NvencH264 {adapter_luid}=>draft.catalog.as_ref().and_then(|r|r.as_ref().ok()).and_then(|c|c.adapters.iter().find(|a|a.luid==adapter_luid)).map(|a|format!("NVIDIA NVENC H.264 · {}",a.name)).unwrap_or_else(||format!("NVIDIA NVENC H.264 · LUID {adapter_luid:016x}"))};
    egui::ComboBox::from_id_salt("h264-encoder-provider").selected_text(selected).show_ui(ui,|ui|{
        ui.selectable_value(&mut draft.selection,VideoEncoder::Software,crate::i18n::tr(ui,software)).on_hover_text(crate::i18n::tr(ui,if cfg!(target_os="windows"){"Use the Windows software encoder. No NVIDIA driver is required."}else{"Use the existing system H.264 encoder."}));
        if let Some(Ok(catalog))=&draft.catalog{for adapter in &catalog.adapters {
            let text=format!("NVIDIA NVENC H.264 · {}",adapter.name);
            let response=ui.add_enabled(adapter.available,egui::Button::new(text).selected(draft.selection==VideoEncoder::NvencH264 {adapter_luid:adapter.luid}));
            if response.clicked(){draft.selection=VideoEncoder::NvencH264 {adapter_luid:adapter.luid};}
            response.on_hover_text(adapter.reason.as_deref().map(str::to_owned).unwrap_or_else(||crate::i18n::tf(ui,"NVENC Baseline / NV12 / CBR · {minw}×{minh}…{maxw}×{maxh}",&[("{minw}",adapter.min_width.to_string()),("{minh}",adapter.min_height.to_string()),("{maxw}",adapter.max_width.to_string()),("{maxh}",adapter.max_height.to_string())]))).on_disabled_hover_text(adapter.reason.as_deref().unwrap_or(""));
        }}
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose an encoder for the next output start. An unavailable NVIDIA adapter never falls back silently."));
    if ui.add_enabled(cfg!(all(target_os="windows",target_arch="x86_64"))&&!draft.probing,egui::Button::new(crate::i18n::tr(ui,if draft.probing{"Probing NVIDIA encoders…"}else{"Probe NVIDIA encoders"}))).on_hover_text(crate::i18n::tr(ui,"Explicitly load the installed NVIDIA driver and query real adapter capabilities on a worker. This does not start recording or streaming.")).on_disabled_hover_text(crate::i18n::tr(ui,"NVENC requires Windows x64. A running probe must retire before another is admitted.")).clicked(){draft.request=true;}
    match &draft.catalog{Some(Ok(catalog)) if catalog.adapters.is_empty()=>{ui.label(crate::i18n::tr(ui,"No NVIDIA hardware adapters were found."));},Some(Err(error))=>{ui.colored_label(ui.visuals().error_fg_color,error);},_=>{}}
    ui.small(crate::i18n::tr(ui,"NVENC: H.264 Baseline, 8-bit NV12, CBR, P4, no B-frames or lookahead. Driver support is checked again at Start."));
}
