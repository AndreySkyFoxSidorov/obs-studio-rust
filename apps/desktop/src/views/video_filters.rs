//! Staged, bounded source-effect editor. Canonical source ownership stays in
//! the command engine; this view never evaluates or edits pixel buffers.
use super::Intent;
use eframe::egui;
use skylive_core::{
    ChromaKeyType, ColorKeyType, Command, Id, KeyMatch, MAX_VIDEO_FILTERS, MatteView, SpillChannel,
    StudioSnapshot, VideoEffect, VideoFilter,
};
struct Row {
    id: u32,
    name: String,
    filter: VideoFilter,
    mask_path:String,mask_loading:bool,mask_error:Option<String>,
    lut_path:String,lut_loading:bool,lut_error:Option<String>,
}
pub use skylive_core::VideoFilterPreviewTarget as VideoFilterTarget;
pub struct VideoFilterDraft {
    pub mask_intent:Option<Box<Intent>>,
    pub lut_intent:Option<Box<Intent>>,
    pub token:u64,
    pub preview:crate::filter_preview::Preview,
    pub session: u64,
    pub revision: u64,
    pub target: VideoFilterTarget,
    source_is_ndi: bool,
    source_is_camera: bool,
    name: String,
    rows: Vec<Row>,
    selected: Option<u32>,
    next_id: u32,
    error: Option<String>,
}
impl VideoFilterDraft {
    pub fn capture(session: u64, model: &StudioSnapshot, source: Id) -> Result<Self, String> {
        let source = model
            .source(source)
            .ok_or("The selected source no longer exists.")?;
        if model
            .source_video_dimensions(source.kind())
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("Audio-only sources have no video filters.".into());
        }
        Ok(Self::capture_rows(
            session,
            model,
            VideoFilterTarget::Source(source.id()),
            source.name(),
            source.video_filters(),
            matches!(source.kind(),skylive_core::SourceKind::NdiSource {..}),
            matches!(source.kind(),skylive_core::SourceKind::VideoCapture {..}),
        ))
    }
    pub fn capture_scene(session: u64, model: &StudioSnapshot, scene: Id) -> Result<Self, String> {
        let scene_ref = model
            .scene(scene)
            .ok_or("The selected scene no longer exists.")?;
        let filters = model
            .scene_source(scene)
            .map_or(&[][..], |source| source.video_filters());
        Ok(Self::capture_rows(
            session,
            model,
            VideoFilterTarget::Scene(scene),
            scene_ref.name(),
            filters,
            false,
            false,
        ))
    }
    fn capture_rows(
        session: u64,
        model: &StudioSnapshot,
        target: VideoFilterTarget,
        name: &str,
        filters: &[VideoFilter],
        source_is_ndi:bool,
        source_is_camera:bool,
    ) -> Self {
        let rows: Vec<_> = filters
            .iter()
            .enumerate()
            .map(|(index, filter)| Row {
                id: index as u32 + 1,
                name: filter.name().into(),
                mask_path:filter.mask_image().map_or(String::new(),|a|a.path().to_owned()),mask_loading:false,mask_error:None,
                lut_path:filter.lut_asset().map_or(String::new(),|a|a.path().to_owned()),lut_loading:false,lut_error:None,
                filter: filter.clone(),
            })
            .collect();
        Self {
            mask_intent:None,lut_intent:None,token:0,preview:Default::default(),
            session,
            revision: model.revision(),
            target,
            source_is_ndi,
            source_is_camera,
            name: name.into(),
            selected: rows.first().map(|row| row.id),
            next_id: rows.len() as u32 + 1,
            rows,
            error: None,
        }
    }
    /// Runtime-only identity; row values never come from positions or labels.
    pub fn preview_row_ids(&self)->Vec<u32>{self.rows.iter().map(|row|row.id).collect()}
    pub fn can_expire_freeze(&self,window:u64,row:u32,source:Id)->bool{
        self.token==window&&self.preview.freeze_source()==Some(source)&&self.rows.iter().any(|value|value.id==row&&value.filter.enabled&&matches!(value.filter.effect,VideoEffect::Freeze{..}))
    }
    pub fn expire_freeze(&mut self,window:u64,row:u32,source:Id)->bool{
        if !self.can_expire_freeze(window,row,source){return false;}
        if let Some(value)=self.rows.iter_mut().find(|value|value.id==row){value.filter.enabled=false;true}else{false}
    }
    pub fn preview_filters(&self)->Result<Vec<VideoFilter>,String> {
        // Names are presentation-only while the dialog is open. In particular,
        // an empty name during editing must not invalidate a live camera frame.
        // The canonical command still validates and publishes the draft names.
        self.build_filters(false)
    }
    pub fn command(&self) -> Result<Command, String> {
        let filters = self.build_filters(true)?;
        Ok(match self.target {
            VideoFilterTarget::Source(source) => Command::SetSourceVideoFilters { source, filters },
            VideoFilterTarget::Scene(scene) => Command::SetSceneVideoFilters { scene, filters },
        })
    }
    fn build_filters(&self, draft_names: bool) -> Result<Vec<VideoFilter>, String> {
        if self.rows.len() > MAX_VIDEO_FILTERS {
            return Err("A source supports at most 32 video filters.".into());
        }
        if (self.source_is_ndi || self.source_is_camera) && self.rows.iter().any(|row|row.filter.enabled && matches!(row.filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})) {
            if self.rows.iter().any(|row|row.filter.enabled && matches!(row.filter.effect,
                VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..})) {
                return Err("Async Video Delay cannot yet be combined with Freeze or Render Delay.".into());
            }
            // Mask resources and neural inference are evaluated on the
            // lease-bearing delayed image. Before/between async queues they
            // would run at a different timestamp, so accept them only after
            // the last active nonzero delay.
            let last_delay=self.rows.iter().rposition(|row|row.filter.enabled
                && matches!(row.filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})).expect("active delay was checked above");
            let staged_mask=(self.source_is_camera || self.source_is_ndi) && self.rows.iter().take(last_delay).any(|row|row.filter.enabled
                && (matches!(row.filter.effect,VideoEffect::PersonBackgroundRemoval{..}|VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..})
                    || matches!(row.filter.effect,VideoEffect::ApplyLut{..}) && !row.lut_path.is_empty()
                    || matches!(row.filter.effect,VideoEffect::ImageMask{..}) && !row.mask_path.is_empty()));
            for row in self.rows.iter().take(last_delay).filter(|row|row.filter.enabled) {
                match row.filter.effect {
                    VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX}=>{},
                    VideoEffect::Scroll{..} if (self.source_is_camera || self.source_is_ndi) && cfg!(target_os="windows")=>{},
                    VideoEffect::Scroll{..}=>return Err("Async Video Delay cannot yet be combined with Freeze or Render Delay; Scroll must follow the final delay.".into()),
                    VideoEffect::ImageMask{..} if row.mask_path.is_empty()=>{},
                    VideoEffect::ImageMask{..} if self.source_is_camera=>{
                        // Animated masks need a Windows device clock that can
                        // be correlated with playback. The worker validates
                        // each resource and sample before publication.
                        let ext=std::path::Path::new(&row.mask_path).extension().and_then(|v|v.to_str()).unwrap_or("").to_ascii_lowercase();
                        let static_format=matches!(ext.as_str(),"jpg"|"jpeg"|"bmp"|"tga");
                        let timed_format=cfg!(target_os="windows") && matches!(ext.as_str(),"png"|"apng"|"webp"|"gif");
                        if !static_format && !timed_format {
                            return Err("This camera mask format is unavailable before Async Video Delay on this platform.".into());
                        }
                    },
                    VideoEffect::ImageMask{..} if self.source_is_ndi=>{
                        let ext=std::path::Path::new(&row.mask_path).extension().and_then(|v|v.to_str()).unwrap_or("").to_ascii_lowercase();
                        if !matches!(ext.as_str(),"jpg"|"jpeg"|"bmp"|"tga") && !(cfg!(target_os="windows") && matches!(ext.as_str(),"png"|"apng"|"webp"|"gif")) {
                            return Err("This NDI mask format is unavailable before Async Video Delay on this platform.".into());
                        }
                    },
                    VideoEffect::ScaleFilter{target:skylive_core::ScaleFilterTarget::Resolution{..},..} if self.source_is_camera=>{},
                    VideoEffect::ScaleFilter{..} if self.source_is_camera=>return Err("Before Async Video Delay, camera Scale currently requires a fixed Resolution target.".into()),
                    VideoEffect::ApplyLut{..} if self.source_is_camera||self.source_is_ndi=>{},
                    VideoEffect::PersonBackgroundRemoval{..} if self.source_is_camera||self.source_is_ndi=>{},
                    VideoEffect::ImageMask{..}|VideoEffect::PersonBackgroundRemoval{..}=>return Err("Move Image Mask and AI Background Removal after all active Async Video Delay filters.".into()),
                    _ if staged_mask=>return Err("Before Async Video Delay, only fixed-resolution camera Scale, Image Mask, Apply LUT and AI Background Removal are supported in this chain.".into()),
                    _=>{},
                }
            }
            let total=self.rows.iter().filter_map(|row|if row.filter.enabled {if let VideoEffect::AsyncVideoDelay{delay_ms}=row.filter.effect {Some(u32::from(delay_ms))}else{None}}else{None}).sum::<u32>();
            if total>20_000{return Err("The combined Async Video Delay cannot exceed 20 seconds.".into());}
        }
        let mut filters = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            if (draft_names || row.filter.enabled) && matches!(row.filter.effect,VideoEffect::ImageMask {..}) && !row.mask_path.is_empty()
                && (row.mask_loading||row.mask_error.is_some()||row.filter.mask_image().is_none_or(|a|a.path()!=row.mask_path)) {
                return Err("Load or refresh the mask image before applying this filter.".into());
            }
            if (draft_names || row.filter.enabled) && matches!(row.filter.effect,VideoEffect::ApplyLut{..})&&!row.lut_path.is_empty()
                &&(row.lut_loading||row.lut_error.is_some()||row.filter.lut_asset().is_none_or(|a|a.path()!=row.lut_path)){
                return Err("Load or refresh the LUT before applying this filter.".into());
            }
            row.filter.validate().map_err(|error|error.to_string())?;
            let mut filter=row.filter.clone();
            if matches!(filter.effect,VideoEffect::ImageMask{..})&&row.mask_path.is_empty(){filter=filter.with_mask_image(None).map_err(|e|e.to_string())?;}
            if matches!(filter.effect,VideoEffect::ApplyLut{..})&&row.lut_path.is_empty(){filter=filter.with_lut_asset(None).map_err(|e|e.to_string())?;}
            // Stable row identities keep preview names unique even when two
            // newly added effects share the same default name. They also keep
            // text editing out of the rendered-frame cache identity.
            let name = if draft_names { row.name.clone() } else { format!("Preview filter {}",row.id) };
            filter=filter.with_name(name).map_err(|e|e.to_string())?;
            filters.push(filter);
        }
        Ok(filters)
    }
    pub fn intent(&self) -> Result<Intent, String> {
        match self.command()? {
            Command::SetSourceVideoFilters { source, filters } => Ok(Intent::ApplyVideoFilters {
                token:self.token,session: self.session,
                revision: self.revision,
                source,
                filters,
            }),
            Command::SetSceneVideoFilters { scene, filters } => {
                Ok(Intent::ApplySceneVideoFilters {
                    token:self.token,session: self.session,
                    revision: self.revision,
                    scene,
                    filters,
                })
            }
            _ => Err("Invalid video filter command.".into()),
        }
    }

    pub fn append_factory(&mut self,factory:skylive_core::VideoFilterFactory)->Result<(),String>{
        self.add(factory.prototype());
        match &self.error {Some(error)=>Err(error.clone()),None=>Ok(())}
    }
    /// Duplicate an owned draft without reusing the canonical filter identity.
    /// Resource preparations are tied to the old row, so wait for them before
    /// copying their validated asset and path into the new row.
    fn duplicate(&mut self, id:u32) {
        let Some(index)=self.rows.iter().position(|row|row.id==id) else{return;};
        if self.rows.len()>=MAX_VIDEO_FILTERS {
            self.error=Some("A source supports at most 32 video filters.".into());return;
        }
        if self.rows[index].mask_loading || self.rows[index].lut_loading {
            self.error=Some("Wait for the filter resource to finish loading before duplicating it.".into());return;
        }
        let Some(next)=self.next_id.checked_add(1) else {
            self.error=Some("Video filter editor identity is exhausted.".into());return;
        };
        let source=&self.rows[index];
        let base=if source.name.trim().is_empty() || source.name.chars().any(char::is_control) {
            source.filter.effect.name()
        } else {source.name.trim()};
        let name=(2..=MAX_VIDEO_FILTERS+2).find_map(|number| {
            let suffix=format!(" {number}");
            let mut end=base.len().min(256-suffix.len());
            while !base.is_char_boundary(end){end-=1;}
            let candidate=format!("{}{suffix}",base[..end].trim_end());
            (!self.rows.iter().any(|row|row.name==candidate)).then_some(candidate)
        });
        let Some(name)=name else {self.error=Some("Video filter name generation exhausted.".into());return;};
        let row=Row {id:self.next_id,name,filter:source.filter.as_new(),
            mask_path:source.mask_path.clone(),mask_loading:false,mask_error:source.mask_error.clone(),
            lut_path:source.lut_path.clone(),lut_loading:false,lut_error:source.lut_error.clone()};
        self.rows.insert(index+1,row);
        self.selected=Some(self.next_id);self.next_id=next;self.error=None;
    }
    fn add(&mut self, effect: VideoEffect) {
        if self.rows.len() >= MAX_VIDEO_FILTERS {
            self.error = Some("A source supports at most 32 video filters.".into());
            return;
        }
        let Some(next) = self.next_id.checked_add(1) else {
            self.error = Some("Video filter editor identity is exhausted.".into());
            return;
        };
        match VideoFilter::new(effect) {
            Ok(filter) => {
                let base = filter.name();
                let name = (1..=MAX_VIDEO_FILTERS + 1)
                    .map(|i| {
                        if i == 1 {
                            base.to_owned()
                        } else {
                            format!("{base} {i}")
                        }
                    })
                    .find(|name| !self.rows.iter().any(|row| row.name == *name));
                let Some(name) = name else {
                    self.error = Some("Video filter name generation exhausted.".into());
                    return;
                };
                self.rows.push(Row {
                    id: self.next_id,
                    name,
                    filter,mask_path:String::new(),mask_loading:false,mask_error:None,lut_path:String::new(),lut_loading:false,lut_error:None,
                });
                self.selected = Some(self.next_id);
                self.next_id = next;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}
pub fn editor(ui: &mut egui::Ui, model: &StudioSnapshot, draft: &mut VideoFilterDraft) {
    ui.label(crate::i18n::tf(
        ui,
        "Source: {name}",
        &[("{name}", draft.name.clone())],
    ));
    ui.label(crate::i18n::tr(ui,"Filters run from top to bottom, matching OBS. Changes affect every scene reference to this source. Apply saves the stack; Cancel discards the draft."));
    let available = model
        .preferences()
        .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN);
    if !available {
        ui.label(crate::i18n::tr(
            ui,
            "Craft effects are disabled. Built-in OBS video filters remain available.",
        ));
    }
    ui.add_enabled_ui(draft.rows.len() < MAX_VIDEO_FILTERS, |ui| {
        ui.menu_button(crate::i18n::tr(ui, "Add Filter"), |ui| {
            let choices = skylive_core::VideoFilterFactory::ALL.map(|factory|super::filter_add_menu::Choice {
                label:factory.label(),hint:factory.hint(),
                enabled:model.preferences().plugin_enabled(factory.plugin_id())
                    && (factory!=skylive_core::VideoFilterFactory::AsyncVideoDelay || draft.source_is_ndi || draft.source_is_camera),
                disabled_hint:if factory==skylive_core::VideoFilterFactory::AsyncVideoDelay && !draft.source_is_ndi && !draft.source_is_camera {
                    Some("Video Delay (Async) currently supports cameras and NDI sources only.")
                }else{None},
                group:Some(match factory.plugin_id(){
                    skylive_core::CRAFT_VIDEO_PLUGIN=>"Craft video effects",
                    skylive_core::PERSON_BACKGROUND_PLUGIN=>"Neural filters",
                    _=>"OBS video filters",
                }),
            });
            if let Some(factory)=super::filter_add_menu::show(ui,&choices)
                .and_then(|index|skylive_core::VideoFilterFactory::ALL.get(index).copied()) {
                draft.add(factory.prototype());
            }
        })
        .response
        .on_hover_text(crate::i18n::tr(
            ui,
            "Add a video effect to this source. A stack supports at most 32 filters.",
        ))
        .on_disabled_hover_text(crate::i18n::tr(
            ui,
            "A source supports at most 32 video filters.",
        ));
    });
    if let Some(error) = &draft.error {
        let translated=match error.as_str() {
            "Async Video Delay cannot yet be combined with Freeze or Render Delay; Scroll must follow the final delay." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.TemporalConflict",error),
            "Async Video Delay cannot yet be combined with Freeze or Render Delay." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.FreezeConflict",error),
            "NDI masks before Async Video Delay currently require a static JPEG, BMP or TGA image." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.NdiStaticMaskRequired",error),
            "This NDI mask format is unavailable before Async Video Delay on this platform." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.NdiMaskFormat",error),
            "Before Async Video Delay, only Image Mask, Apply LUT and AI Background Removal are supported in this chain." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.StagedChainSupported",error),
            "Move Image Mask and AI Background Removal after all active Async Video Delay filters." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.MaskOrdering",error),
            "The combined Async Video Delay cannot exceed 20 seconds." => crate::i18n::tr_key_or(ui,"AsyncVideoDelay.TotalLimit",error),
            _=>crate::i18n::tr(ui,error),
        };
        ui.colored_label(ui.visuals().error_fg_color,translated);
    }
    if ui.available_width() >= 620.0 {
        let (area, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 360.0),
            egui::Sense::hover(),
        );
        let list_rect =
            egui::Rect::from_min_max(area.min, egui::pos2(area.left() + 224.0, area.bottom()));
        let property_rect =
            egui::Rect::from_min_max(egui::pos2(list_rect.right() + 16.0, area.top()), area.max);
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("video-filter-list-pane")
                .max_rect(list_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        left.set_clip_rect(list_rect.intersect(ui.clip_rect()));
        left.set_width(list_rect.width());
        list(&mut left, model, draft, 220.0);
        ui.painter().line_segment(
            [
                egui::pos2(list_rect.right() + 8.0, area.top()),
                egui::pos2(list_rect.right() + 8.0, area.bottom()),
            ],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("video-filter-property-pane")
                .max_rect(property_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        right.set_clip_rect(property_rect.intersect(ui.clip_rect()));
        right.set_width(property_rect.width());
        draft.preview.show(&mut right,150.0);
        right.separator();
        properties(&mut right, model, draft);
    } else {
        draft.preview.show(ui,160.0);
        ui.separator();
        list(ui, model, draft, 112.0);
        ui.separator();
        properties(ui, model, draft);
    }
    if draft.source_is_camera && draft.rows.iter().rposition(|row|row.filter.enabled
        && matches!(row.filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})).is_some_and(|last|
            draft.rows[..last].iter().any(|row|row.filter.enabled&&(matches!(row.filter.effect,VideoEffect::PersonBackgroundRemoval{..}|VideoEffect::ScaleFilter{..})
                || matches!(row.filter.effect,VideoEffect::ApplyLut{..})&&!row.lut_path.is_empty()
                || matches!(row.filter.effect,VideoEffect::ImageMask{..})&&!row.mask_path.is_empty()))) {
        ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.StagedPreview","Preview uses the latest camera frame; Video Delay takes effect after Apply. PNG, APNG, WebP and GIF masks require a correlated camera timestamp; unavailable timing is reported for the source."));
    }
    if let Some(key)=draft.mask_key() {
        ui.horizontal_wrapped(|ui|{
            if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::Mask,!draft.mask_busy()) {draft.mask_intent=Some(Box::new(intent));}
            if ui.add_enabled(!draft.mask_busy()&&!key.path.is_empty(),egui::Button::new(crate::i18n::tr(ui,"Load / Refresh"))).on_hover_text(crate::i18n::tr(ui,"Read the current file on the I/O worker and prepare its exact hash and dimensions for this draft.")).clicked(){draft.mask_intent=Some(Box::new(Intent::PrepareMask(key.clone())));}
        });
    }
    if let Some(key)=draft.lut_key(){ui.horizontal_wrapped(|ui|{
        if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::Lut,!draft.lut_busy()){draft.lut_intent=Some(Box::new(intent));}
        if ui.add_enabled(!draft.lut_busy()&&!key.path.is_empty(),egui::Button::new(crate::i18n::tr(ui,"Load / Refresh LUT")))
            .on_hover_text(crate::i18n::tr(ui,"Validate the current LUT on the I/O worker and prepare its exact content hash and dimensions. Apply accepts this resource; Cancel discards the draft."))
            .clicked(){draft.lut_intent=Some(Box::new(Intent::PrepareLut(key.clone())));}
    });}
    let requested=draft.preview_filters();
    draft.preview.observe(requested,ui.ctx());
}
fn list(ui: &mut egui::Ui, model: &StudioSnapshot, draft: &mut VideoFilterDraft, height: f32) {
    let mut duplicate=None;
    let can_duplicate=draft.rows.len()<MAX_VIDEO_FILTERS;
    egui::ScrollArea::vertical()
        .id_salt("video-filter-list")
        .max_height(height)
        .show(ui, |ui| {
            let scope=super::filter_chain_reorder::Scope::new(ui.id(),draft.session,
                model.revision(),draft.token,draft.rows.iter().map(|row|row.id));
            let mut rectangles=Vec::with_capacity(draft.rows.len());
            for row in &mut draft.rows {
                ui.push_id(("video-filter-row", row.id), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            model.preferences().plugin_enabled(row.filter.plugin_id()),
                            egui::Checkbox::new(
                                &mut row.filter.enabled,
                                crate::i18n::tr(ui, "Enabled"),
                            ),
                        )
                        .on_hover_text(crate::i18n::tr(
                            ui,
                            "Enable or bypass this video filter without losing its settings.",
                        ));
                        let response=ui
                            .add_sized(
                                [ui.available_width(), 28.0],
                                egui::Button::new(row.name.as_str())
                                    .selected(draft.selected == Some(row.id))
                                    .sense(egui::Sense::click_and_drag())
                                    .truncate(),
                            )
                            .on_hover_text(crate::i18n::tr(
                                ui,
                                "Select this filter to edit its properties. Drag to change its processing order, or use Move Filter Up and Move Filter Down. Apply saves the order; Cancel discards it.",
                            ));
                        if let Some(scope)=scope {
                            super::filter_chain_reorder::row(ui,&response,scope,row.id,
                                model.preferences().plugin_enabled(row.filter.plugin_id()));
                        }
                        rectangles.push((row.id,response.rect));
                        if response.clicked() {
                            draft.selected = Some(row.id);
                        }
                        response.context_menu(|ui| {
                            if ui.add_enabled(can_duplicate && !row.mask_loading && !row.lut_loading,
                                egui::Button::new(crate::i18n::tr(ui,"Duplicate"))).clicked() {
                                duplicate=Some(row.id);ui.close_menu();
                            }
                        });
                    });
                });
            }
            if draft.rows.is_empty() {
                ui.label(crate::i18n::tr(ui, "No video filters."));
            }
            if let Some(scope)=scope {
                if let Some((old,index))=super::filter_chain_reorder::finish(ui,scope,&rectangles,
                    |id|draft.rows.iter().find(|row|row.id==id)
                        .is_some_and(|row|model.preferences().plugin_enabled(row.filter.plugin_id()))) {
                    let row=draft.rows.remove(old);
                    draft.selected=Some(row.id);
                    draft.rows.insert(index,row);
                }
            }
        });
    if let Some(id)=duplicate {draft.duplicate(id);}
    let index = draft
        .selected
        .and_then(|id| draft.rows.iter().position(|row| row.id == id));
    let selected_available = index.is_some_and(|index| {
        model
            .preferences()
            .plugin_enabled(draft.rows[index].filter.plugin_id())
    });
    let actions = [
        super::modal_footer::Action::new(
            "Move Filter Up",
            selected_available && index.is_some_and(|index| index > 0),
            "Move Filter Up",
        ),
        super::modal_footer::Action::new(
            "Move Filter Down",
            selected_available && index.is_some_and(|index| index + 1 < draft.rows.len()),
            "Move Filter Down",
        ),
        super::modal_footer::Action::new("Duplicate", index.is_some_and(|i|
            draft.rows.len()<MAX_VIDEO_FILTERS && !draft.rows[i].mask_loading && !draft.rows[i].lut_loading), "Duplicate"),
        super::modal_footer::Action::new("Remove Filter", index.is_some(), "Remove Filter"),
    ];
    ui.push_id("video-filter-stack-actions", |ui| {
        if let Some(action) = super::modal_footer::show(ui, &actions) {
            if let Some(index) = index {
                match action {
                    0 => draft.rows.swap(index, index - 1),
                    1 => draft.rows.swap(index, index + 1),
                    2 => { let id=draft.rows[index].id; draft.duplicate(id); },
                    _ => {
                        draft.rows.remove(index);
                        draft.selected = draft
                            .rows
                            .get(index.min(draft.rows.len().saturating_sub(1)))
                            .map(|row| row.id);
                    }
                }
            }
        }
    });
}
// Grid cells begin narrow on their first layout pass. Let labels request their
// natural width instead of wrapping individual words into a 40px column.
// The grid then aligns every input after the widest translated label.
fn grid_label(ui: &mut egui::Ui, caption: &str) -> egui::Response {
    ui.add(egui::Label::new(crate::i18n::tr(ui, caption)).extend())
}
fn number(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    unit: &str,
    hint: &str,
) {
    let label = grid_label(ui, label);
    ui.add(
        egui::DragValue::new(value)
            .range(min..=max)
            .clamp_existing_to_range(false)
            .speed(1.0)
            .suffix(unit),
    )
    .labelled_by(label.id)
    .on_hover_text(crate::i18n::tr(ui, hint));
    ui.end_row();
}
fn properties(ui: &mut egui::Ui, model: &StudioSnapshot, draft: &mut VideoFilterDraft) {
    // Resolve the input at this filter's position, including earlier size-changing
    // effects. Live camera frames may differ from the authored capture mode.
    let canvas = Some((model.video().width(), model.video().height()));
    let raw = draft.preview.source_dimensions().or_else(|| match draft.target {
        VideoFilterTarget::Source(source) => model.source(source)
            .and_then(|source| model.source_video_dimensions(source.kind()).ok().flatten()),
        VideoFilterTarget::Scene(_) => canvas,
    });
    let mut input_dimensions = raw;
    for previous in &draft.rows {
        if Some(previous.id) == draft.selected { break; }
        if previous.filter.enabled && model.preferences().plugin_enabled(previous.filter.plugin_id()) {
            input_dimensions = input_dimensions.and_then(|input|
                previous.filter.effect.output_dimensions_with_canvas(input, canvas).ok());
        }
    }
    let source_is_ndi=draft.source_is_ndi;
    let source_is_camera=draft.source_is_camera;
    let Some(row) = draft
        .rows
        .iter_mut()
        .find(|row| Some(row.id) == draft.selected)
    else {
        ui.label(crate::i18n::tr(
            ui,
            "Add or select a filter to edit its properties.",
        ));
        return;
    };
    let available = model.preferences().plugin_enabled(row.filter.plugin_id());
    ui.push_id(("video-filter-properties",row.id),|ui|{
        if matches!(row.filter.effect, VideoEffect::AsyncVideoDelay {..}) {
            ui.strong(crate::i18n::tr_key_or(ui,"Module.obs-filters.AsyncDelayFilter","Video Delay (Async)"));
        } else {
            ui.strong(crate::i18n::tr(ui,row.filter.effect.name()));
        }
        if !available {ui.label(crate::i18n::tr(ui,"Module disabled · settings retained. Enable it in Tools > Plugins to edit these properties."));}
        ui.horizontal(|ui|{let label=ui.label(crate::i18n::tr(ui,"Name"));
            ui.add_enabled(available,egui::TextEdit::singleline(&mut row.name).char_limit(256).id_salt("video-filter-name"))
                .labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Name this filter. Names must be unique within this source."));});
        if ui.add_enabled(available&&!row.mask_loading&&!row.lut_loading,egui::Button::new(crate::i18n::tr(ui,"Defaults")))
            .on_hover_text(crate::i18n::tr(ui,"Restore this filter's default settings in the draft. Apply saves them; Cancel keeps the original settings. The filter name and enabled state are preserved."))
            .on_disabled_hover_text(crate::i18n::tr(ui,if row.mask_loading||row.lut_loading{"Wait for linked filter resource preparation before resetting this filter."}else{"Enable this filter's module in Tools > Plugins before resetting it."})).clicked(){
            row.filter.reset_settings();row.mask_path.clear();row.mask_error=None;row.lut_path.clear();row.lut_error=None;
        }
        egui::ScrollArea::vertical().id_salt("selected-video-filter").max_height(ui.available_height().clamp(72.0,280.0)).show(ui,|ui|{
            ui.add_enabled_ui(available,|ui|{
                let mut person_interval=row.filter.person_interval_ms();
                match &mut row.filter.effect{
                    VideoEffect::Freeze{settings}=>{
                        ui.small(crate::i18n::tr(ui,"Freeze · CPU · SDR"));
                        ui.label(crate::i18n::tr(ui,"The Enabled checkbox holds the first real frame at this filter position. Disable and Apply to release it; enable and Apply to capture again."));
                        let mut timing=settings.timing();
                        let duration=ui.label(crate::i18n::tr(ui,"Duration (ms)"));
                        ui.add(egui::DragValue::new(&mut timing[0]).range(0..=100_000).speed(10.0).suffix(" ms")).labelled_by(duration.id).on_hover_text(crate::i18n::tr(ui,"Disable Freeze after this many milliseconds from enabling it, including time spent waiting for the first frame. Zero keeps it enabled indefinitely."));
                        let refresh=ui.label(crate::i18n::tr(ui,"Refresh interval (ms)"));
                        ui.add(egui::DragValue::new(&mut timing[1]).range(0..=100_000).speed(10.0).suffix(" ms")).labelled_by(refresh.id).on_hover_text(crate::i18n::tr(ui,"Capture a replacement after this interval. The previous picture remains visible until a real replacement succeeds. Zero disables automatic refresh."));
                        if let Ok(value)=settings.with_timing(timing){*settings=value;}
                        let mut mask=settings.mask();let mut percent=settings.mask_percent();
                        ui.checkbox(&mut mask,crate::i18n::tr(ui,"Partial freeze mask")).on_hover_text(crate::i18n::tr(ui,"Overlay selected held regions on current live video. With all edges at zero, no frozen region is selected."));
                        if mask{
                            for(index,label)in ["Freeze left (%)","Freeze right (%)","Freeze top (%)","Freeze bottom (%)"].into_iter().enumerate(){
                                let caption=ui.label(crate::i18n::tr(ui,label));
                                ui.add(egui::DragValue::new(&mut percent[index]).range(0.0..=100.0).speed(0.01).suffix(" %")).labelled_by(caption.id)
                                    .on_hover_text(crate::i18n::tr(ui,"Select the frozen edge percentage from 0 to 100. Opposing edges above 100 percent together use the original Freeze mask intersection rule."));
                            }
                        }
                        if let Ok(value)=settings.with_mask(mask,percent){*settings=value;}
                        ui.small(crate::i18n::tr(ui,"The filter preview owns an independent frozen picture. Apply keeps an existing Program capture; draft pixels are not adopted into Program."));
                        ui.small(crate::i18n::tr(ui,"Hidden, stopped, rebound, resized or seeked sources release their held picture. Resume captures a fresh real frame."));
                        ui.small(crate::i18n::tr(ui,"Held, replacing and still displayed frames share the 128 MiB filter budget. Overload is reported without reducing resolution."));
                        ui.small(crate::i18n::tr(ui,"Start/end delays, lifecycle actions, Freeze hotkeys and filter-position screenshots are not implemented."));
                    },
                    VideoEffect::AsyncVideoDelay{delay_ms}=>{
                        let label=ui.label(crate::i18n::tr_key_or(ui,"Module.obs-filters.DelayMs","Delay"));
                        ui.add_enabled(source_is_ndi || source_is_camera,egui::DragValue::new(delay_ms).range(0..=20_000).speed(1.0).suffix(" ms"))
                            .labelled_by(label.id).on_disabled_hover_text(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.SourceUnavailable","Video Delay (Async) currently supports cameras and NDI sources only."));
                        if source_is_ndi {
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.NdiPreview","The preview uses the latest delivered frame; the new delay takes effect after Apply."));
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.NdiOutputs","Streaming, recording and monitoring use separate audio readers. Program, Replay Buffer and NDI Program use estimated capture timing; their output synchronization is not confirmed."));
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.NdiScrollEstimate","Before or between delays, animated masks and Scroll use local packet receive time, not sender capture time. Network latency affects the result; Scroll estimates the phase within 5 ms of receive time and frames may be skipped if the clock is unavailable or imprecise. Freeze and Render Delay are unavailable."));
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.NdiLifecycle","Changing this delay while streaming or recording can stop that output. Configure it before starting."));
                        } else if source_is_camera {
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.CameraPreview","The preview uses the latest delivered frame; the new delay takes effect after Apply."));
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.CameraTiming","Camera audio synchronization needs a valid capture clock. If it is unavailable, audio remains silent rather than being presented without alignment."));
                            ui.small(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.CameraScrollEstimate","Before or between delays, camera Scroll estimates capture time within 5 ms. If the camera clock is unavailable or too imprecise, frames cannot be processed; move Scroll after the final delay. Freeze and Render Delay remain unavailable."));
                        } else {ui.label(crate::i18n::tr_key_or(ui,"AsyncVideoDelay.SourceUnavailable","Video Delay (Async) currently supports cameras and NDI sources only."));}
                    },
                    VideoEffect::RenderDelay{delay_ms}=>{
                        let label=ui.label(crate::i18n::tr(ui,"Delay (ms)"));
                        ui.add(egui::DragValue::new(delay_ms).range(0..=500).speed(1.0).suffix(" ms")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set Render Delay from 0 to 500 milliseconds. Zero bypasses the delay; audio is unchanged."));
                        ui.small(crate::i18n::tr(ui,"Render Delay · CPU · SDR · shared source history"));
                        ui.small(crate::i18n::tr(ui,"The queue follows actual rendered project ticks. Repeated draws share one capture; missed ticks are not invented. Warmup is transparent."));
                        ui.small(crate::i18n::tr(ui,"All delayed frames share the 128 MiB filter budget. Large resolution, frame rate or delay may be rejected; quality is never reduced automatically."));
                        ui.small(crate::i18n::tr(ui,"Hiding, stopping, seeking or rebinding a source retires its private history. Resuming starts a fresh queue."));
                    },
                    VideoEffect::ScaleFilter{target,sampling,undistort}=>super::scale_filter::editor(ui,model,target,sampling,undistort),
                    VideoEffect::Scroll {speed_x,speed_y,limit_width,limit_height,width,height,looping}=>{
                        craft_slider(ui,"Horizontal Speed",speed_x,-500.0,500.0,1.0,"Horizontal Scroll speed in pixels per second. Positive values move source content left; zero resets this axis.");
                        craft_slider(ui,"Vertical Speed",speed_y,-500.0,500.0,1.0,"Vertical Scroll speed in pixels per second. Positive values move source content up; zero resets this axis.");
                        ui.checkbox(limit_width,crate::i18n::tr(ui,"Limit Width")).on_hover_text(crate::i18n::tr(ui,"Override the Scroll output width without resizing the input. Other filters receive this width."));
                        if *limit_width{let label=ui.label(crate::i18n::tr(ui,"Width"));ui.add(egui::DragValue::new(width).range(1..=8192).clamp_existing_to_range(false).suffix(" px")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set the Scroll output width from 1 to 8192 pixels."));}
                        ui.checkbox(limit_height,crate::i18n::tr(ui,"Limit Height")).on_hover_text(crate::i18n::tr(ui,"Override the Scroll output height without resizing the input. Other filters receive this height."));
                        if *limit_height{let label=ui.label(crate::i18n::tr(ui,"Height"));ui.add(egui::DragValue::new(height).range(1..=8192).clamp_existing_to_range(false).suffix(" px")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set the Scroll output height from 1 to 8192 pixels."));}
                        ui.checkbox(looping,crate::i18n::tr(ui,"Loop")).on_hover_text(crate::i18n::tr(ui,"Wrap both source axes while scrolling. When disabled, pixels outside the source are transparent."));
                        ui.small(crate::i18n::tr(ui,"Scroll · CPU · SDR · linear sampling"));
                        ui.small(crate::i18n::tr(ui,"Showing the source restarts Scroll. Speed edits retain phase; zero resets an axis. Media seeking keeps Scroll independent."));
                    }
                    VideoEffect::ApplyLut {amount,passthrough_alpha}=>{
                        craft_slider(ui,"Amount",amount,0.0,1.0,0.0001,"Blend original and LUT colors in linear light using the OBS amount control.");
                        ui.checkbox(passthrough_alpha,crate::i18n::tr(ui,"Passthrough Alpha"))
                            .on_hover_text(crate::i18n::tr(ui,"At full strength and full domain, map premultiplied RGB directly. Other OBS branches restore straight RGB first; source alpha remains unchanged."));
                        let label=ui.label(crate::i18n::tr(ui,"LUT file"));
                        ui.add_enabled(!row.lut_loading,egui::TextEdit::singleline(&mut row.lut_path).char_limit(4096).desired_width(ui.available_width()))
                            .labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Lut.AtlasFileHelp","Absolute .cube or static OBS PNG LUT path. An empty path bypasses the filter. Combined 1D and 3D CUBE files are supported; HDR and animated images are unsupported."));
                        if row.lut_path.is_empty(){row.lut_error=None;}
                        ui.small(crate::i18n::tr(ui,"Apply LUT · SDR · linear 1D / trilinear 3D"));
                        ui.small(crate::i18n::tr_key_or(ui,"SkyLive.Lut.AtlasLayoutHelp","CUBE 1D: 2..65536 entries; CUBE 3D: 2..128 per axis. Image LUT: OBS 64³ PNG atlas with dimensions divisible by 64. Reload and Apply after editing the file."));
                        if let Some(asset)=row.filter.lut_asset().filter(|asset|asset.path()==row.lut_path){ui.label(crate::i18n::tf(ui,"Prepared LUT: {size} · {format}",&[("{size}",asset.size().to_string()),("{format}",match asset.format(){skylive_core::LutFormat::Cube1d=>"CUBE1D",skylive_core::LutFormat::Cube3d=>"CUBE3D",skylive_core::LutFormat::CubeShaper3d=>"CUBE1D+3D",skylive_core::LutFormat::ObsPng64=>"PNG64³"}.into())]));}
                        if row.lut_loading{ui.spinner();}if let Some(error)=&row.lut_error{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tf(ui,"LUT error: {detail}",&[("{detail}",crate::i18n::tr(ui,error))]));}
                    }
                    VideoEffect::ImageMask {method,color,opacity,stretch}=>{
                        egui::ComboBox::from_id_salt("image-mask-method").selected_text(crate::i18n::tr(ui,method.label())).show_ui(ui,|ui|{
                            for value in skylive_core::ImageMaskMethod::ALL {ui.selectable_value(method,value,crate::i18n::tr(ui,value.label()));}
                        });
                        ui.horizontal(|ui|{ui.label(crate::i18n::tr(ui,"Color"));ui.color_edit_button_srgb(color);});
                        craft_slider(ui,"Opacity",opacity,0.0,1.0,0.01,"Scale source alpha before applying the mask or blend.");
                        ui.checkbox(stretch,crate::i18n::tr(ui,"Stretch Image (discard image aspect ratio)")).on_hover_text(crate::i18n::tr(ui,"Disable stretching to center the mask at its original aspect ratio with clamped edges."));
                        let label=ui.label(crate::i18n::tr(ui,"Mask image"));
                        ui.add(egui::TextEdit::singleline(&mut row.mask_path).char_limit(4096).desired_width(ui.available_width())).labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.ImageMask.AnimationFormats","PNG/APNG, JPEG, BMP, TGA, WebP or GIF. Animated GIF, APNG and WebP masks are supported. Changes reload automatically when dimensions stay the same. Empty path bypasses the filter."));
                        if row.mask_path.is_empty(){row.mask_error=None;}
                        ui.label(crate::i18n::tr(ui,"Choose a mask, then Load / Refresh. Apply saves its exact content identity; Cancel retains the original filter."));
                        ui.label(crate::i18n::tr(ui,"Image Mask/Blend · CPU · SDR"));
                        if let Some(asset)=row.filter.mask_image().filter(|a|a.path()==row.mask_path) {
                            ui.label(crate::i18n::tf(ui,"Prepared mask: {width} × {height}",&[("{width}",asset.width().to_string()),("{height}",asset.height().to_string())]));
                        }
                        if row.mask_loading {ui.spinner();}
                        if let Some(error)=&row.mask_error {ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tf(ui,"Mask image error: {detail}",&[("{detail}",crate::i18n::tr(ui,error))]));}
                    }
                    VideoEffect::GaussianBlur{blurriness,dimensions,repeat_edge}=>{
                        ui.label(crate::i18n::tr(ui,"EffectCraft Gaussian Blur · CPU"));
                        let label=ui.label(crate::i18n::tr(ui,"Blurriness"));
                        // The pointer range is intentionally narrower than the
                        // model's editable range. Bound only an actual edit so
                        // a valid value above 50 remains unchanged on redraw.
                        let previous=*blurriness;
                        let response=ui.add(egui::Slider::new(blurriness,0.0..=50.0).clamping(egui::SliderClamping::Never)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set blur in pixels. You can enter values up to 3000; large radii use more time and memory."));
                        if response.changed(){*blurriness=if blurriness.is_finite(){(*blurriness).clamp(0.0,3000.0)}else{previous};}
                        let caption=ui.label(crate::i18n::tr(ui,"Blur Dimensions"));
                        egui::ComboBox::from_id_salt("gaussian-dimensions").selected_text(crate::i18n::tr(ui,dimensions.label())).show_ui(ui,|ui|{for value in skylive_core::GaussianBlurDimensions::ALL{ui.selectable_value(dimensions,value,crate::i18n::tr(ui,value.label()));}}).response.labelled_by(caption.id);
                        ui.checkbox(repeat_edge,crate::i18n::tr(ui,"Repeat Edge Pixels")).on_hover_text(crate::i18n::tr(ui,"Extend edge pixels instead of adding transparent padding around the source."));
                    }
                    VideoEffect::Vignette {amount,angle_of_view,center,pin_highlights}=>{
                        ui.label(crate::i18n::tr(ui,"EffectCraft lens falloff · CPU · SDR"));
                        craft_slider(ui,"Vignette Amount",amount,-100.0,100.0,1.0,"Positive values darken the edges; negative values brighten them. Zero bypasses this effect.");
                        craft_slider(ui,"Angle of View",angle_of_view,0.0,179.0,1.0,"Set the lens angle in degrees. Wider angles strengthen the edge falloff; zero bypasses this effect.");
                        craft_slider(ui,"Vignette Center X (%)",&mut center[0],-800.0,800.0,0.1,"Set the horizontal center as a percentage of the current input width. 50% is centered; off-frame values are allowed.");
                        craft_slider(ui,"Vignette Center Y (%)",&mut center[1],-800.0,800.0,0.1,"Set the vertical center as a percentage of the current input height. 50% is centered; off-frame values are allowed.");
                        craft_slider(ui,"Pin Highlights",pin_highlights,0.0,100.0,1.0,"Reduce the vignette on bright linear-light areas while preserving the source alpha.");
                        ui.label(crate::i18n::tr(ui,"The center follows the image size at this filter position, after any earlier Crop/Pad or Scaling filter."));
                    }
                    VideoEffect::Invert{amount}|VideoEffect::Grayscale{amount}=>{craft_slider(ui,"Amount",amount,0.0,100.0,1.0,"Set the effect blend from the original source to the full result.");}
                    VideoEffect::PhotoFilter{preset,custom_color,density,preserve_luminosity}=>photo_filter_controls(ui,preset,custom_color,density,preserve_luminosity),
                    VideoEffect::Sepia{density,preserve_luminosity}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.SepiaDescription","EffectCraft Sepia photo-filter tint · CPU/GPU · alpha unchanged"));
                        craft_slider(ui,"Density",density,0.0,100.0,1.0,"Blend the fixed warm Sepia photo-filter color into the original RGB. Zero leaves the source unchanged.");
                        ui.checkbox(preserve_luminosity,crate::i18n::tr(ui,"Preserve Luminosity")).on_hover_text(crate::i18n::tr(ui,"Preserve Rec.709 working-space luminance using EffectCraft’s original photo-filter normalization. SDR clipping still applies."));
                    }
                    VideoEffect::Posterize{levels}=>{craft_integer(ui,"Levels",levels,2,255,"Choose how many discrete RGB levels remain.");}
                    VideoEffect::Threshold{level}=>{craft_integer(ui,"Threshold Level",level,1,255,"Set the luminosity level separating black and white.");}
                    VideoEffect::CraftGammaPedestalGain{gamma,pedestal,gain,black_stretch}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.GpgDescription","Adjust gamma, pedestal and gain for each color channel · CPU/GPU · alpha unchanged"));
                        let stretch=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.GpgBlackStretch","Black Stretch"));
                        ui.add_sized([ui.available_width(),22.0],egui::Slider::new(black_stretch,0.0..=4.0).step_by(0.01)).labelled_by(stretch.id)
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.GpgStretchHint","Lift dark values while preserving black and white endpoints."));
                        for (i,(key,title)) in [("SkyLive.Craft.GpgRed","Red"),("SkyLive.Craft.GpgGreen","Green"),("SkyLive.Craft.GpgBlue","Blue")].into_iter().enumerate(){
                            ui.separator();ui.label(crate::i18n::tr_key_or(ui,key,title));
                            for (caption,value,min,max) in [("SkyLive.Craft.GpgGamma",&mut gamma[i],0.1,10.0),("SkyLive.Craft.GpgPedestal",&mut pedestal[i],-2.0,2.0),("SkyLive.Craft.GpgGain",&mut gain[i],0.0,4.0)]{
                                let title=match caption {"SkyLive.Craft.GpgGamma"=>"Gamma","SkyLive.Craft.GpgPedestal"=>"Pedestal",_=>"Gain"};
                                let label=ui.label(crate::i18n::tr_key_or(ui,caption,title));
                                ui.add_sized([ui.available_width(),22.0],egui::Slider::new(value,min..=max).step_by(0.01)).labelled_by(label.id)
                                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.GpgChannelHint","Adjust this channel using EffectCraft's gamma, pedestal and gain curve."));
                            }
                        }
                    }
                    VideoEffect::CraftChannelMixer{matrix,monochrome}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.ChannelMixerDescription","Mix RGB channels and offsets · CPU/GPU · alpha unchanged"));
                        let rows=[("SkyLive.Craft.ChannelMixerRedOutput","Red output"),("SkyLive.Craft.ChannelMixerGreenOutput","Green output"),("SkyLive.Craft.ChannelMixerBlueOutput","Blue output")];
                        let columns=[("SkyLive.Craft.ChannelMixerRedInput","Red input"),("SkyLive.Craft.ChannelMixerGreenInput","Green input"),("SkyLive.Craft.ChannelMixerBlueInput","Blue input"),("SkyLive.Craft.ChannelMixerOffset","Constant")];
                        ui.checkbox(monochrome,crate::i18n::tr_key_or(ui,"SkyLive.Craft.ChannelMixerMonochrome","Monochrome"))
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.ChannelMixerMonochromeHint","Use the red output mix for all three channels."));
                        for (index,(values,(key,title))) in matrix.iter_mut().zip(rows).enumerate(){
                            ui.add_enabled_ui(!*monochrome||index==0,|ui|{
                                ui.label(crate::i18n::tr_key_or(ui,key,title));
                                for (value,(key,title)) in values.iter_mut().zip(columns){
                                    let label=ui.label(crate::i18n::tr_key_or(ui,key,title));
                                    ui.add_sized([ui.available_width(),22.0],egui::Slider::new(value,-200.0..=200.0).clamping(egui::SliderClamping::Edits).step_by(1.0).suffix(" %")).labelled_by(label.id)
                                        .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.ChannelMixerSliderHint","Set how much this input channel contributes to the output, or adjust its constant offset."));
                                }
                            });
                        }
                    }
                    VideoEffect::CraftColorBalance{shadows,midtones,highlights,preserve_luminosity}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.ColorBalanceDescription","EffectCraft Color Balance · CPU/GPU · SDR · alpha unchanged"));
                        for (values,key,title) in [(shadows,"SkyLive.Craft.ColorBalanceShadows","Shadows"),(midtones,"SkyLive.Craft.ColorBalanceMidtones","Midtones"),(highlights,"SkyLive.Craft.ColorBalanceHighlights","Highlights")] {
                            ui.label(crate::i18n::tr_key_or(ui,key,title));
                            for (value,key,name) in values.iter_mut().zip(["SkyLive.Craft.ColorBalanceRed","SkyLive.Craft.ColorBalanceGreen","SkyLive.Craft.ColorBalanceBlue"]).zip(["Red","Green","Blue"]).map(|((v,k),n)|(v,k,n)) {
                                let label=ui.label(crate::i18n::tr_key_or(ui,key,name));
                                ui.add_sized([ui.available_width(),22.0],egui::Slider::new(value,-100.0..=100.0).clamping(egui::SliderClamping::Edits).step_by(1.0)).labelled_by(label.id)
                                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.ColorBalanceSliderHint","Adjust this color in the selected tonal range."));
                            }
                        }
                        ui.checkbox(preserve_luminosity,crate::i18n::tr_key_or(ui,"SkyLive.Craft.ColorBalancePreserve","Preserve luminosity"))
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.ColorBalancePreserveHint","Retain the original luminance while changing color balance."));
                    }
                    VideoEffect::Tritone{shadows,midtones,highlights,blend}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TritoneDescription","EffectCraft Tritone · CPU/GPU · SDR · alpha unchanged"));
                        for (color,key,label,hint_key,hint) in [(shadows,"SkyLive.Craft.TritoneShadows","Shadows","SkyLive.Craft.TritoneShadowsHint","Choose the color for dark tones."),(midtones,"SkyLive.Craft.TritoneMidtones","Midtones","SkyLive.Craft.TritoneMidtonesHint","Choose the color for middle tones."),(highlights,"SkyLive.Craft.TritoneHighlights","Highlights","SkyLive.Craft.TritoneHighlightsHint","Choose the color for bright tones.")] {ui.horizontal_wrapped(|ui|{let text=ui.label(crate::i18n::tr_key_or(ui,key,label));ui.color_edit_button_srgb(color).labelled_by(text.id).on_hover_text(crate::i18n::tr_key_or(ui,hint_key,hint));});}
                        let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TritoneBlend","Blend With Original"));ui.add_sized([ui.available_width(),22.0],egui::Slider::new(blend,0.0..=100.0).clamping(egui::SliderClamping::Edits).step_by(1.0)).labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TritoneBlendHint","Set how much original image to retain; 100 leaves the source unchanged."));
                    }
                    VideoEffect::CraftTint{black,white,amount}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintDescription","EffectCraft Tint · CPU/GPU · SDR · alpha unchanged"));
                        ui.horizontal_wrapped(|ui|{let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintBlack","Map Black To"));ui.color_edit_button_srgb(black).labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintBlackHint","Select the color used for black tones."));});
                        ui.horizontal_wrapped(|ui|{let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintWhite","Map White To"));ui.color_edit_button_srgb(white).labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintWhiteHint","Select the color used for white tones."));});
                        let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintAmount","Amount to Tint"));ui.add_sized([ui.available_width(),22.0],egui::Slider::new(amount,0.0..=100.0).clamping(egui::SliderClamping::Edits).step_by(1.0)).labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintAmountHint","Mix the mapped colors with the original image. Zero leaves the source unchanged."));
                    }
                    VideoEffect::Vibrance{vibrance,saturation}=>{
                        ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.VibranceDescription","EffectCraft Vibrance · CPU/GPU · SDR · alpha unchanged"));
                        let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Vibrance","Vibrance"));
                        ui.add_sized([ui.available_width(),22.0],egui::Slider::new(vibrance,-100.0..=100.0).clamping(egui::SliderClamping::Edits).step_by(1.0)).labelled_by(label.id)
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.VibranceHint","Adjust less saturated colors more strongly using EffectCraft's vibrance curve."));
                        let label=ui.label(crate::i18n::tr(ui,"Saturation"));
                        ui.add_sized([ui.available_width(),22.0],egui::Slider::new(saturation,-100.0..=100.0).clamping(egui::SliderClamping::Edits).step_by(1.0)).labelled_by(label.id)
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.VibranceSaturationHint","Adjust the overall HSL saturation. Zero preserves the current saturation."));
                    }
                    VideoEffect::Exposure{exposure,offset,gamma,bypass_linear}=>{
                        craft_slider(ui,"Exposure Stops",exposure,-20.0,20.0,0.05,"One stop doubles or halves linear-light exposure.");
                        craft_slider(ui,"Exposure Offset",offset,-0.5,0.5,0.001,"Add an offset before exposure gamma correction.");
                        craft_slider(ui,"Exposure Gamma",gamma,0.01,9.99,0.01,"Adjust gamma after exposure and offset.");
                        ui.checkbox(bypass_linear,crate::i18n::tr(ui,"Bypass Linear Light Conversion")).on_hover_text(crate::i18n::tr(ui,"Apply exposure directly to encoded values instead of the signed2.2 conversion."));
                    }
                    VideoEffect::TemperatureTint{temperature,tint}=>{
                        craft_slider(ui,"Temperature",temperature,-100.0,100.0,1.0,"Balance red and blue channels toward warm or cool colors.");
                        craft_slider(ui,"Tint",tint,-100.0,100.0,1.0,"Balance the green channel toward green or magenta.");
                    }
                    VideoEffect::PersonBackgroundRemoval {threshold,softness}=>{
                        ui.label(crate::i18n::tr(ui,"Fixed Selfie256 model · CPU · person foreground mask"));
                        let label=ui.label(crate::i18n::tr(ui,"AI refresh interval (ms)"));
                        ui.add(egui::DragValue::new(&mut person_interval).range(0..=5000).speed(1.0).suffix(" ms")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Zero recalculates every frame. For cameras, a larger interval reuses the last person mask on newer frames and can reduce CPU use; edges may lag during motion."));
                        ui.small(crate::i18n::tr(ui,"If this filter chain contains Freeze or Render Delay, AI still recalculates every frame."));
                        ui.label(crate::i18n::tr(ui,"Defaults preserve the model probabilities. Threshold shifts the matte; softness controls its transition width."));
                        egui::Grid::new("person-background-values").show(ui,|ui|{
                            person_number(ui,"Person Threshold",threshold,0.0,1.0,"Set the foreground probability at the middle of the alpha transition.");
                            person_number(ui,"Mask Softness",softness,0.001,1.0,"Set the probability transition width. Default 1.0 preserves raw probabilities at threshold 0.5.");
                        });
                    }
                    VideoEffect::ColorKey{key_color_type,key_color,similarity,smoothness,opacity,contrast,brightness,gamma}=>{
                        let mut selected=*key_color_type as usize;
                        key_type(ui,&mut selected,&["Green","Blue","Red","Magenta","Custom"]);
                        *key_color_type=[ColorKeyType::Green,ColorKeyType::Blue,ColorKeyType::Red,ColorKeyType::Magenta,ColorKeyType::Custom][selected];
                        key_values(ui,key_color,*key_color_type==ColorKeyType::Custom,similarity,smoothness,None,opacity,contrast,brightness,gamma);
                    }
                    VideoEffect::ChromaKey{key_color_type,key_color,similarity,smoothness,spill,opacity,contrast,brightness,gamma}=>{
                        let mut selected=*key_color_type as usize;
                        key_type(ui,&mut selected,&["Green","Blue","Magenta","Custom"]);
                        *key_color_type=[ChromaKeyType::Green,ChromaKeyType::Blue,ChromaKeyType::Magenta,ChromaKeyType::Custom][selected];
                        key_values(ui,key_color,*key_color_type==ChromaKeyType::Custom,similarity,smoothness,Some(spill),opacity,contrast,brightness,gamma);
                    }
                    VideoEffect::CropPad {relative,left,top,right,bottom,width,height}=>{
                        let was_relative=*relative;
                        let current=VideoEffect::CropPad {relative:true,left:*left,top:*top,right:*right,bottom:*bottom,width:*width,height:*height};
                        let initial_size=input_dimensions.and_then(|input|current.output_dimensions_with_canvas(input,canvas).ok());
                        let can_change=!*relative||(*width!=0&&*height!=0)||initial_size.is_some();
                        ui.add_enabled(can_change,egui::Checkbox::new(relative,crate::i18n::tr(ui,"Relative")))
                            .on_hover_text(crate::i18n::tr(ui,"Use signed edge offsets; disable to set X, Y and an absolute output size."))
                            .on_disabled_hover_text(crate::i18n::tr(ui,"Cannot determine the filter input size yet. Wait for the source preview before switching to absolute size."));
                        if was_relative && !*relative && (*width==0 || *height==0) {
                            // Initial absolute size is the existing relative output,
                            // not the prototype's zero-by-zero (rendered as 1×1).
                            if let Some((w,h))=initial_size {
                                if *width==0 {*width=w;} if *height==0 {*height=h;}
                            }
                        }
                        ui.label(crate::i18n::tr(ui,"Positive offsets crop pixels; negative offsets add transparent padding. Output axes are at least one pixel."));
                        egui::Grid::new("obs-crop-pad-fields").show(ui,|ui|{
                            for (caption,value) in [(if *relative {"Left"}else{"X"},left),(if *relative {"Top"}else{"Y"},top)] {
                                let label=grid_label(ui,caption);ui.add(egui::DragValue::new(value).range(-8192..=8192).speed(1.0)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Move the source sampling origin in whole pixels; negative values add transparent padding."));ui.end_row();
                            }
                            if *relative {
                                for (caption,value) in [("Right",right),("Bottom",bottom)] {
                                    let label=grid_label(ui,caption);ui.add(egui::DragValue::new(value).range(-8192..=8192).speed(1.0)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Remove pixels from this edge, or use a negative value to add transparent padding."));ui.end_row();
                                }
                            }else{
                                for (caption,value) in [("Width",width),("Height",height)] {
                                    let label=grid_label(ui,caption);ui.add(egui::DragValue::new(value).range(0..=8192).speed(1.0)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set the absolute output dimension; zero produces one pixel, matching OBS."));ui.end_row();
                                }
                            }
                        });
                        ui.label(crate::i18n::tr(ui,"Crop/Pad changes displayed source dimensions without changing camera or capture settings. CPU processing; 8192 pixels per axis and 32 million pixels maximum."));
                    }
                    VideoEffect::LumaKey {luma_max,luma_min,luma_max_smooth,luma_min_smooth,..}=>{
                        craft_slider(ui,"Luma Max",luma_max,0.0,1.0,0.0001,"Set the upper luminance cutoff in linear light (0 to 1).");
                        craft_slider(ui,"Luma Max Smooth",luma_max_smooth,0.0,1.0,0.0001,"Soften the upper luminance cutoff (0 is a hard edge).");
                        craft_slider(ui,"Luma Min",luma_min,0.0,1.0,0.0001,"Set the lower luminance cutoff in linear light (0 to 1).");
                        craft_slider(ui,"Luma Min Smooth",luma_min_smooth,0.0,1.0,0.0001,"Soften the lower luminance cutoff (0 is a hard edge).");
                    }
                    VideoEffect::Sharpen {sharpness}=>{
                        craft_slider(ui,"Sharpness",sharpness,0.0,1.0,0.01,"Set OBS edge sharpening strength from 0 to 1; the default is 0.08.");
                    }
                    VideoEffect::ColorCorrection {gamma,contrast,brightness,saturation,hue_shift,opacity,color_multiply,color_add}=>{
                        ui.label(crate::i18n::tr(ui,"Color Correction v2 applies to SDR video. HDR correction and legacy v1 are not supported."));
                        egui::Grid::new("obs-color-correction-values").show(ui,|ui|{
                            precise_number(ui,"Gamma",gamma,-3.0,3.0,0.01,"Adjust the gamma exponent in linear light.");
                            precise_number(ui,"Contrast",contrast,-4.0,4.0,0.01,"Scale linear-light contrast around black.");
                            precise_number(ui,"Brightness",brightness,-1.0,1.0,0.0001,"Add a linear-light brightness offset.");
                            precise_number(ui,"Saturation",saturation,-1.0,5.0,0.01,"Adjust color saturation using OBS luminance weights.");
                            precise_number(ui,"Hue Shift",hue_shift,-180.0,180.0,0.01,"Rotate hue around the RGB diagonal in degrees.");
                            precise_number(ui,"Opacity",opacity,0.0,1.0,0.0001,"Set the filtered source opacity from zero to one.");
                        });
                        for (caption,color,hint) in [("Color Multiply",color_multiply,"Multiply RGB by this color in linear light."),("Color Add",color_add,"Add this color in linear light.")] {
                            ui.horizontal(|ui|{let name=crate::i18n::tr(ui,caption);let label=ui.label(&name);
                                let response=ui.color_edit_button_srgb(color).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,hint));
                                let info=egui::WidgetInfo::labeled(egui::WidgetType::ColorButton,ui.is_enabled(),name);response.widget_info(||info.clone());});
                        }
                    }
                    VideoEffect::LinearColorKey{color,matching,tolerance,softness,keep,view}=>{
                        ui.horizontal_wrapped(|ui|{let name=crate::i18n::tr(ui,"Key Color");let label=ui.label(&name);let response=ui.color_edit_button_srgb(color).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the background color to remove or keep."));let info=egui::WidgetInfo::labeled(egui::WidgetType::ColorButton,ui.is_enabled(),name);response.widget_info(||info.clone());});
                        ui.label(crate::i18n::tr(ui,"Matching"));ui.horizontal_wrapped(|ui|{for(value,label)in[(KeyMatch::Rgb,"RGB"),(KeyMatch::Hue,"Hue"),(KeyMatch::Chroma,"Chroma")]{ui.selectable_value(matching,value,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"Choose which color components are compared with the key color."));}});
                        egui::Grid::new("key-amounts").show(ui,|ui|{number(ui,"Tolerance",tolerance,0.0,100.0,"%","Set the range of colors treated as a match.");number(ui,"Softness",softness,0.0,100.0,"%","Soften the edge of the color match.");});
                        ui.checkbox(keep,crate::i18n::tr(ui,"Keep Keyed Colors")).on_hover_text(crate::i18n::tr(ui,"Keep matching colors instead of removing them."));
                        ui.label(crate::i18n::tr(ui,"View"));ui.horizontal_wrapped(|ui|{for(value,label)in[(MatteView::Final,"Final Output"),(MatteView::Source,"Source"),(MatteView::Matte,"Matte")]{ui.selectable_value(view,value,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"Display the filtered result, original source, or alpha matte."));}});
                    }
                    VideoEffect::BrightnessContrast{brightness,contrast,legacy}=>{
                        egui::Grid::new("brightness-contrast-amounts").show(ui,|ui|{number(ui,"Brightness",brightness,-150.0,150.0,"","Adjust the brightness of this source.");number(ui,"Contrast",contrast,-100.0,100.0,"%","Adjust the contrast of this source.");});
                        ui.checkbox(legacy,crate::i18n::tr(ui,"Use Legacy")).on_hover_text(crate::i18n::tr(ui,"Use the legacy brightness and contrast calculation."));
                    }
                    VideoEffect::SpillSuppression{channel,balance,amount}=>{
                        ui.label(crate::i18n::tr(ui,"Spill Color"));ui.horizontal_wrapped(|ui|{for(value,label)in[(SpillChannel::Green,"Green"),(SpillChannel::Blue,"Blue")]{ui.selectable_value(channel,value,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"Choose the background color reflected onto foreground edges."));}});
                        egui::Grid::new("spill-amounts").show(ui,|ui|{number(ui,"Balance",balance,0.0,100.0,"%","Adjust the balance used to identify color spill.");number(ui,"Amount",amount,0.0,100.0,"%","Set how much color spill is removed.");});
                    }
                }
                if matches!(row.filter.effect,VideoEffect::PersonBackgroundRemoval{..}){let _=row.filter.set_person_interval_ms(person_interval);}
                if ui.add_enabled(!row.mask_loading&&!row.lut_loading,egui::Button::new(crate::i18n::tr(ui,"Reset Filter")))
                    .on_hover_text(crate::i18n::tr(ui,"Restore this filter's parameter defaults without changing its enabled state.")).clicked(){
                        row.filter.reset_settings();row.mask_path.clear();row.mask_error=None;row.lut_path.clear();row.lut_error=None;
                    }
            });
        });
    });
}
fn photo_filter_controls(ui:&mut egui::Ui,preset:&mut skylive_core::PhotoFilterPreset,custom_color:&mut[f32;3],density:&mut f32,preserve_luminosity:&mut bool){
    use skylive_core::PhotoFilterPreset;
    ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.PhotoFilterDescription","EffectCraft Photo Filter · CPU/GPU · SDR · alpha unchanged"));
    let label=ui.label(crate::i18n::tr(ui,"Photo Filter Preset"));
    egui::ComboBox::from_id_salt("craft-photo-filter-preset").selected_text(crate::i18n::tr(ui,preset.label())).show_ui(ui,|ui|{
        for value in PhotoFilterPreset::ALL{ui.selectable_value(preset,value,crate::i18n::tr(ui,value.label()));}
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose an exact EffectCraft photographic tint, or Custom to edit the retained color."));
    if *preset==PhotoFilterPreset::Custom{
        ui.horizontal_wrapped(|ui|{
            let label=ui.label(crate::i18n::tr(ui,"Custom Color"));
            // egui's float RGB picker means LINEAR RGB; these authored values
            // are encoded working RGB. Use a temporary sRGB8 picker instead.
            // Commit only an actual edit: idle frames MUST NOT quantize floats.
            let mut picker=(*custom_color).map(|value|(value.clamp(0.0,1.0)*255.0).round()as u8);
            if ui.color_edit_button_srgb(&mut picker).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"The swatch edits an 8-bit encoded color. Use the RGB number fields for fractional channel precision.")).changed(){*custom_color=picker.map(|value|f32::from(value)/255.0);}
        });
        for(index,(value,caption))in custom_color.iter_mut().zip(["Custom Red","Custom Green","Custom Blue"]).enumerate(){ui.push_id(("photo-custom-channel",index),|ui|craft_slider(ui,caption,value,0.0,1.0,0.001,"Edit the custom encoded-sRGB channel from 0 to 1. It is used only when Custom is selected."));}
    }else if let Some(color)=preset.rgb8(){
        ui.horizontal_wrapped(|ui|{
            ui.label(crate::i18n::tr(ui,"Preset Color"));
            let(rect,response)=ui.allocate_exact_size(egui::vec2(28.0,18.0),egui::Sense::hover());
            ui.painter().rect_filled(rect,2.0,egui::Color32::from_rgb(color[0],color[1],color[2]));
            response.on_hover_text(crate::i18n::tr(ui,"This preset fixes the tint. Select Custom to edit a color; the previous custom value is retained."));
            ui.label(format!("#{:02X}{:02X}{:02X}",color[0],color[1],color[2]));
        });
        ui.label(crate::i18n::tr(ui,"Preset colors ignore but retain the custom color. Switching to Custom restores that value."));
    }
    ui.push_id("photo-density",|ui|craft_slider(ui,"Density",density,0.0,100.0,1.0,"Blend the selected photo-filter color into the original RGB. Zero leaves the source unchanged."));
    ui.push_id("photo-preserve-luminosity",|ui|{ui.checkbox(preserve_luminosity,crate::i18n::tr(ui,"Preserve Luminosity")).on_hover_text(crate::i18n::tr(ui,"Preserve Rec.709 working-space luminance using EffectCraft’s original photo-filter normalization. SDR clipping still applies."));});
}

