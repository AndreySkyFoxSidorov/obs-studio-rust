//! Bounded text-source drafts. Glyph layout/rasterization belongs to the host;
//! Apply creates one validated core command and Cancel publishes nothing.
use eframe::egui;
use skylive_core::{
    Command, Id, MAX_TEXT_CHARS, TextAlignment, TextSource, TextStyle, TextVerticalAlignment,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextEdit {
    Add { scene: Id },
    Rebind { source: Id },
}
#[derive(Clone,PartialEq,Eq)]
pub struct FontKey{pub token:u64,pub session:u64,pub revision:u64,pub path:String,pub edit:TextEdit,pub name:String,pub settings:TextSource,pub selected:Option<skylive_core::FontAsset>,pub system:bool}
pub struct TextDraft {
    pub token:u64,pub session:u64,pub revision:u64,
    pub font_path:String,pub font:Option<skylive_core::FontAsset>,pub faces:Vec<skylive_core::FontAsset>,
    pub font_loading:bool,pub font_error:Option<String>,
    pub system_font:bool,pub system_faces:Vec<skylive_core::FontAsset>,pub system_search:String,pub system_summary:Option<(usize,u32,bool)>,
    pub edit: TextEdit,
    pub name: String,
    pub text: String,
    pub read_file: bool,
    pub file_path: String,
    pub chatlog: bool,
    pub chatlog_lines: u16,
    pub width: u32,
    pub height: u32,
    pub auto_extents:bool,
    // Imported OBS GDI+ uses authored layout dimensions and an even texture.
    obs_layout: bool,
    pub font_size: u32,
    pub case_transform:skylive_core::TextCaseTransform,
    pub outline:skylive_core::TextOutline,
    pub gradient:skylive_core::TextGradient,
    pub antialiasing:bool,
    pub underline:bool,
    pub strikeout:bool,
    pub style: TextStyle,
}
impl TextDraft {
    pub fn add(scene: Id, name: String) -> Self {
        Self::from_settings(TextEdit::Add { scene }, name, &TextSource::default())
    }
    pub fn rebind(source: Id, settings: &TextSource) -> Self {
        Self::from_settings(TextEdit::Rebind { source }, String::new(), settings)
    }
    fn from_settings(edit: TextEdit, name: String, settings: &TextSource) -> Self {
        Self {
            token:0,session:0,revision:0,
            font_path:settings.font().map_or(String::new(),|font|font.path().to_owned()),font:settings.font().cloned(),
            faces:settings.font().cloned().into_iter().collect(),font_loading:false,font_error:None,
            system_font:settings.system_font(),system_faces:Vec::new(),system_search:String::new(),system_summary:None,
            edit,
            name,
            text: settings.text().into(),
            read_file: settings.file().is_some(),
            file_path: settings.file_settings().map_or(String::new(), |file|file.path().to_owned()),
            chatlog: settings.file_settings().and_then(|file|file.last_lines()).is_some(),
            chatlog_lines: settings.file_settings().map_or(6,|file|file.saved_lines()),
            width: settings.layout_width(),
            height: settings.layout_height(),
            auto_extents:settings.auto_extents(),
            obs_layout: settings.obs_layout().is_some(),
            font_size: settings.font_size(),
            case_transform:settings.case_transform(),
            outline:settings.outline(),
            gradient:settings.gradient(),
            antialiasing:settings.antialiasing(),
            underline:settings.underline(),
            strikeout:settings.strikeout(),
            style: TextStyle {
                foreground: settings.foreground(),
                background: settings.background(),
                bold: settings.bold(),
                italic: settings.italic(),
                wrap: settings.wrap(),
                alignment: settings.alignment(),
                vertical_alignment: settings.vertical_alignment(),
            },
        }
    }
    pub fn title(&self) -> &'static str {
        if matches!(self.edit, TextEdit::Add { .. }) {
            "Add Text Source"
        } else {
            "Properties: Text"
        }
    }
    fn base_settings(&self)->Result<TextSource,String>{
        TextSource::new(
            self.text.clone(),
            if self.obs_layout {self.width.saturating_add(self.width%2)} else {self.width},
            if self.obs_layout {self.height.saturating_add(self.height%2)} else {self.height},
            self.font_size,
            self.style.clone(),
        )
        .and_then(|settings|if self.obs_layout{settings.with_obs_layout(self.width,self.height)}else{Ok(settings)})
        .and_then(|settings|settings.with_auto_extents(self.auto_extents))
        .and_then(|settings|settings.with_case_transform(self.case_transform))
        .and_then(|settings|settings.with_outline(self.outline))
        .and_then(|settings|settings.with_gradient(self.gradient))
        .map(|settings|settings.with_antialiasing(self.antialiasing).with_decoration(self.underline,self.strikeout))
        .and_then(|settings|{
            let keep=self.read_file||!self.file_path.is_empty()||self.chatlog||self.chatlog_lines!=6;
            let file=if keep {Some(skylive_core::TextFile::settings(self.file_path.clone(),self.chatlog,self.chatlog_lines)?)}else{None};
            settings.with_file_settings(file,self.read_file)
        })
        .map_err(|error| error.to_string())
    }
    pub fn font_key(&self)->Result<FontKey,String>{Ok(FontKey{token:self.token,session:self.session,revision:self.revision,path:self.font_path.clone(),edit:self.edit,name:self.name.clone(),settings:self.base_settings()?,selected:self.font.clone(),system:self.system_font})}
    pub fn font_finish(&mut self,key:&FontKey,result:Result<skylive_host::font_assets::FontCatalog,String>){
        if self.token!=key.token{return;}
        self.font_loading=false;
        if self.font_key().as_ref()!=Ok(key){self.font_error=Some("The text or font draft changed; prepare the current file again.".into());return;}
        match result{
            Ok(catalog)=>{
                let old=self.font.as_ref().map(|font|font.face().index());
                let selected=if let Some(id)=old{catalog.faces.iter().find(|font|font.face().index()==id)}else{catalog.faces.first()}.cloned();
                if let Some(selected)=selected{self.font_path=selected.path().to_owned();self.font=Some(selected);self.system_font=false;self.faces=catalog.faces;self.font_error=None;}
                else{self.faces=catalog.faces;self.font_error=Some("The previously selected font face is unavailable. Choose an actual prepared face.".into());}
            },
            Err(error)=>self.font_error=Some(error),
        }
    }
    pub fn system_finish(&mut self,key:&FontKey,result:Result<skylive_host::system_fonts::SystemFontCatalog,String>){
        if self.token!=key.token{return;}
        self.font_loading=false;
        if self.font_key().as_ref()!=Ok(key){self.font_error=Some("The text or font draft changed; discover installed fonts again.".into());return;}
        match result{Ok(catalog)=>{
            self.system_summary=Some((catalog.faces.len(),catalog.skipped,catalog.complete));
            self.system_faces=catalog.faces;self.font_error=None;
        },Err(error)=>self.font_error=Some(error)}
    }
    pub fn command(&self) -> Result<Command, String> {
        if self.font_loading{return Err("Wait for font preparation to finish.".into());}
        if !self.font_path.is_empty()&&self.font.as_ref().is_none_or(|font|font.path()!=self.font_path||!self.faces.contains(font))||self.font_path.is_empty()&&self.font.is_some(){return Err("Load / Refresh the current font file before applying.".into());}
        let base=self.base_settings()?;
        let settings=if self.system_font {base.with_system_font(self.font.clone().ok_or("Choose a prepared installed font face.")?)}else{base.with_font(self.font.clone())}.map_err(|e|e.to_string())?;
        Ok(match self.edit {
            TextEdit::Add { scene } => {
                if self.name.trim().is_empty()
                    || self.name.len() > 256
                    || self.name.chars().any(char::is_control)
                {
                    return Err(
                        "Source name requires 1..256 UTF-8 bytes without control characters."
                            .into(),
                    );
                }
                Command::AddTextSource {
                    scene,
                    name: self.name.clone(),
                    settings,
                }
            }
            TextEdit::Rebind { source } => Command::SetTextSource { source, settings },
        })
    }
}

