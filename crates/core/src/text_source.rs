//! Authored text-box settings, independent of glyph layout, fonts and GUI.
use crate::Error;
use serde::{Deserialize, Serialize};
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_TEXT_CHARS: usize = 4096;
pub const MAX_TEXT_PIXELS: u64 = 8 * 1024 * 1024;
fn is_false(value:&bool)->bool{!*value}
/// Authored file reference; content is obtained by the host and is not saved
/// as project state. None for `last_lines` displays the whole file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "TextFileData")]
pub struct TextFile { path: String, last_lines: Option<u16>, #[serde(skip_serializing_if="Option::is_none")] saved_lines: Option<u16> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextFileData { path: String, last_lines: Option<u16>, #[serde(default)] saved_lines: Option<u16> }
impl TryFrom<TextFileData> for TextFile {
    type Error = Error;
    fn try_from(value: TextFileData) -> Result<Self, Error> {
        if value.last_lines==Some(0)||value.saved_lines==Some(0)||value.last_lines.is_some()&&value.saved_lines.is_some(){return Err(Error::Invalid("Invalid retained chat-log settings."));}
        Self::settings(value.path, value.last_lines.is_some(), value.last_lines.or(value.saved_lines).unwrap_or(6))
    }
}
impl TextFile {
    pub fn new(path: String, last_lines: Option<u16>) -> Result<Self, Error> {
        if path.is_empty() {return Err(Error::Invalid("Active text file requires a path."));}
        Self::settings(path,last_lines.is_some(),last_lines.unwrap_or(6))
    }
    /// Retains the chat-log count while that option or the entire file is inactive.
    pub fn settings(path:String, chatlog:bool, lines:u16)->Result<Self,Error>{
        let bytes = path.as_bytes();
        let absolute = path.starts_with('/') || path.starts_with("\\\\")
            || (bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'));
        if (!path.is_empty()&&!absolute) || path.len() > 4096 || path.chars().any(char::is_control) {
            return Err(Error::Invalid("Text file requires an absolute path of at most 4096 UTF-8 bytes without controls."));
        }
        if lines==0 { return Err(Error::Invalid("Chat log line count must be at least one.")); }
        Ok(Self { path, last_lines:chatlog.then_some(lines), saved_lines:(!chatlog&&lines!=6).then_some(lines) })
    }
    pub fn path(&self) -> &str { &self.path }
    pub fn last_lines(&self) -> Option<u16> { self.last_lines }
    pub fn saved_lines(&self) -> u16 { self.last_lines.or(self.saved_lines).unwrap_or(6) }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextVerticalAlignment {
    #[default]
    Top,
    Center,
    Bottom,
}
/// Gradient settings retain the second endpoint while the gradient is off.
#[derive(Clone,Copy,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextGradient{pub enabled:bool,pub color:[u8;3],pub opacity:u8,pub direction:f32}
impl Default for TextGradient{fn default()->Self{Self{enabled:false,color:[255;3],opacity:100,direction:90.0}}}
impl PartialEq for TextGradient{fn eq(&self,other:&Self)->bool{self.enabled==other.enabled&&self.color==other.color&&self.opacity==other.opacity&&self.direction.to_bits()==other.direction.to_bits()}}
impl Eq for TextGradient{}
impl std::hash::Hash for TextGradient{fn hash<H:std::hash::Hasher>(&self,state:&mut H){std::hash::Hash::hash(&self.enabled,state);std::hash::Hash::hash(&self.color,state);std::hash::Hash::hash(&self.opacity,state);std::hash::Hash::hash(&self.direction.to_bits(),state);}}
impl TextGradient{pub fn validate(self)->Result<(),Error>{if self.opacity>100||!self.direction.is_finite()||!(0.0..=360.0).contains(&self.direction){return Err(Error::Invalid("Text gradient direction must be 0..360 degrees and opacity 0..100 percent."));}Ok(())}}
/// OBS-compatible outline controls, including values retained while disabled.
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextOutline{pub enabled:bool,pub size:u8,pub color:[u8;3],pub opacity:u8}
impl Default for TextOutline{fn default()->Self{Self{enabled:false,size:2,color:[255;3],opacity:100}}}
impl TextOutline{pub fn validate(self)->Result<(),Error>{if !(1..=20).contains(&self.size)||self.opacity>100{return Err(Error::Invalid("Text outline size must be 1..20 and opacity 0..100 percent."));}Ok(())}}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextStyle {
    pub foreground: [u8; 4],
    pub background: [u8; 4],
    pub bold: bool,
    pub italic: bool,
    pub wrap: bool,
    pub alignment: TextAlignment,
    pub vertical_alignment: TextVerticalAlignment,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            foreground: [255; 4],
            background: [0; 4],
            bold: false,
            italic: false,
            wrap: true,
            alignment: TextAlignment::Left,
            vertical_alignment: TextVerticalAlignment::Top,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "TextSourceData")]
pub struct TextSource {
    /// Auto geometry is resolved by a worker and is not an authored resize.
    #[serde(default,skip_serializing_if="is_false")]
    auto_extents:bool,
    /// OBS GDI+ lays out inside the authored extent before rounding the texture.
    #[serde(skip_serializing_if="Option::is_none")]
    obs_layout: Option<(u32,u32)>,
    #[serde(default,skip_serializing_if="is_false")]
    underline: bool,
    #[serde(default,skip_serializing_if="is_false")]
    strikeout: bool,
    #[serde(default,skip_serializing_if="is_false")]
    no_antialiasing: bool,
    #[serde(default,skip_serializing_if="is_false")]
    file_inactive: bool,
    #[serde(skip_serializing_if="Option::is_none")]
    file: Option<TextFile>,
    #[serde(skip_serializing_if="Option::is_none")]
    font:Option<crate::FontAsset>,
    #[serde(default,skip_serializing_if="is_false")]
    system_font:bool,
    #[serde(skip_serializing_if="Option::is_none")]
    case_transform:Option<crate::TextCaseTransform>,
    #[serde(skip_serializing_if="Option::is_none")]
    outline:Option<TextOutline>,
    #[serde(skip_serializing_if="Option::is_none")]
    gradient:Option<TextGradient>,
    text: String,
    width: u32,
    height: u32,
    font_size: u32,
    foreground: [u8; 4],
    background: [u8; 4],
    bold: bool,
    italic: bool,
    wrap: bool,
    alignment: TextAlignment,
    vertical_alignment: TextVerticalAlignment,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextSourceData {
    #[serde(default)]
    auto_extents:bool,
    #[serde(default)]
    obs_layout: Option<(u32,u32)>,
    #[serde(default)]
    underline: bool,
    #[serde(default)]
    strikeout: bool,
    #[serde(default)]
    no_antialiasing: bool,
    #[serde(default)]
    file_inactive: bool,
    #[serde(default)]
    file: Option<TextFile>,
    #[serde(default)]
    font:Option<crate::FontAsset>,
    #[serde(default)]
    system_font:bool,
    #[serde(default)]
    case_transform:Option<crate::TextCaseTransform>,
    #[serde(default)]
    outline:Option<TextOutline>,
    #[serde(default)]
    gradient:Option<TextGradient>,
    text: String,
    width: u32,
    height: u32,
    font_size: u32,
    foreground: [u8; 4],
    background: [u8; 4],
    bold: bool,
    italic: bool,
    wrap: bool,
    alignment: TextAlignment,
    vertical_alignment: TextVerticalAlignment,
}
impl TryFrom<TextSourceData> for TextSource {
    type Error = Error;
    fn try_from(v: TextSourceData) -> Result<Self, Error> {
        let result = Self {
            auto_extents:v.auto_extents,
            obs_layout: v.obs_layout,
            underline: v.underline,
            strikeout: v.strikeout,
            no_antialiasing: v.no_antialiasing,
            file_inactive: v.file_inactive,
            file: v.file,
            font:v.font,
            system_font:v.system_font,
            case_transform:v.case_transform,
            outline:v.outline,
            gradient:v.gradient,
            text: v.text.into_boxed_str().into_string(),
            width: v.width,
            height: v.height,
            font_size: v.font_size,
            foreground: v.foreground,
            background: v.background,
            bold: v.bold,
            italic: v.italic,
            wrap: v.wrap,
            alignment: v.alignment,
            vertical_alignment: v.vertical_alignment,
        };
        result.validate()?;
        Ok(result)
    }
}
impl Default for TextSource {
    fn default() -> Self {
        Self {
            auto_extents:false,
            obs_layout: None,
            underline: false,
            strikeout: false,
            no_antialiasing: false,
            file_inactive: false,
            file: None,
            font:None,
            system_font:false,
            case_transform:None,
            outline:None,
            gradient:None,
            text: "Text".into(),
            width: 640,
            height: 160,
            font_size: 48,
            foreground: [255; 4],
            background: [0; 4],
            bold: false,
            italic: false,
            wrap: true,
            alignment: TextAlignment::Left,
            vertical_alignment: TextVerticalAlignment::Top,
        }
    }
}
impl TextSource {
    pub fn new(
        text: String,
        width: u32,
        height: u32,
        font_size: u32,
        style: TextStyle,
    ) -> Result<Self, Error> {
        TextSourceData {
            auto_extents:false,
            obs_layout: None,
            underline: false,
            strikeout: false,
            no_antialiasing: false,
            file_inactive: false,
            file: None,
            font:None,
            system_font:false,
            case_transform:None,
            outline:None,
            gradient:None,
            text,
            width,
            height,
            font_size,
            foreground: style.foreground,
            background: style.background,
            bold: style.bold,
            italic: style.italic,
            wrap: style.wrap,
            alignment: style.alignment,
            vertical_alignment: style.vertical_alignment,
        }
        .try_into()
    }
    pub fn gradient(&self)->TextGradient{self.gradient.unwrap_or_default()}
    pub fn auto_extents(&self)->bool{self.auto_extents}
    /// This authored mode must not be exposed until runtime geometry is
    /// available consistently to output, preview and hit testing.
    pub fn with_auto_extents(mut self,enabled:bool)->Result<Self,Error>{
        self.auto_extents=enabled;self.validate()?;Ok(self)
    }
    /// Authored GDI+ layout extent, distinct from its even-sized texture.
    pub fn obs_layout(&self)->Option<(u32,u32)>{self.obs_layout}
    pub fn layout_width(&self)->u32{self.obs_layout.map_or(self.width,|v|v.0)}
    pub fn layout_height(&self)->u32{self.obs_layout.map_or(self.height,|v|v.1)}
    pub fn with_obs_layout(mut self,width:u32,height:u32)->Result<Self,Error>{
        self.obs_layout=Some((width,height)); self.validate()?; Ok(self)
    }
    /// Worker-local projection used for automatic text rasterization. The
    /// authored source must remain unchanged; geometry belongs to its derived
    /// result and is not an edit or an undoable command.
    pub fn with_derived_auto_extent(mut self,width:u32,height:u32)->Result<Self,Error>{
        self.width=width; self.height=height; self.obs_layout=None; self.wrap=false; self.auto_extents=false;
        self.validate()?; Ok(self)
    }
    pub fn underline(&self)->bool{self.underline}
    pub fn strikeout(&self)->bool{self.strikeout}
    pub fn with_decoration(mut self,underline:bool,strikeout:bool)->Self{self.underline=underline;self.strikeout=strikeout;self}
    /// Text rasterization uses grayscale coverage unless explicitly disabled.
    pub fn antialiasing(&self)->bool{!self.no_antialiasing}
    pub fn with_antialiasing(mut self,enabled:bool)->Self{self.no_antialiasing=!enabled;self}
    pub fn has_gradient_metadata(&self)->bool{self.gradient.is_some()}
    pub fn with_gradient(mut self,gradient:TextGradient)->Result<Self,Error>{gradient.validate()?;self.gradient=(gradient!=TextGradient::default()).then_some(gradient);self.validate()?;Ok(self)}
    pub fn outline(&self)->TextOutline{self.outline.unwrap_or_default()}
    pub fn has_outline_metadata(&self)->bool{self.outline.is_some()}
    pub fn with_outline(mut self,outline:TextOutline)->Result<Self,Error>{outline.validate()?;self.outline=(outline!=TextOutline::default()).then_some(outline);self.validate()?;Ok(self)}
    pub fn font(&self)->Option<&crate::FontAsset>{self.font.as_ref()}
    pub fn system_font(&self)->bool{self.system_font}
    pub fn file(&self) -> Option<&TextFile> { if self.file_inactive {None} else {self.file.as_ref()} }
    pub fn file_settings(&self) -> Option<&TextFile> { self.file.as_ref() }
    pub fn with_file(mut self, file: Option<TextFile>) -> Result<Self, Error> { self.file = file; self.file_inactive=false; self.validate()?; Ok(self) }
    pub fn with_file_settings(mut self, file:Option<TextFile>, enabled:bool)->Result<Self,Error>{if enabled&&file.is_none(){return Err(Error::Invalid("Active text file requires settings."));}self.file_inactive=file.is_some()&&!enabled;self.file=file;self.validate()?;Ok(self)}
    /// Derived worker input; preserves all authored appearance and font fields.
    pub fn with_resolved_text(mut self, text: String) -> Result<Self, Error> { self.text = text; self.file = None; self.file_inactive=false; self.validate()?; Ok(self) }
    pub fn with_font(mut self,font:Option<crate::FontAsset>)->Result<Self,Error>{self.font=font;self.system_font=false;self.validate()?;Ok(self)}
    pub fn with_system_font(mut self,font:crate::FontAsset)->Result<Self,Error>{self.font=Some(font);self.system_font=true;self.validate()?;Ok(self)}
    pub fn validate(&self) -> Result<(), Error> {
        if let Some((w,h))=self.obs_layout {
            if w==0||h==0||w.checked_add(w%2)!=Some(self.width)||h.checked_add(h%2)!=Some(self.height){
                return Err(Error::Invalid("OBS text layout must match its even texture extent."));
            }
        }
        if self.file_inactive && self.file.is_none(){return Err(Error::Invalid("Inactive text file settings require metadata."));}
        if self.file().is_some_and(|file|file.path().is_empty()){return Err(Error::Invalid("Active text file requires a path."));}
        if self.system_font&&self.font.is_none(){return Err(Error::Invalid("System font selection requires an exact prepared font face."));}
        if let Some(font)=&self.font{font.validate()?;}
        if let Some(outline)=self.outline{outline.validate()?;}
        if let Some(gradient)=self.gradient{gradient.validate()?;}
        if self.text.len() > MAX_TEXT_BYTES
            || self.text.chars().count() > MAX_TEXT_CHARS
            || self.text.contains('\0')
        {
            return Err(Error::Limit(
                "text source exceeds 16 KiB / 4096 characters or contains NUL",
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > MAX_TEXT_PIXELS
        {
            return Err(Error::Invalid(
                "text extents exceed 1..8192 pixels or 8 Mpixels",
            ));
        }
        if !(1..=1000).contains(&self.font_size) {
            return Err(Error::Invalid("font size must be 1..1000 pixels"));
        }
        if let Some(transform)=self.case_transform{transform.measure(&self.text)?;}
        Ok(())
    }
    pub fn case_transform(&self)->crate::TextCaseTransform{self.case_transform.unwrap_or_default()}
    pub fn has_case_transform_metadata(&self)->bool{self.case_transform.is_some()}
    pub fn with_case_transform(mut self,transform:crate::TextCaseTransform)->Result<Self,Error>{
        self.case_transform=(transform!=crate::TextCaseTransform::None).then_some(transform);
        self.validate()?;Ok(self)
    }
    /// Worker-side preparation. Authored text remains unchanged; Unicode case
    /// expansion is measured before the one bounded output allocation.
    pub fn display_text(&self)->Result<std::borrow::Cow<'_,str>,Error>{
        self.validate()?;
        let mut text=self.text.as_str();
        if self.file_inactive {
            if let Some(lines)=self.file.as_ref().and_then(|file|file.last_lines()) {
                let mut remaining=usize::from(lines);let mut start=0;
                for (index,byte) in text.bytes().enumerate().rev() {
                    if byte==b'\n' && index+1<text.len() {remaining-=1;if remaining==0 {start=index+1;break;}}
                }
                // OBS GetMainString advances past a leading newline even when
                // it reaches the beginning before finding the requested count.
                if remaining>0 && text.as_bytes().first()==Some(&b'\n') {start=1;}
                text=&text[start..];
            }
        }
        let transform=self.case_transform();
        if transform==crate::TextCaseTransform::None{return Ok(std::borrow::Cow::Borrowed(text));}
        let bytes=transform.measure(text)?;
        let mut output=String::new();output.try_reserve_exact(bytes).map_err(|_|Error::Limit("Cannot allocate transformed text."))?;
        transform.visit(text,|value|{output.push(value);Ok(())})?;
        Ok(std::borrow::Cow::Owned(output))
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn font_size(&self) -> u32 {
        self.font_size
    }
    pub fn foreground(&self) -> [u8; 4] {
        self.foreground
    }
    pub fn background(&self) -> [u8; 4] {
        self.background
    }
    pub fn bold(&self) -> bool {
        self.bold
    }
    pub fn italic(&self) -> bool {
        self.italic
    }
    pub fn wrap(&self) -> bool {
        self.wrap
    }
    pub fn alignment(&self) -> TextAlignment {
        self.alignment
    }
    pub fn vertical_alignment(&self) -> TextVerticalAlignment {
        self.vertical_alignment
    }
}
impl crate::Collection {
    pub fn has_text_antialiasing_metadata(&self)->bool{self.sources().iter().any(|source|matches!(source.kind(),crate::SourceKind::Text{settings}if !settings.antialiasing()||settings.underline()||settings.strikeout()||settings.obs_layout().is_some()||settings.auto_extents()))}
    pub fn has_text_file_metadata(&self)->bool{self.sources().iter().any(|source|matches!(source.kind(),crate::SourceKind::Text{settings}if settings.file_settings().is_some()))}
    pub fn has_text_gradient_metadata(&self)->bool{self.sources().iter().any(|source|matches!(source.kind(),crate::SourceKind::Text{settings}if settings.has_gradient_metadata()))}
    pub fn has_text_outline_metadata(&self)->bool{self.sources().iter().any(|source|matches!(source.kind(),crate::SourceKind::Text{settings}if settings.has_outline_metadata()))}
    pub fn has_text_font_metadata(&self)->bool{self.sources().iter().any(|source|matches!(source.kind(),crate::SourceKind::Text{settings}if settings.font().is_some()))}
    pub fn has_text_case_metadata(&self)->bool{self.sources().iter().any(|source|matches!(source.kind(),crate::SourceKind::Text{settings}if settings.has_case_transform_metadata()))}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_extents_utf8_and_font_limits_are_independent() {
        let make = |text, w, h, size| TextSource::new(text, w, h, size, TextStyle::default());
        assert!(make("Привіт".into(), 640, 160, 48).is_ok());
        assert!(make("x".repeat(MAX_TEXT_CHARS + 1), 640, 160, 48).is_err());
        assert!(make("x\0".into(), 640, 160, 48).is_err());
        for (w, h, size) in [
            (0, 1, 48),
            (8193, 1, 48),
            (8192, 8192, 48),
            (640, 160, 0),
            (640, 160, 1001),
        ] {
            assert!(make("x".into(), w, h, size).is_err());
        }
        assert!(make("".into(), 1, 1, 1).is_ok());
    }
    #[test]
    fn shared_text_commands_resize_all_references_and_undo_exact_authored_state() {
        use crate::{Command, Session, SourceKind, Transform};
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddTextSource {
                scene,
                name: "Caption".into(),
                settings: TextSource::default(),
            })
            .unwrap();
        let original = session.snapshot();
        let source = original.sources()[0].id();
        session
            .apply(Command::AddExistingSource { scene, source })
            .unwrap();
        let before = session.snapshot();
        let settings = TextSource::new(
            "Привіт\nHello".into(),
            320,
            80,
            24,
            TextStyle {
                bold: true,
                ..TextStyle::default()
            },
        )
        .unwrap();
        session
            .apply(Command::SetTextSource {
                source,
                settings: settings.clone(),
            })
            .unwrap();
        assert_eq!(session.snapshot().scene(scene).unwrap().items().len(), 2);
        for item in session.snapshot().scene(scene).unwrap().items() {
            assert_eq!(item.transform(), &Transform::full(320, 80));
        }
        let edited = session.snapshot();
        assert_eq!(
            edited.source(source).unwrap().kind(),
            &SourceKind::Text { settings }
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().sources(), before.sources());
        assert_eq!(session.snapshot().scenes(), before.scenes());
        session.apply(Command::Redo).unwrap();
        assert_eq!(session.snapshot().sources(), edited.sources());
        assert_eq!(session.snapshot().scenes(), edited.scenes());
        let revision = session.snapshot().revision();
        let published = session.snapshot();
        assert!(
            session
                .apply(Command::SetColor {
                    source,
                    rgba: [0; 4]
                })
                .is_err()
        );
        assert_eq!(session.snapshot().revision(), revision);
        assert!(std::sync::Arc::ptr_eq(&published, &session.snapshot()));
    }
}