fn craft_slider(ui:&mut egui::Ui,caption:&str,value:&mut f32,min:f32,max:f32,step:f64,hint:&str) {
    let label=ui.label(crate::i18n::tr(ui,caption));
    ui.add_sized([ui.available_width(),22.0],egui::Slider::new(value,min..=max).clamping(egui::SliderClamping::Edits).step_by(step)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,hint)).on_disabled_hover_text(crate::i18n::tr(ui,hint));
}
fn craft_integer(ui:&mut egui::Ui,caption:&str,value:&mut u16,min:u16,max:u16,hint:&str) {
    let label=ui.label(crate::i18n::tr(ui,caption));
    ui.add_sized([ui.available_width(),22.0],egui::Slider::new(value,min..=max).integer().clamping(egui::SliderClamping::Edits)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,hint)).on_disabled_hover_text(crate::i18n::tr(ui,hint));
}
fn person_number(
    ui: &mut egui::Ui,
    caption: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    hint: &str,
) {
    let label = grid_label(ui, caption);
    let response = ui
        .add(
            egui::DragValue::new(value)
                .range(min..=max)
                .clamp_existing_to_range(false)
                .speed(0.01)
                .max_decimals(3),
        )
        .labelled_by(label.id)
        .on_hover_text(crate::i18n::tr(ui, hint))
        .on_disabled_hover_text(crate::i18n::tr(ui, hint));
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("person-filter-value", caption)),
            response.rect,
        );
        data.insert_temp(
            egui::Id::new(("person-filter-enabled", caption)),
            response.enabled(),
        );
    });
    let _ = response;
    ui.end_row();
}