pub fn editor(ui: &mut egui::Ui, draft: &mut TextDraft, intents:&mut Vec<super::Intent>) {
    // The enclosing modal owns a viewport-sized scroll area and reserves its
    // footer. Avoid a second fixed-height scroll area inside that body.
    ui.vertical(|ui|{
        if matches!(draft.edit,TextEdit::Add{..}) {
            let label=ui.label(crate::i18n::tr(ui,"Name"));
            ui.add(egui::TextEdit::singleline(&mut draft.name).char_limit(256).id_salt("text-source-name")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Name this text source so you can find it in Sources.")).on_disabled_hover_text(crate::i18n::tr(ui,"Name this text source so you can find it in Sources."));
        }
        ui.checkbox(&mut draft.read_file,crate::i18n::tr(ui,"Read from file"))
            .on_hover_text(crate::i18n::tr(ui,"Display a UTF-8 file and refresh it once per second. The inline text is retained."));
        if draft.read_file {
            let label=ui.label(crate::i18n::tr(ui,"Text file"));
            ui.add(egui::TextEdit::singleline(&mut draft.file_path).char_limit(4096).desired_width(ui.available_width())).labelled_by(label.id)
                .on_hover_text(crate::i18n::tr(ui,"Select a UTF-8 file. Full text is limited to 1 MiB; chat-log mode reads up to 1 MiB from the end."));
            if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::TextFile,true){intents.push(intent);}
        } else {
            let label=ui.label(crate::i18n::tr(ui,"Text"));
            ui.add(egui::TextEdit::multiline(&mut draft.text).id_salt("text-source-content").char_limit(MAX_TEXT_CHARS).desired_rows(5).desired_width(440.0)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Enter up to 4096 characters of text. Line breaks are preserved."));
        }
        ui.horizontal(|ui|{
            ui.checkbox(&mut draft.chatlog,crate::i18n::tr(ui,"Chat log mode"))
                .on_hover_text(crate::i18n::tr(ui,"Show only the last selected number of lines."));
            ui.add_enabled(draft.chatlog,egui::DragValue::new(&mut draft.chatlog_lines).range(1..=u16::MAX))
                .on_hover_text(crate::i18n::tr(ui,"Number of recent lines to display."));
        });
        let label=ui.label(crate::i18n::tr(ui,"Text Transform"));
        egui::ComboBox::from_id_salt("text-source-case-transform")
            .selected_text(crate::i18n::tr(ui,draft.case_transform.label()))
            .show_ui(ui,|ui|{for value in skylive_core::TextCaseTransform::ALL{ui.selectable_value(&mut draft.case_transform,value,crate::i18n::tr(ui,value.label()));}})
            .response.labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(ui,"Change the displayed letter case without changing the original text. Start Case follows whitespace; punctuation does not start a new word."))
            .on_disabled_hover_text(crate::i18n::tr(ui,"Change the displayed letter case without changing the original text. Start Case follows whitespace; punctuation does not start a new word."));
        ui.horizontal_wrapped(|ui|{
            if ui.add_enabled(!draft.font_loading,egui::Button::new(crate::i18n::tr(ui,"Find Installed Fonts"))).on_hover_text(crate::i18n::tr(ui,"Discover supported installed Windows font faces on the background file worker. The selected exact face is saved; unavailable fonts never fall back automatically.")).clicked(){match draft.font_key(){Ok(key)=>intents.push(super::Intent::DiscoverTextSystemFonts(key)),Err(error)=>draft.font_error=Some(error)}}
        });
        if let Some((faces,skipped,complete))=draft.system_summary{ui.label(crate::i18n::tf(ui,if complete{"Installed fonts: {faces} faces; {skipped} entries unavailable or unsupported."}else{"Installed font list is incomplete: {faces} faces; {skipped} entries unavailable or unsupported."},&[("{faces}",faces.to_string()),("{skipped}",skipped.to_string())]));}
        if !draft.system_faces.is_empty(){
            let label=ui.label(crate::i18n::tr(ui,"Search installed families"));
            ui.add(egui::TextEdit::singleline(&mut draft.system_search).char_limit(256)).labelled_by(label.id);
            let query=draft.system_search.to_lowercase();
            let matches:Vec<usize>=draft.system_faces.iter().enumerate().filter_map(|(index,asset)|asset.face().family().to_lowercase().contains(&query).then_some(index)).collect();
            egui::ScrollArea::vertical().id_salt("system-font-faces").max_height(160.0).show_rows(ui,22.0,matches.len(),|ui,range|{
                for index in range{
                    let asset=&draft.system_faces[matches[index]];
                    let face=asset.face();let title=format!("{} · {} · {} · {:?}",face.family(),face.post_script_name(),face.weight(),face.style());
                    if ui.selectable_label(draft.system_font&&draft.font.as_ref()==Some(asset),title).clicked(){draft.font=Some(asset.clone());draft.font_path=asset.path().to_owned();draft.faces=vec![asset.clone()];draft.system_font=true;draft.font_error=None;}
                }
            });
        }
        ui.label(crate::i18n::tr(ui,"Font File"));
        let label=ui.label(crate::i18n::tr(ui,"Font path"));
        ui.add(egui::TextEdit::singleline(&mut draft.font_path).char_limit(4096).desired_width(ui.available_width())).labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(ui,"Choose a static TrueType TTF or TTC file, then Load / Refresh. The exact file and face are saved; missing fonts never silently fall back."));
        ui.horizontal_wrapped(|ui|{
            if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::TextFont,!draft.font_loading){intents.push(intent);}
            if ui.add_enabled(!draft.font_loading&&!draft.font_path.is_empty(),egui::Button::new(crate::i18n::tr(ui,"Load / Refresh Font")))
                .on_hover_text(crate::i18n::tr(ui,"Prepare at most16 static outline faces from one font file on the background I/O worker."))
                .clicked(){match draft.font_key(){Ok(key)=>intents.push(super::Intent::PrepareTextFont(key)),Err(error)=>draft.font_error=Some(error)}}
            if ui.button(crate::i18n::tr(ui,"Use Embedded Noto")).on_hover_text(crate::i18n::tr(ui,"Explicitly remove the linked font and restore the bundled Noto family; original text and style flags are retained.")).clicked(){draft.font=None;draft.system_font=false;draft.faces.clear();draft.font_path.clear();draft.font_loading=false;draft.font_error=None;}
        });
        if draft.font_loading{ui.spinner();}
        if let Some(font)=draft.font.as_ref(){
            let selected=format!("{} · {} · #{}",font.face().family(),font.face().post_script_name(),font.face().index());
            let label=ui.label(crate::i18n::tr(ui,"Font Face"));
            ui.add_enabled_ui(!draft.font_loading,|ui|{
                egui::ComboBox::from_id_salt("text-font-face").width(ui.available_width().min(420.0)).selected_text(selected).show_ui(ui,|ui|{
                    for face in &draft.faces{let title=format!("{} · {} · #{}",face.face().family(),face.face().post_script_name(),face.face().index());if ui.selectable_label(draft.font.as_ref()==Some(face),title).clicked(){draft.font=Some(face.clone());draft.font_path=face.path().to_owned();draft.font_error=None;}}
                }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Select an actual prepared face. Custom fonts use this face's real weight and style; no sibling or system font is substituted."));
            });
            ui.label(crate::i18n::tr(ui,"Selected fonts use static unhinted TrueType outlines only. CFF OpenType and variable/color/bitmap fonts are not supported."));
            ui.label(crate::i18n::tr(ui,"Custom-font shaping is bounded; heavily fragmented or very long text may require shorter captions."));
        }else{ui.label(crate::i18n::tr(ui,"Embedded Noto font. Choose an installed font explicitly to use it instead."));}
        if let Some(error)=&draft.font_error{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tf(ui,"Font preparation: {detail}",&[("{detail}",crate::i18n::tr(ui,error))]));}
        ui.checkbox(&mut draft.gradient.enabled,crate::i18n::tr(ui,"Gradient")).on_hover_text(crate::i18n::tr(ui,"Blend the text color into a second color across the source. Disabled gradient settings are retained."));
        ui.add_enabled_ui(draft.gradient.enabled,|ui|{
            ui.horizontal_wrapped(|ui|{
                let label=ui.label(crate::i18n::tr(ui,"Gradient Color"));
                ui.color_edit_button_srgb(&mut draft.gradient.color).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the second text gradient color."));
            });
            let label=ui.label(crate::i18n::tr(ui,"Gradient Opacity"));
            ui.add(egui::Slider::new(&mut draft.gradient.opacity,0..=100).suffix(" %")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set the second color opacity from 0 to 100 percent."));
            let label=ui.label(crate::i18n::tr(ui,"Gradient Direction"));
            ui.add(egui::Slider::new(&mut draft.gradient.direction,0.0..=360.0).clamping(egui::SliderClamping::Never).step_by(0.1).suffix("°")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set the gradient direction in degrees. 0 runs left to right; 90 runs top to bottom."));
        });
        ui.checkbox(&mut draft.outline.enabled,crate::i18n::tr(ui,"Outline")).on_hover_text(crate::i18n::tr(ui,"Draw a round outline behind the text. Settings are retained when it is disabled."));
        ui.add_enabled_ui(draft.outline.enabled,|ui|{
            ui.horizontal_wrapped(|ui|{
                let label=ui.label(crate::i18n::tr(ui,"Outline Size"));
                ui.add(egui::DragValue::new(&mut draft.outline.size).range(1..=20)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set outline width in source pixels, from 1 to 20."));
                let label=ui.label(crate::i18n::tr(ui,"Outline Color"));
                ui.color_edit_button_srgb(&mut draft.outline.color).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the outline color."));
            });
            let label=ui.label(crate::i18n::tr(ui,"Outline Opacity"));
            ui.add(egui::Slider::new(&mut draft.outline.opacity,0..=100).suffix(" %")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Set outline opacity from 0 to 100 percent."));
        });
        ui.checkbox(&mut draft.auto_extents,crate::i18n::tr_key_or(ui,"SkyLive.Text.AutoExtents","Automatic text size"))
            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.AutoExtentsHelp","Measure the text automatically. Fixed width, height and word wrapping are ignored. Font metrics may differ from OBS."));
        let auto_extents=draft.auto_extents;
        egui::Grid::new("text-source-size").num_columns(2).show(ui,|ui|{
            for (label,value,range) in [("Width (px)",&mut draft.width,1..=8192),("Height (px)",&mut draft.height,1..=8192),("Font size (px)",&mut draft.font_size,1..=1000)] {
                let name=ui.label(crate::i18n::tr(ui,label));
                ui.add_enabled(!auto_extents||label=="Font size (px)",egui::DragValue::new(value).range(range).clamp_existing_to_range(false)).labelled_by(name.id).on_hover_text(crate::i18n::tr(ui,match label {"Width (px)"=>"Set the text box width in source pixels. The box is limited to 8 megapixels.","Height (px)"=>"Set the text box height in source pixels. Text outside the box is clipped.",_=>"Set the font size in source pixels, from 1 to 1000."})).on_disabled_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.AutoExtentsDisabled","Automatic text size determines this value."));ui.end_row();
            }
        });
        ui.horizontal_wrapped(|ui|{
            ui.add_enabled(draft.font.is_none(),egui::Checkbox::new(&mut draft.style.bold,crate::i18n::tr(ui,"Bold"))).on_hover_text(crate::i18n::tr(ui,"Use a bold text appearance.")).on_disabled_hover_text(crate::i18n::tr(ui,"Custom font files use the selected face's actual weight and style. Choose a prepared bold or italic face, or switch to Embedded Noto."));
            ui.add_enabled(draft.font.is_none(),egui::Checkbox::new(&mut draft.style.italic,crate::i18n::tr(ui,"Italic"))).on_hover_text(crate::i18n::tr(ui,"Use an italic text appearance.")).on_disabled_hover_text(crate::i18n::tr(ui,"Custom font files use the selected face's actual weight and style. Choose a prepared bold or italic face, or switch to Embedded Noto."));
            ui.checkbox(&mut draft.underline,crate::i18n::tr_key_or(ui,"SkyLive.Text.Underline","Underline")).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.UnderlineHelp","Draw a line under each line of text."));
            ui.checkbox(&mut draft.strikeout,crate::i18n::tr_key_or(ui,"SkyLive.Text.Strikeout","Strikeout")).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.StrikeoutHelp","Draw a line through each line of text."));
            ui.add_enabled(!auto_extents,egui::Checkbox::new(&mut draft.style.wrap,crate::i18n::tr(ui,"Word Wrap"))).on_hover_text(crate::i18n::tr(ui,"Wrap text to fit the text box width.")).on_disabled_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.AutoExtentsDisabled","Automatic text size determines this value."));
            ui.checkbox(&mut draft.antialiasing,crate::i18n::tr(ui,"Enable Antialiasing"))
                .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.TextAntialiasingHelp","Smooth the edges of text glyphs. Turn off for hard pixel edges."))
                .on_disabled_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Text.TextAntialiasingHelp","Smooth the edges of text glyphs. Turn off for hard pixel edges."));
        });
        ui.label(crate::i18n::tr(ui,"Horizontal Alignment"));
        ui.horizontal_wrapped(|ui|{for (value,label) in [(TextAlignment::Left,"Left"),(TextAlignment::Center,"Center"),(TextAlignment::Right,"Right")] {
            ui.selectable_value(&mut draft.style.alignment,value,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,match value{TextAlignment::Left=>"Align text to the left of its box.",TextAlignment::Center=>"Center text horizontally in its box.",TextAlignment::Right=>"Align text to the right of its box."})).on_disabled_hover_text(crate::i18n::tr(ui,match value{TextAlignment::Left=>"Align text to the left of its box.",TextAlignment::Center=>"Center text horizontally in its box.",TextAlignment::Right=>"Align text to the right of its box."}));
        }});
        ui.label(crate::i18n::tr(ui,"Vertical Alignment"));
        ui.horizontal_wrapped(|ui|{for (value,label) in [(TextVerticalAlignment::Top,"Top"),(TextVerticalAlignment::Center,"Center"),(TextVerticalAlignment::Bottom,"Bottom")] {
            ui.selectable_value(&mut draft.style.vertical_alignment,value,crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,match value{TextVerticalAlignment::Top=>"Align text to the top of its box.",TextVerticalAlignment::Center=>"Center text vertically in its box.",TextVerticalAlignment::Bottom=>"Align text to the bottom of its box."})).on_disabled_hover_text(crate::i18n::tr(ui,match value{TextVerticalAlignment::Top=>"Align text to the top of its box.",TextVerticalAlignment::Center=>"Center text vertically in its box.",TextVerticalAlignment::Bottom=>"Align text to the bottom of its box."}));
        }});
        ui.horizontal_wrapped(|ui|{
            let name=crate::i18n::tr(ui,"Text Color");let label=ui.label(&name);let response=ui.color_edit_button_srgba_unmultiplied(&mut draft.style.foreground).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the text color and opacity.")).on_disabled_hover_text(crate::i18n::tr(ui,"Choose the text color and opacity."));
            let info=egui::WidgetInfo::labeled(egui::WidgetType::ColorButton,ui.is_enabled(),name);response.widget_info(||info.clone());
            let name=crate::i18n::tr(ui,"Background Color");let label=ui.label(&name);let response=ui.color_edit_button_srgba_unmultiplied(&mut draft.style.background).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the text box background color and opacity.")).on_disabled_hover_text(crate::i18n::tr(ui,"Choose the text box background color and opacity."));
            let info=egui::WidgetInfo::labeled(egui::WidgetType::ColorButton,ui.is_enabled(),name);response.widget_info(||info.clone());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_drafts_validate_only_on_apply_and_rebind_is_one_undoable_edit() {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let mut draft = TextDraft::add(scene, "Caption".into());
        assert_eq!((draft.width, draft.height, draft.font_size), (640, 160, 48));
        let original = session.snapshot();
        draft.text = "Привіт\n世界".into();
        draft.style.bold = true;
        draft.style.background = [30, 40, 50, 128];
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        session.apply(draft.command().unwrap()).unwrap();
        let source = session.snapshot().sources()[0].id();
        let model = session.snapshot();
        let skylive_core::SourceKind::Text { settings } = model.source(source).unwrap().kind()
        else {
            panic!("text")
        };
        let mut properties = TextDraft::rebind(source, settings);
        properties.style.alignment = TextAlignment::Right;
        properties.font_size = 72;
        session.apply(properties.command().unwrap()).unwrap();
        session.apply(Command::Undo).unwrap();
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),skylive_core::SourceKind::Text{settings} if settings.font_size()==48&&settings.alignment()==TextAlignment::Left)
        );
        session.apply(Command::Redo).unwrap();
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),skylive_core::SourceKind::Text{settings} if settings.font_size()==72&&settings.alignment()==TextAlignment::Right)
        );
        for invalid in ["x".repeat(MAX_TEXT_CHARS + 1), "bad\0text".into()] {
            properties.text = invalid;
            assert!(properties.command().is_err());
        }
        properties.text = "Valid".into();
        properties.width = 8192;
        properties.height = 8192;
        assert!(properties.command().is_err());
        properties.width = 640;
        properties.height = 160;
        properties.font_size = 0;
        assert!(properties.command().is_err());
    }
    fn frame(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<super::super::dialogs::Dialog>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<super::super::Intent>) {
        let mut intents = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| super::super::dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
        );
        (output, intents)
    }
    fn caption(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| caption(shape, label))
            }
            _ => None,
        }
    }
    fn click(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<super::super::dialogs::Dialog>,
        label: &str,
    ) -> Vec<super::super::Intent> {
        frame(context, session, slot, vec![]);
        let (output, _) = frame(context, session, slot, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|clipped| {
                caption(&clipped.shape, label).filter(|pos| clipped.clip_rect.contains(*pos))
            })
            .expect("dialog action remains visible");
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
    fn real_text_dialog_apply_cancel_validation_and_undo_redo_use_shared_commands() {
        use super::super::{Intent, dialogs::Dialog};
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let ctx = egui::Context::default();
        let mut draft = TextDraft::add(scene, "Caption".into());
        draft.text = "Caption in two\nlines".into();
        let mut slot = Some(Dialog::Text(draft));
        let original = session.snapshot();
        assert!(click(&ctx, &session, &mut slot, "Cancel").is_empty());
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        let mut invalid = TextDraft::add(scene, "Caption".into());
        invalid.font_size = 0;
        invalid.text = "Do not discard this".into();
        slot = Some(Dialog::Text(invalid));
        let intents = click(&ctx, &session, &mut slot, "Apply");
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, Intent::DialogError(_)))
        );
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        assert!(
            matches!(&slot,Some(Dialog::Text(draft)) if draft.font_size==0&&draft.text=="Do not discard this")
        );
        if let Some(Dialog::Text(draft)) = &mut slot {
            draft.font_size = 48;
            draft.style.background = [4, 8, 16, 128];
        }
        let intents = click(&ctx, &session, &mut slot, "Apply");
        let command = intents
            .into_iter()
            .find_map(|intent| {
                if let Intent::Command(command) = intent {
                    Some(command)
                } else {
                    None
                }
            })
            .unwrap();
        session.apply(command).unwrap();
        assert_eq!(session.snapshot().revision(), original.revision() + 1);
        let source = session.snapshot().sources()[0].id();
        session.apply(Command::Undo).unwrap();
        assert!(session.snapshot().source(source).is_none());
        session.apply(Command::Redo).unwrap();
        assert!(
            matches!(session.snapshot().source(source).unwrap().kind(),skylive_core::SourceKind::Text{settings} if settings.text()=="Do not discard this"&&settings.background()==[4,8,16,128])
        );
    }
    #[test]
    fn text_color_buttons_have_accessible_visible_label_relations() {
        let context = egui::Context::default();
        context.enable_accesskit();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let session = skylive_core::Session::new();
        let mut draft = TextDraft::add(session.preview(), "Caption".into());
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 1000.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| editor(ui, &mut draft));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        for name in ["Text Color", "Background Color"] {
            let label = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Label && node.value() == Some(name)
                })
                .map(|(id, _)| *id)
                .expect("the color control has a visible accessible label");
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == egui::accesskit::Role::ColorWell
                        && node.label() == Some(name)
                        && node.labelled_by().contains(&label)),
                "{name} color button must reference its visible label"
            );
        }
    }
}