fn key_type(ui: &mut egui::Ui, selected: &mut usize, labels: &[&str]) {
    ui.label(crate::i18n::tr(ui, "Key Color Type"));
    ui.horizontal_wrapped(|ui| {
        for (index, label) in labels.iter().enumerate() {
            ui.selectable_value(selected, index, crate::i18n::tr(ui, label))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Choose a preset background color or use the retained custom color.",
                ));
        }
    });
}
#[allow(clippy::too_many_arguments)] // Shared complete OBS keyer property group, editing the authored values directly.
fn key_values(
    ui: &mut egui::Ui,
    color: &mut [u8; 3],
    custom: bool,
    similarity: &mut u16,
    smoothness: &mut u16,
    spill: Option<&mut u16>,
    opacity: &mut f32,
    contrast: &mut f64,
    brightness: &mut f32,
    gamma: &mut f64,
) {
    ui.add_enabled_ui(custom, |ui| {
        ui.horizontal(|ui| {
            let name = crate::i18n::tr(ui, "Key Color");
            let label = ui.label(&name);
            let response = ui
                .color_edit_button_srgb(color)
                .labelled_by(label.id)
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Choose the custom key color. Presets preserve this value for later use.",
                ));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::ColorButton,
                    ui.is_enabled(),
                    name.clone(),
                )
            });
        });
    });
    ui.label(crate::i18n::tr(ui,"OBS keyers apply to SDR video. Chroma Key samples one source-pixel neighborhood; large or repeated stages may exceed the work budget."));
    egui::Grid::new("obs-keyer-values").show(ui, |ui| {
        precise_number(
            ui,
            "Similarity",
            similarity,
            1,
            1000,
            1.0,
            "Set the OBS key similarity from 1 to 1000.",
        );
        precise_number(
            ui,
            "Smoothness",
            smoothness,
            1,
            1000,
            1.0,
            "Soften the OBS key transition from 1 to 1000.",
        );
        if let Some(spill) = spill {
            precise_number(
                ui,
                "Spill Reduction",
                spill,
                1,
                1000,
                1.0,
                "Reduce background chroma spill using the OBS mask.",
            );
        }
        precise_number(
            ui,
            "Opacity",
            opacity,
            0.0,
            1.0,
            0.0001,
            "Set the filtered source opacity from zero to one.",
        );
        precise_number(
            ui,
            "Contrast",
            contrast,
            -4.0,
            4.0,
            0.01,
            "Scale linear-light contrast around black.",
        );
        precise_number(
            ui,
            "Brightness",
            brightness,
            -1.0,
            1.0,
            0.0001,
            "Add a linear-light brightness offset.",
        );
        precise_number(
            ui,
            "Gamma",
            gamma,
            -1.0,
            1.0,
            0.01,
            "Adjust the gamma exponent in linear light.",
        );
    });
}

fn precise_number<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    caption: &str,
    value: &mut Num,
    min: Num,
    max: Num,
    step: f64,
    hint: &str,
) {
    let label = grid_label(ui, caption);
    let response = ui
        .add(
            egui::DragValue::new(value)
                .range(min.to_f64()..=max.to_f64())
                .speed(step)
                .max_decimals(4),
        )
        .labelled_by(label.id)
        .on_hover_text(crate::i18n::tr(ui, hint))
        .on_disabled_hover_text(crate::i18n::tr(ui, hint));
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(egui::Id::new(("obs-color-value", caption)), response.rect)
    });
    let _ = response;
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::dialogs::{self, Dialog};
    #[test]
    fn real_builtin_picker_hue_edit_and_apply_work_with_craft_disabled() {
        let (mut session, source) = fixture();
        session
            .apply(Command::SetPluginEnabled {
                plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
                enabled: false,
            })
            .unwrap();
        let context = egui::Context::default();
        crate::theme::install(&context);
        let before = session.snapshot();
        let mut slot = Some(Dialog::VideoFilters(
            VideoFilterDraft::capture(session.identity(), &before, source).unwrap(),
        ));
        click(&context, &session, &mut slot, "Add Filter");
        click(&context, &session, &mut slot, "Color Correction");
        assert!(
            matches!(&slot,Some(Dialog::VideoFilters(d)) if d.rows.len()==1 && matches!(d.rows[0].filter.effect,VideoEffect::ColorCorrection{..}))
        );
        for _ in 0..2 {
            frame(&context, &session, &mut slot, vec![]);
        }
        let rect = context
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("obs-color-value", "Hue Shift"))))
            .unwrap();
        for _ in 0..2 {
            for pressed in [true, false] {
                frame(
                    &context,
                    &session,
                    &mut slot,
                    vec![
                        egui::Event::PointerMoved(rect.center()),
                        egui::Event::PointerButton {
                            pos: rect.center(),
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        frame(
            &context,
            &session,
            &mut slot,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Text("120".into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(
            matches!(&slot,Some(Dialog::VideoFilters(d)) if matches!(d.rows[0].filter.effect,VideoEffect::ColorCorrection{hue_shift:120.0,..}))
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let intent = click(&context, &session, &mut slot, "Apply")
            .into_iter()
            .find(|i| matches!(i, Intent::ApplyVideoFilters { .. }))
            .unwrap();
        let Intent::ApplyVideoFilters {
            revision,
            source,
            filters,
            ..
        } = intent
        else {
            panic!("typed filter intent")
        };
        session
            .apply_at(
                Some(revision),
                Command::SetSourceVideoFilters { source, filters },
            )
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .len(),
            1
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .is_empty()
        );
    }
    pub(super) fn fixture() -> (skylive_core::Session, Id) {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Green screen".into(),
                rgba: [0, 255, 0, 255],
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        (session, source)
    }
    fn frame(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<Dialog>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<Intent>) {
        crate::i18n::install(
            context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut intents = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
        );
        (output, intents)
    }
    fn position(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| position(shape, label))
            }
            _ => None,
        }
    }
    pub(super) fn click(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<Dialog>,
        label: &str,
    ) -> Vec<Intent> {
        frame(context, session, slot, vec![]);
        let (output, _) = frame(context, session, slot, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                position(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .expect("video filter control is visible");
        frame(
            context,
            session,
            slot,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            context,
            session,
            slot,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .1
    }
    #[test]
    fn selected_filter_identity_moves_with_rows_and_settings_validate_before_any_publication() {
        let (mut session, source) = fixture();
        let before = session.snapshot();
        let mut draft = VideoFilterDraft::capture(session.identity(), &before, source).unwrap();
        draft.add(VideoEffect::color_key());
        draft.add(VideoEffect::brightness_contrast());
        draft.add(VideoEffect::spill_suppression());
        let selected = draft.selected;
        draft.rows.swap(1, 2);
        assert_eq!(draft.selected, selected);
        assert!(matches!(
            draft.rows[1].filter.effect,
            VideoEffect::SpillSuppression { .. }
        ));
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        if let VideoEffect::LinearColorKey { tolerance, .. } = &mut draft.rows[0].filter.effect {
            *tolerance = 101.0;
        }
        assert!(draft.command().is_err());
        if let VideoEffect::LinearColorKey { tolerance, .. } = &mut draft.rows[0].filter.effect {
            *tolerance = 10.0;
        }
        let intent = draft.intent().unwrap();
        let Intent::ApplyVideoFilters {
            session: identity,
            revision,
            source: target,
            filters,
        } = intent
        else {
            panic!("fenced video command")
        };
        assert_eq!(identity, session.identity());
        assert_eq!(target, source);
        assert_eq!(revision, before.revision());
        session
            .apply(Command::RenameSource {
                source,
                name: "Renamed".into(),
            })
            .unwrap();
        let changed = session.snapshot();
        assert!(
            session
                .apply_at(
                    Some(revision),
                    Command::SetSourceVideoFilters { source, filters }
                )
                .is_err()
        );
        assert!(std::sync::Arc::ptr_eq(&changed, &session.snapshot()));
    }
    #[test]
    fn actual_key_checkbox_cancel_stack_reorder_and_apply_are_one_undoable_edit() {
        let (mut session, source) = fixture();
        let before = session.snapshot();
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut slot = Some(Dialog::VideoFilters(
            VideoFilterDraft::capture(session.identity(), &before, source).unwrap(),
        ));
        click(&context, &session, &mut slot, "Add Filter");
        click(&context, &session, &mut slot, "Linear Color Key");
        click(&context, &session, &mut slot, "Keep Keyed Colors");
        assert!(
            matches!(&slot,Some(Dialog::VideoFilters(draft)) if matches!(draft.rows[0].filter.effect,VideoEffect::LinearColorKey{keep:true,..}))
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        click(&context, &session, &mut slot, "Cancel");
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let mut draft = VideoFilterDraft::capture(session.identity(), &before, source).unwrap();
        draft.add(VideoEffect::color_key());
        draft.add(VideoEffect::brightness_contrast());
        draft.add(VideoEffect::spill_suppression());
        let selected = draft.selected;
        if let VideoEffect::SpillSuppression { amount, .. } = &mut draft.rows[2].filter.effect {
            *amount = 25.0;
        }
        slot = Some(Dialog::VideoFilters(draft));
        click(&context, &session, &mut slot, "Reset Filter");
        assert!(
            matches!(&slot,Some(Dialog::VideoFilters(draft)) if matches!(draft.rows[2].filter.effect,VideoEffect::SpillSuppression{amount:100.0,..}))
        );
        click(&context, &session, &mut slot, "Move Filter Up");
        assert!(
            matches!(&slot,Some(Dialog::VideoFilters(draft)) if draft.selected==selected&&matches!(draft.rows[1].filter.effect,VideoEffect::SpillSuppression{..}))
        );
        let intent = click(&context, &session, &mut slot, "Apply")
            .into_iter()
            .find(|intent| matches!(intent, Intent::ApplyVideoFilters { .. }))
            .unwrap();
        let Intent::ApplyVideoFilters {
            revision,
            source,
            filters,
            ..
        } = intent
        else {
            panic!("apply")
        };
        session
            .apply_at(
                Some(revision),
                Command::SetSourceVideoFilters { source, filters },
            )
            .unwrap();
        assert_eq!(session.snapshot().revision(), before.revision() + 1);
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .len(),
            3
        );
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .is_empty()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .len(),
            3
        );
    }
    #[test]
    fn narrow_stack_actions_have_equal_centered_bounds_and_key_color_is_named() {
        let (session, source) = fixture();
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut draft =
            VideoFilterDraft::capture(session.identity(), &session.snapshot(), source).unwrap();
        draft.add(VideoEffect::color_key());
        draft.add(VideoEffect::spill_suppression());
        let mut area = egui::Rect::NOTHING;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(260.0, 650.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    area = ui.available_rect_before_wrap();
                    list(ui, &session.snapshot(), &mut draft, 220.0);
                });
            },
        );
        let rects: Vec<_> = (0..3)
            .map(|index| {
                context
                    .data_mut(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                    })
                    .unwrap()
            })
            .collect();
        for rect in &rects {
            assert!((rect.width() - rects[0].width()).abs() < 0.01);
            assert!(area.contains_rect(*rect));
            assert!((rect.center().x - area.center().x).abs() < 0.01);
        }
        context.enable_accesskit();
        draft.selected = Some(draft.rows[0].id);
        let mut slot = Some(Dialog::VideoFilters(draft));
        let (output, _) = frame(&context, &session, &mut slot, vec![]);
        let update = output.platform_output.accesskit_update.unwrap();
        let label = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Label && node.value() == Some("Key Color")
            })
            .map(|(id, _)| *id)
            .unwrap();
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.role() == egui::accesskit::Role::ColorWell
                    && node.label() == Some("Key Color")
                    && node.labelled_by().contains(&label))
        );
    }
}

#[cfg(test)]
mod keyer_editor_tests {
    use super::*;
    #[test]
    fn source_and_scene_drafts_preserve_all_keyer_values_and_publish_only_on_apply() {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let before = session.snapshot();
        let mut draft =
            VideoFilterDraft::capture_scene(session.identity(), &before, scene).unwrap();
        assert!(before.scene_source(scene).is_none());
        draft.add(VideoEffect::chroma_key_v2());
        if let VideoEffect::ChromaKey {
            key_color_type,
            key_color,
            similarity,
            smoothness,
            spill,
            opacity,
            contrast,
            brightness,
            gamma,
        } = &mut draft.rows[0].filter.effect
        {
            *key_color_type = ChromaKeyType::Custom;
            *key_color = [12, 34, 56];
            *similarity = 123;
            *smoothness = 456;
            *spill = 789;
            *opacity = 0.8;
            *contrast = 0.1234567890123;
            *brightness = -0.1;
            *gamma = -0.234567890123;
        }
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        assert!(
            matches!(draft.intent().unwrap(),Intent::ApplySceneVideoFilters{scene:id,..}if id==scene)
        );
        session.apply(draft.command().unwrap()).unwrap();
        let model = session.snapshot();
        let filters = model.scene_source(scene).unwrap().video_filters();
        assert_eq!(filters[0].effect, draft.rows[0].filter.effect);
        let source_draft = VideoFilterDraft::capture(
            session.identity(),
            &model,
            model.scene_source(scene).unwrap().id(),
        )
        .unwrap();
        assert_eq!(source_draft.rows[0].filter.id(), filters[0].id());
        session.apply(Command::Undo).unwrap();
        assert!(session.snapshot().scene_source(scene).is_none());
    }
    #[test]
    fn real_builtin_picker_adds_each_keyer_with_craft_disabled() {
        use crate::views::dialogs::Dialog;
        for label in ["Color Key", "Chroma Key"] {
            let (mut session, source) = super::tests::fixture();
            session
                .apply(Command::SetPluginEnabled {
                    plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
                    enabled: false,
                })
                .unwrap();
            let context = egui::Context::default();
            crate::theme::install(&context);
            let mut slot = Some(Dialog::VideoFilters(
                VideoFilterDraft::capture(session.identity(), &session.snapshot(), source).unwrap(),
            ));
            super::tests::click(&context, &session, &mut slot, "Add Filter");
            super::tests::click(&context, &session, &mut slot, label);
            assert!(
                matches!(&slot,Some(Dialog::VideoFilters(d)) if d.rows.len()==1 && d.rows[0].filter.effect.name()==label)
            );
            assert!(
                session
                    .snapshot()
                    .source(source)
                    .unwrap()
                    .video_filters()
                    .is_empty()
            );
        }
    }
}

#[cfg(test)]
#[path = "person_background_dialog_tests.rs"]
mod person_background_dialog_tests;

/// Exact source/editor/row/configuration receipt. No pixel buffers cross into UI.
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct MaskKey {pub token:u64,pub session:u64,pub revision:u64,pub target:VideoFilterTarget,pub row:u32,pub path:String,name:String,signature:Vec<u8>}
impl VideoFilterDraft {
    pub fn mask_busy(&self)->bool {self.rows.iter().any(|r|r.mask_loading)}
    pub fn mask_key(&self)->Option<MaskKey>{
        let row=self.rows.iter().find(|r|Some(r.id)==self.selected)?;
        if !matches!(row.filter.effect,VideoEffect::ImageMask {..}){return None;}
        Some(MaskKey {token:self.token,session:self.session,revision:self.revision,target:self.target,row:row.id,path:row.mask_path.clone(),name:row.name.clone(),signature:serde_json::to_vec(&row.filter).ok()?})
    }
    pub fn mask_path(&mut self,key:&MaskKey,path:String)->bool {
        if self.mask_key().as_ref()!=Some(key)||self.mask_busy(){return false;}
        if let Some(row)=self.rows.iter_mut().find(|r|r.id==key.row){row.mask_path=path;row.mask_error=None;true}else{false}
    }
    pub fn mask_begin(&mut self,key:&MaskKey)->bool {
        if key.token==0||self.mask_busy()||self.mask_key().as_ref()!=Some(key){return false;}
        if let Some(row)=self.rows.iter_mut().find(|r|r.id==key.row){row.mask_loading=true;row.mask_error=None;true}else{false}
    }
    pub fn mask_finish(&mut self,key:&MaskKey,result:Result<skylive_core::ImageAsset,String>){
        // Selection, name and effect parameters do not change which file is
        // being read. The prepared asset can be attached to the current filter
        // even if those unrelated draft fields changed during the read.
        let current=self.token==key.token&&self.session==key.session&&self.revision==key.revision&&self.target==key.target
            &&self.rows.iter().find(|row|row.id==key.row).is_some_and(|row|
                matches!(row.filter.effect,VideoEffect::ImageMask {..})&&row.mask_path==key.path);
        if self.token!=key.token{return;}
        if let Some(row)=self.rows.iter_mut().find(|r|r.id==key.row){
            row.mask_loading=false;
            if !current {row.mask_error=Some("Mask image target changed; load the current draft again.".into());return;}
            match result {Ok(asset)=>{let path=asset.path().to_owned();match row.filter.clone().with_mask_image(Some(asset)){Ok(filter)=>{row.filter=filter;row.mask_path=path;row.mask_error=None;},Err(error)=>row.mask_error=Some(error.to_string())}},Err(error)=>row.mask_error=Some(error)}
        }
    }
}

/// Exact linked-LUT request identity; only small validated metadata returns to UI.
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LutKey {pub token:u64,pub session:u64,pub revision:u64,pub target:VideoFilterTarget,pub row:u32,pub path:String,name:String,signature:Vec<u8>}
impl VideoFilterDraft {
    pub fn lut_busy(&self)->bool{self.rows.iter().any(|row|row.lut_loading)}
    pub fn lut_key(&self)->Option<LutKey>{let row=self.rows.iter().find(|row|Some(row.id)==self.selected)?;if !matches!(row.filter.effect,VideoEffect::ApplyLut{..}){return None;}Some(LutKey {token:self.token,session:self.session,revision:self.revision,target:self.target,row:row.id,path:row.lut_path.clone(),name:row.name.clone(),signature:serde_json::to_vec(&row.filter).ok()?})}
    pub fn lut_path(&mut self,key:&LutKey,path:String)->bool{if self.lut_busy()||self.lut_key().as_ref()!=Some(key){return false;}if let Some(row)=self.rows.iter_mut().find(|row|row.id==key.row){row.lut_path=path;row.lut_error=None;true}else{false}}
    pub fn lut_begin(&mut self,key:&LutKey)->bool{if key.token==0||self.lut_busy()||self.lut_key().as_ref()!=Some(key){return false;}if let Some(row)=self.rows.iter_mut().find(|row|row.id==key.row){row.lut_loading=true;row.lut_error=None;true}else{false}}
    pub fn lut_finish(&mut self,key:&LutKey,result:Result<skylive_core::LutAsset,String>){
        // Selection and effect parameters can change while the LUT loads.
        // Apply the result to the original filter only if its file and draft
        // identity still match; retain the current name and effect settings.
        let current=self.token==key.token&&self.session==key.session&&self.revision==key.revision&&self.target==key.target
            &&self.rows.iter().find(|row|row.id==key.row).is_some_and(|row|
                matches!(row.filter.effect,VideoEffect::ApplyLut{..})&&row.lut_path==key.path);
        if self.token!=key.token{return;}
        if let Some(row)=self.rows.iter_mut().find(|row|row.id==key.row){row.lut_loading=false;if !current{row.lut_error=Some("LUT target changed; load the current draft again.".into());return;}
            match result{Ok(asset)=>{let path=asset.path().to_owned();match row.filter.clone().with_lut_asset(Some(asset)){Ok(filter)=>{row.filter=filter;row.lut_path=path;row.lut_error=None;},Err(error)=>row.lut_error=Some(error.to_string())}},Err(error)=>row.lut_error=Some(error)}
        }
    }
}
