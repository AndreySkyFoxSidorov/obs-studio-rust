//! Worker-owned bounded Unicode text shaping/rasterization; no system font scanning.
use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent,
    Weight, Wrap, fontdb,
};
use skylive_core::{TextAlignment, TextSource, TextVerticalAlignment};
use skylive_media::RgbaFrame;
use std::{collections::HashMap, sync::Arc};
const FONT_FILES: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/NotoSansSymbols-Regular.ttf"),
    include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/NotoSans-Bold.ttf"),
    include_bytes!("../../../assets/fonts/NotoSans-Italic.ttf"),
    include_bytes!("../../../assets/fonts/NotoSans-BoldItalic.ttf"),
    include_bytes!("../../../assets/fonts/NotoSansArabic-Regular.ttf"),
    include_bytes!("../../../assets/fonts/NotoSansHebrew-Regular.ttf"),
];
pub const MAX_TEXT_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_GLYPH_PIXELS: usize = 16 * 1024 * 1024;
pub(crate) const MAX_TEXT_FILE_BYTES:u64=1024*1024;
#[path="text_font_raster.rs"] mod font_raster;
#[path="text_outline.rs"] mod text_outline;
#[path="text_gradient.rs"] mod text_gradient;
#[path="text_shape_admission.rs"] pub(crate) mod font_shape;
#[path="text_auto_worker.rs"] pub(crate) mod auto_worker;
fn crate_font_budget_for_auto()->crate::font_assets::FontReadBudget{
    // Measure and rasterize load the same validated font independently.
    crate::font_assets::FontReadBudget::new(skylive_core::MAX_FONT_FILE_BYTES.saturating_mul(2).saturating_add(2))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextError {
    pub kind: crate::resource_issue::ResourceIssueKind,
    pub code: &'static str,
    pub detail: String,
}
/// Errors that can disappear when the file writer finishes replacing or
/// updating the text file, including a partially written UTF-8 sequence.
/// Keep the last complete frame while retrying, but still surface the error
/// to the caller even if the file remains invalid.
pub(crate) fn retryable_file_read(error: &TextError) -> bool {
    matches!(error.code,
        "text_file_open" | "text_file_metadata" | "text_file_read" |
        "text_file_changed" | "text_file_read_budget" | "text_file_encoding")
}
impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}
impl std::error::Error for TextError {}
fn unavailable(code: &'static str, detail: impl Into<String>) -> TextError {
    let mut value = error(code, detail);
    value.kind = crate::resource_issue::ResourceIssueKind::Unavailable;
    value
}
fn limit(code: &'static str, detail: impl Into<String>) -> TextError {
    let mut value = error(code, detail);
    value.kind = crate::resource_issue::ResourceIssueKind::Limit;
    value
}
fn error(code: &'static str, detail: impl Into<String>) -> TextError {
    TextError {
        kind: crate::resource_issue::ResourceIssueKind::Invalid,
        code,
        detail: detail.into(),
    }
}
#[derive(Default)]
pub(crate) struct TextCache {
    pub bytes: usize,
    entries: HashMap<TextSource, Result<Arc<RgbaFrame>, TextError>>,
    files: HashMap<TextSource, FileEntry>,
}
struct FileEntry { checked: std::time::Instant, text: Option<String>, value: Result<Arc<RgbaFrame>, TextError>, transient: Option<TextError> }
impl TextCache {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.files.clear();
        self.bytes = 0;
    }
    /// Last complete raster for the same authored file and text settings.
    /// Used only while a transient read is retried; callers retain diagnostics.
    pub(crate) fn previous_file_frame(&self, settings: &TextSource) -> Option<Arc<RgbaFrame>> {
        self.files.get(settings)?.value.as_ref().ok().cloned()
    }
    pub fn load(
        &mut self,
        settings: &TextSource,
        remaining: usize,
    ) -> Result<Arc<RgbaFrame>, TextError> {
        self.load_with_fonts(settings,remaining,&mut crate::font_assets::FontReadBudget::new(skylive_core::MAX_FONT_FILE_BYTES+1),&mut (16*1024*1024))
    }
    pub(crate) fn load_with_fonts(&mut self,settings:&TextSource,remaining:usize,fonts:&mut crate::font_assets::FontReadBudget,text_reads:&mut usize)->Result<Arc<RgbaFrame>,TextError>{
        if settings.file().is_some() { return self.load_file(settings, remaining, fonts, text_reads); }
        if let Some(value) = self.entries.get(settings) {
            return value.clone();
        }
        let size = settings.width() as usize * settings.height() as usize * 4;
        let value = if self.entries.len().saturating_add(self.files.len()) >= skylive_core::MAX_SOURCES
            || self.bytes.saturating_add(size) > MAX_TEXT_CACHE_BYTES
            || self.bytes.saturating_add(size) > remaining
        {
            Err(limit(
                "text_raster_budget",
                "text raster cache exceeds admitted render bytes",
            ))
        } else {
            render_with_fonts(settings,fonts,remaining.saturating_sub(self.bytes)).map(Arc::new)
        };
        if let Ok(image) = &value {
            self.bytes += image.byte_len();
        }
        // At most MAX_SOURCES input-derived keys, including cached errors.
        if self.entries.len().saturating_add(self.files.len()) < skylive_core::MAX_SOURCES {
            self.entries.insert(settings.clone(), value.clone());
        }
        value
    }
    fn load_file(&mut self, settings:&TextSource, remaining:usize, fonts:&mut crate::font_assets::FontReadBudget,text_reads:&mut usize)->Result<Arc<RgbaFrame>,TextError>{
        let now=std::time::Instant::now();
        if let Some(entry)=self.files.get(settings) {
            if now.saturating_duration_since(entry.checked)<std::time::Duration::from_secs(1) {
                return match &entry.transient { Some(error) => Err(error.clone()), None => entry.value.clone() };
            }
        }
        if !self.files.contains_key(settings) && self.files.len().saturating_add(self.entries.len())>=skylive_core::MAX_SOURCES {
            return Err(limit("text_file_count", "Too many cached text sources."));
        }
        let content=read_file_text(settings,text_reads);
        // Writers often replace files atomically or hold a Windows sharing
        // lock briefly. Treat these read failures as transient as well.
        // Keep the last complete cached raster and retry on the next render
        // instead of replacing it with an error cached for a full second.
        // The current render still reports the failure; we never present an
        // incomplete read as a successful, current file snapshot.
        if let Err(error)=&content {
            if retryable_file_read(error) {
                if let Some(entry)=self.files.get_mut(settings) {
                    entry.checked=now;
                    entry.transient=Some(error.clone());
                } else {
                    self.files.insert(settings.clone(),FileEntry { checked:now, text:None, value:Err(error.clone()), transient:Some(error.clone()) });
                }
                return Err(error.clone());
            }
        }
        if let (Ok(text),Some(entry))=(&content,self.files.get_mut(settings)) {
            if entry.text.as_deref()==Some(text.as_str()) && entry.value.is_ok() { entry.checked=now; entry.transient=None; return entry.value.clone(); }
        }
        let old=self.files.remove(settings);
        if let Some(Ok(image))=old.as_ref().map(|entry|&entry.value) { self.bytes=self.bytes.saturating_sub(image.byte_len()); }
        drop(old);
        let size=settings.width() as usize*settings.height() as usize*4;
        let text=content.as_ref().ok().cloned();
        let result=if self.bytes.saturating_add(size)>MAX_TEXT_CACHE_BYTES || self.bytes.saturating_add(size)>remaining {
            Err(limit("text_raster_budget", "text raster cache exceeds admitted render bytes"))
        } else {
            content.and_then(|text|settings.clone().with_resolved_text(text).map_err(|e|error("text_file_content",e.to_string())))
                .and_then(|resolved|render_with_fonts(&resolved,fonts,remaining.saturating_sub(self.bytes))).map(Arc::new)
        };
        if let Ok(image)=&result { self.bytes=self.bytes.saturating_add(image.byte_len()); }
        self.files.insert(settings.clone(),FileEntry{checked:now,text,value:result.clone(),transient:None});
        result
    }
}
fn read_file_text(settings:&TextSource,budget:&mut usize)->Result<String,TextError>{
    use std::io::{Read,Seek,SeekFrom};
    let file=settings.file().ok_or_else(||error("text_file", "Missing text file settings."))?;
    let mut input=std::fs::File::open(file.path()).map_err(|e|unavailable("text_file_open",e.to_string()))?;
    let meta=input.metadata().map_err(|e|unavailable("text_file_metadata",e.to_string()))?;
    if !meta.is_file() {return Err(error("text_file_type","Text path is not a regular file."));}
    if meta.len()>MAX_TEXT_FILE_BYTES && file.last_lines().is_none() {return Err(limit("text_file_size","Text file exceeds 1 MiB."));}
    // Reserve the entire bounded read before touching file content, including
    // a growth sentinel. Other text files share this render invocation budget.
    // Read the marker separately so a bounded chat-log tail retains its
    // encoding even when the marker is outside the selected window.
    let mut marker=[0u8;2];
    if *budget<2{return Err(limit("text_file_read_budget","Text file reads exceed the render budget."));}
    *budget-=2;
    let marker_len=meta.len().min(2) as usize;
    input.read_exact(&mut marker[..marker_len]).map_err(|e|unavailable("text_file_read",e.to_string()))?;
    // A byte-order marker determines UTF-16 byte order, including when a
    // bounded chat-log read starts after the marker.
    let utf16_le=marker_len==2 && marker==[0xff,0xfe];
    let utf16_be=marker_len==2 && marker==[0xfe,0xff];
    let utf16=utf16_le||utf16_be;
    if utf16 && meta.len()%2!=0{return Err(error("text_file_encoding","UTF-16 text file has an incomplete code unit."));}
    let window=meta.len().min(MAX_TEXT_FILE_BYTES);
    // UTF-16 input and the 1 MiB window are both even-sized, so the
    // tail starts at a code-unit boundary. Selected lines start after LF;
    // a truncated first line is rejected before UTF-16 decoding.
    let offset=meta.len()-window;
    let allowance=(window as usize)+1;
    if *budget<allowance{return Err(limit("text_file_read_budget","Text file reads exceed the render budget."));}
    *budget-=allowance;
    let mut bytes=Vec::new();
    bytes.try_reserve_exact(allowance).map_err(|_|limit("text_file_size","Cannot allocate text file buffer."))?;
    input.seek(SeekFrom::Start(offset)).map_err(|e|unavailable("text_file_read",e.to_string()))?;
    input.by_ref().take(allowance as u64).read_to_end(&mut bytes).map_err(|e|unavailable("text_file_read",e.to_string()))?;
    if bytes.len()>window as usize {return Err(unavailable("text_file_changed","Text file grew during the read; it will be retried."));}
    let after=input.metadata().map_err(|e|unavailable("text_file_metadata",e.to_string()))?;
    if bytes.len()!=window as usize || after.len()!=meta.len() || after.modified().ok()!=meta.modified().ok() {
        return Err(unavailable("text_file_changed","Text file changed during the read; it will be retried."));
    }
    if utf16 {
        let bom=if offset==0 {2} else {0};
        let units:Vec<u16>=bytes[bom..].chunks_exact(2).map(|pair|if utf16_be {u16::from_be_bytes([pair[0],pair[1]])} else {u16::from_le_bytes([pair[0],pair[1]])}).collect();
        let mut start=0;
        if let Some(lines)=file.last_lines() {
            let mut remaining=usize::from(lines);
            for index in (0..units.len()).rev() {
                if units[index]==u16::from(b'\n') && index+1<units.len() {
                    remaining-=1;
                    if remaining==0 {start=index+1;break;}
                }
            }
            if remaining>0 && offset!=0 {return Err(limit("text_file_tail","The requested chat-log lines exceed the 1 MiB read window."));}
            if remaining>0 && units.first()==Some(&u16::from(b'\n')) {start=1;}
        }
        let text=String::from_utf16(&units[start..]).map_err(|_|error("text_file_encoding","Text file contains invalid UTF-16."))?;
        if text.len()>skylive_core::MAX_TEXT_BYTES || text.chars().count()>skylive_core::MAX_TEXT_CHARS {return Err(limit("text_file_content","Selected text exceeds the text source limit."));}
        return Ok(text);
    }
    // OBS removes a UTF-8 BOM before selecting the chat-log suffix.
    let bom=if offset==0 && bytes.starts_with(&[0xef,0xbb,0xbf]) {3}else{0};
    let start=if let Some(lines)=file.last_lines() {
        let mut remaining=usize::from(lines);let mut start=bom;
        for (relative,byte) in bytes[bom..].iter().copied().enumerate().rev() {
            let index=relative+bom;
            if byte==b'\n' && index+1<bytes.len() { remaining-=1; if remaining==0 {start=index+1;break;} }
        }
        if remaining>0 && offset!=0 {return Err(limit("text_file_tail","The requested chat-log lines exceed the 1 MiB read window."));}
        // OBS advances past a leading newline when the requested number of
        // lines reaches the beginning of the file.
        if remaining>0 && bytes.get(bom)==Some(&b'\n') {start=bom+1;}
        start
    } else {bom};
    let text=std::str::from_utf8(&bytes[start..]).map_err(|_|error("text_file_encoding","Text file must be UTF-8."))?;
    if text.len()>skylive_core::MAX_TEXT_BYTES || text.chars().count()>skylive_core::MAX_TEXT_CHARS {return Err(limit("text_file_content","Selected text exceeds the text source limit."));}
    Ok(text.to_owned())
}
/// A renderer invocation owns all font/layout/scaler caches and drops them off-frame.
/// Embedded fonts or exact prepared static file identities enter the shaper. No system scan.
pub fn render(settings: &TextSource) -> Result<RgbaFrame, TextError> {
    let resolved;
    let settings=if settings.file().is_some(){
        let text=read_file_text(settings,&mut (16*1024*1024))?;
        resolved=settings.clone().with_resolved_text(text).map_err(|e|error("text_file_content",e.to_string()))?;
        &resolved
    }else{settings};
    render_with_fonts(settings,&mut crate::font_assets::FontReadBudget::new(skylive_core::MAX_FONT_FILE_BYTES+1),crate::preview::MAX_RENDER_RESOURCE_BYTES)
}
/// Measure an unwrapped, already-resolved text source on a worker. This is a
/// building block for automatic extents, not a published scene geometry: the
/// caller must still associate the result with the source revision and make
/// it available consistently to rendering, grouping and hit testing.
/// The current shaper is not GDI+, so its metrics are approximate.
#[allow(dead_code)]
pub(crate) fn measure_auto_extents(settings:&TextSource, font_reads:&mut crate::font_assets::FontReadBudget, available:usize)->Result<(u32,u32),TextError>{
    if settings.file().is_some(){return Err(error("text_auto_unresolved_file","Automatic extents require text resolved by the file worker."));}
    settings.validate().map_err(|e|error("text_invalid",e.to_string()))?;
    let display=settings.display_text().map_err(|e|error("text_case_transform",e.to_string()))?;
    let shaping=font_shape::prepare(&display)?;
    let required=shaping.checked_add(32*1024*1024)
        .and_then(|n|n.checked_add(if settings.font().is_some(){skylive_core::MAX_FONT_FILE_BYTES as usize}else{0}))
        .filter(|n|*n<=available).ok_or_else(||limit("text_auto_admission","Automatic text measurement exceeds the admitted worker budget."))?;
    let _=required;
    let custom=settings.font().map(|asset|crate::font_assets::load(asset,font_reads)).transpose()?;
    let selected=custom.as_ref().map(|loaded|loaded.id);
    let mut db=if let Some(loaded)=custom {loaded.db}else{
        let mut db=fontdb::Database::new();for bytes in FONT_FILES {db.load_font_data(bytes.to_vec());}db
    };
    db.set_sans_serif_family(if selected.is_some(){font_shape::SELECTED_FAMILY}else{"Noto Sans"});
    let mut fonts=if selected.is_some(){FontSystem::new_with_locale_and_db_and_fallback("en-US".into(),db,font_shape::SelectedOnly)}else{FontSystem::new_with_locale_and_db("en-US".into(),db)};
    let mut buffer=Buffer::new(&mut fonts,Metrics::new(settings.font_size() as f32,settings.font_size() as f32*1.25));
    buffer.set_size(&mut fonts,None,None);
    buffer.set_wrap(&mut fonts,Wrap::None);
    let attrs=Attrs::new().family(Family::SansSerif)
        .weight(settings.font().map_or_else(||if settings.bold(){Weight::BOLD}else{Weight::NORMAL},|asset|Weight(asset.face().weight())))
        .style(settings.font().map_or_else(||if settings.italic(){Style::Italic}else{Style::Normal},|asset|match asset.face().style(){skylive_core::FontFaceStyle::Normal=>Style::Normal,skylive_core::FontFaceStyle::Italic=>Style::Italic,skylive_core::FontFaceStyle::Oblique=>Style::Oblique}));
    buffer.set_text(&mut fonts,&display,&attrs,Shaping::Advanced);
    buffer.shape_until_scroll(&mut fonts,true);
    let (width,height)=buffer.layout_runs().fold((0.0_f32,0.0_f32),|(w,h),run|(w.max(run.line_w),h.max(run.line_top+run.line_height)));
    if !width.is_finite()||!height.is_finite(){return Err(limit("text_auto_extent","Text measurement is not finite."));}
    let outline=if settings.outline().enabled{u32::from(settings.outline().size)}else{0};
    let w=(width.ceil().max(1.0) as u32).saturating_add(outline).max(2).saturating_add(1)&!1;
    let h=(height.ceil().max(1.0) as u32).saturating_add(outline).max(2).saturating_add(1)&!1;
    if w>8192||h>8192||u64::from(w)*u64::from(h)>8*1024*1024{return Err(limit("text_auto_extent","Automatic text extent exceeds the supported geometry budget."));}
    Ok((w,h))
}

/// One indivisible worker result: consumers must use this raster and its
/// dimensions together. The revision owner is responsible for advancing the
/// guard when session, authored settings, font or linked-file generation
/// changes; a result is invalid until that owner is wired to all consumers.
#[allow(dead_code)]
pub(crate) struct AutoTextFrame {
    pub(crate) session:u64,
    pub(crate) source:skylive_core::Id,
    pub(crate) authored:TextSource,
    pub(crate) revision:u64,
    pub(crate) live:Arc<std::sync::atomic::AtomicU64>,
    pub(crate) raster:Arc<RgbaFrame>,
}
#[allow(dead_code)]
impl AutoTextFrame {
    pub(crate) fn current(&self,session:u64,source:skylive_core::Id,authored:&TextSource)->bool {
        self.session==session && self.source==source && &self.authored==authored
            && self.live.load(std::sync::atomic::Ordering::Acquire)==self.revision
    }
    pub(crate) fn dimensions(&self)->(u32,u32){(self.raster.width(),self.raster.height())}
}
/// Runs only on a worker. It reads an active file once and measures/renders
/// the very same resolved text, avoiding a mismatched geometry and raster if
/// an editor changes the file between operations. No result is published here.
#[allow(dead_code)]
pub(crate) fn prepare_auto_frame(session:u64,source:skylive_core::Id,authored:&TextSource,revision:u64,
    live:Arc<std::sync::atomic::AtomicU64>,font_reads:&mut crate::font_assets::FontReadBudget,
    text_reads:&mut usize,available:usize)->Result<AutoTextFrame,TextError> {
    use std::sync::atomic::Ordering;
    if live.load(Ordering::Acquire)!=revision{return Err(unavailable("text_auto_stale","Text measurement revision is no longer current."));}
    let resolved=if authored.file().is_some(){
        let text=read_file_text(authored,text_reads)?;
        authored.clone().with_resolved_text(text).map_err(|e|error("text_file_content",e.to_string()))?
    }else{authored.clone()};
    let (width,height)=measure_auto_extents(&resolved,font_reads,available)?;
    let raster_settings=resolved.with_derived_auto_extent(width,height).map_err(|e|error("text_auto_extent",e.to_string()))?;
    let raster=Arc::new(render_with_fonts(&raster_settings,font_reads,available)?);
    if live.load(Ordering::Acquire)!=revision{return Err(unavailable("text_auto_stale","Text measurement changed before publication."));}
    Ok(AutoTextFrame {session,source,authored:authored.clone(),revision,live,raster})
}
fn render_with_fonts(settings:&TextSource,font_reads:&mut crate::font_assets::FontReadBudget,available:usize)->Result<RgbaFrame,TextError>{
    if settings.auto_extents(){return Err(unavailable("text_auto_not_published","Automatic text geometry has not been prepared for this render."));}
    settings
        .validate()
        .map_err(|e| error("text_invalid", e.to_string()))?;
    let display_text=settings.display_text().map_err(|e|{
        if matches!(&e,skylive_core::Error::Limit(_)){limit("text_case_transform",e.to_string())}
        else{error("text_case_transform",e.to_string())}
    })?;
    let outline=settings.outline();
    let outline_plan=if outline.enabled&&outline.opacity!=0&&!display_text.is_empty(){Some(text_outline::Plan::new(settings.width(),settings.height(),outline.size)?)}else{None};
    let outline_bytes=outline_plan.as_ref().map_or(Ok(0),|plan|plan.bytes())?;
    let raster=settings.width() as usize*settings.height() as usize*4;
    if raster.checked_add(outline_bytes).is_none_or(|bytes|bytes>available){return Err(limit("text_outline_limit","Text output and outline masks exceed the admitted render budget."));}
    if settings.font().is_some(){
        let shaping=font_shape::prepare(&display_text)?;
        // Reservation includes output pixels, the entire encoded font snapshot,
        // <=320KiB graph proof and bounded scaler/outline/mask scratch. All are
        // transient on this existing render owner; no credit survives a return.
        let reserved=shaping.checked_add(raster).and_then(|v|v.checked_add(outline_bytes)).and_then(|v|v.checked_add(skylive_core::MAX_FONT_FILE_BYTES as usize+32*1024*1024))
            .filter(|v|*v<=available).ok_or_else(||limit("text_shape_admission","Custom-font transient shaping and raster bytes exceed the admitted render budget."))?;
        let _=reserved;
    }
    let custom=settings.font().map(|asset|crate::font_assets::load(asset,font_reads)).transpose()?;
    let selected=custom.as_ref().map(|loaded|loaded.id);
    let mut custom_raster=None;
    let mut db=if let Some(loaded)=custom {custom_raster=Some(font_raster::Raster::new(loaded.id,loaded.costs));loaded.db}else{
        let mut db=fontdb::Database::new();for bytes in FONT_FILES {db.load_font_data(bytes.to_vec());}db
    };
    db.set_sans_serif_family(if selected.is_some(){font_shape::SELECTED_FAMILY}else{"Noto Sans"});
    let mut fonts = if selected.is_some(){
        FontSystem::new_with_locale_and_db_and_fallback("en-US".into(),db,font_shape::SelectedOnly)
    }else{FontSystem::new_with_locale_and_db("en-US".into(),db)};
    let mut buffer = Buffer::new(
        &mut fonts,
        Metrics::new(
            settings.font_size() as f32,
            settings.font_size() as f32 * 1.25,
        ),
    );
    let inset=if outline.enabled{f32::from(outline.size)*0.5}else{0.0};
    let layout_width=if outline.enabled&&settings.wrap(){settings.layout_width().saturating_sub(u32::from(outline.size)).max(1)}else{settings.layout_width()};
    buffer.set_size(&mut fonts, Some(layout_width as f32), None);
    buffer.set_wrap(
        &mut fonts,
        if settings.wrap() {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        },
    );
    let attrs = Attrs::new()
        .family(Family::SansSerif)
        .weight(settings.font().map_or_else(||if settings.bold(){Weight::BOLD}else{Weight::NORMAL},|asset|Weight(asset.face().weight())))
        .style(settings.font().map_or_else(||if settings.italic(){Style::Italic}else{Style::Normal},|asset|match asset.face().style(){skylive_core::FontFaceStyle::Normal=>Style::Normal,skylive_core::FontFaceStyle::Italic=>Style::Italic,skylive_core::FontFaceStyle::Oblique=>Style::Oblique}));
    buffer.set_text(&mut fonts, &display_text, &attrs, Shaping::Advanced);
    let align = match settings.alignment() {
        TextAlignment::Left => cosmic_text::Align::Left,
        TextAlignment::Center => cosmic_text::Align::Center,
        TextAlignment::Right => cosmic_text::Align::Right,
    };
    for line in &mut buffer.lines {
        line.set_align(Some(align));
    }
    buffer.shape_until_scroll(&mut fonts, true);
    let total_height = buffer
        .layout_runs()
        .map(|run| run.line_top + run.line_height)
        .fold(0.0_f32, f32::max);
    let offset = match settings.vertical_alignment() {
        TextVerticalAlignment::Top => 0.0,
        TextVerticalAlignment::Center => (settings.layout_height() as f32 - total_height) * 0.5,
        TextVerticalAlignment::Bottom => settings.layout_height() as f32 - total_height,
    };
    let mut pixels = settings
        .background()
        .repeat(settings.width() as usize * settings.height() as usize);
    let mut scaler = SwashCache::new();
    let mut outline_buffers=outline_plan.as_ref().map(|plan|plan.allocate()).transpose()?;

    let mut work = 0usize;
    let [r, g, b, a] = settings.foreground();
    let gradient=settings.gradient();
    let brush=gradient.enabled.then(||text_gradient::Brush::new([r,g,b,a],gradient,settings.width(),settings.height()));
    let pad = settings.font_size() as i32 * 4 + 64;
    for pass in if outline_buffers.is_some(){0..2}else{1..2} {
    let mut glyphs=0usize;
    for run in buffer.layout_runs() {
        // GDI font decorations follow each laid-out line, including alignment
        // and wrapping. Their metrics are a conservative approximation until
        // native face underline/strike metrics are available.
        if (settings.underline() || settings.strikeout()) && !run.glyphs.is_empty() {
            let left=run.glyphs.iter().map(|g|g.physical((inset,inset),1.0).x).min().unwrap_or(0).clamp(0,settings.width() as i32);
            let right=run.glyphs.iter().map(|g|g.physical((inset,inset),1.0).x.saturating_add(g.w.ceil() as i32)).max().unwrap_or(left).clamp(0,settings.width() as i32);
            let thickness=((settings.font_size() as f32 / 16.0).round() as i32).max(1);
            let baseline=run.line_y.round() as i32+offset.round() as i32;
            for (enabled,y) in [(settings.underline(),baseline+(settings.font_size() as f32*0.08).round() as i32),
                                (settings.strikeout(),baseline-(settings.font_size() as f32*0.30).round() as i32)] {
                if !enabled {continue;}
                let top=y.max(0);let bottom=y.saturating_add(thickness).min(settings.height() as i32);
                let count=(right-left).max(0) as usize*(bottom-top).max(0) as usize;
                work=work.checked_add(count).ok_or_else(||limit("text_glyph_budget","decoration work overflow"))?;
                if work>MAX_GLYPH_PIXELS{return Err(limit("text_glyph_budget","text decoration exceeds raster work budget"));}
                for y in top..bottom {for x in left..right {
                    if pass==0 {if let (Some(plan),Some(mask))=(&outline_plan,&mut outline_buffers){plan.add(mask,x,y,255);}}
                    else {let color=brush.as_ref().map_or([r,g,b,a],|brush|brush.sample(x,y));let target=(y as usize*settings.width() as usize+x as usize)*4;blend(&mut pixels[target..target+4],color);}
                }}
            }
        }
        for glyph in run.glyphs {
            glyphs += 1;
            if glyphs > skylive_core::MAX_TEXT_CHARS * 4 {
                return Err(limit(
                    "text_glyph_budget",
                    "shaped glyph count exceeds16384",
                ));
            }
            if selected.is_some_and(|id|id!=glyph.font_id){
                return Err(error("font_face_mismatch","Text shaping selected another font face; no fallback was accepted."));
            }
            if glyph.glyph_id == 0 && (glyph.w > 0.0 || selected.is_some()) {
                return Err(unavailable(
                    "text_missing_glyph",
                    format!(
                        "the selected font profile cannot render cluster {:?}",
                        run.text.get(glyph.start..glyph.end).unwrap_or("")
                    ),
                ));
            }
            let physical = glyph.physical((inset, inset), 1.0);
            let baseline = run.line_y as i32 + physical.y + offset.round() as i32;
            if physical.x < -pad
                || physical.x > settings.width() as i32 + pad
                || baseline < -pad
                || baseline > settings.height() as i32 + pad
            {
                continue;
            }
            // No persistent Swash image/outline cache: arbitrary text edits cannot retain
            // an unbounded atlas. A single trusted-font glyph allocation is ephemeral.
            let is_custom=custom_raster.is_some();
            let image=if let Some(custom)=&mut custom_raster{custom.image(&mut fonts,physical.cache_key,&mut work)?}
                else{scaler.get_image_uncached(&mut fonts,physical.cache_key)};
            let Some(image)=image else{continue;};
            let width = image.placement.width as usize;
            let height = image.placement.height as usize;
            if !is_custom {work = work
                .checked_add(width.saturating_mul(height))
                .ok_or_else(|| limit("text_glyph_budget", "glyph work overflow"))?;}
            if work > MAX_GLYPH_PIXELS {
                return Err(limit(
                    "text_glyph_budget",
                    "glyph raster work exceeds16Mpixels",
                ));
            }
            let channels = match image.content {
                SwashContent::Mask => 1,
                SwashContent::Color => 4,
                SwashContent::SubpixelMask => 4,
            };
            if image.data.len() != width * height * channels {
                return Err(error("text_raster", "glyph bitmap shape mismatch"));
            }
            for gy in 0..height {
                let y = baseline - image.placement.top + gy as i32;
                if pass==1 && (y < 0 || y >= settings.height() as i32) {
                    continue;
                }
                for gx in 0..width {
                    let x = physical.x + image.placement.left + gx as i32;
                    let index = (gy * width + gx) * channels;
                    if pass==0{
                        let coverage=match image.content{SwashContent::Mask=>image.data[index],SwashContent::SubpixelMask=>*image.data[index..index+3].iter().max().unwrap(),SwashContent::Color=>image.data[index+3]};
                        let coverage=glyph_coverage(coverage,settings.antialiasing());
                        if let (Some(plan),Some(mask))=(&outline_plan,&mut outline_buffers){plan.add(mask,x,y,coverage);}
                        continue;
                    }
                    if x < 0 || x >= settings.width() as i32 {continue;}
                    let rgba=brush.as_ref().map_or([r,g,b,a],|brush|brush.sample(x,y));
                    let color = match image.content {
                        SwashContent::Mask => [
                            rgba[0],
                            rgba[1],
                            rgba[2],
                            ((u16::from(glyph_coverage(image.data[index],settings.antialiasing())) * u16::from(rgba[3]) + 127) / 255)
                                as u8,
                        ],
                        SwashContent::SubpixelMask => [
                            rgba[0],
                            rgba[1],
                            rgba[2],
                            ((u16::from(glyph_coverage(*image.data[index..index + 3].iter().max().unwrap(),settings.antialiasing()))
                                * u16::from(rgba[3])
                                + 127)
                                / 255) as u8,
                        ],
                        SwashContent::Color => [
                            image.data[index],
                            image.data[index + 1],
                            image.data[index + 2],
                            ((u16::from(glyph_coverage(image.data[index + 3],settings.antialiasing())) * u16::from(a) + 127) / 255) as u8,
                        ],
                    };
                    let target = (y as usize * settings.width() as usize + x as usize) * 4;
                    blend(&mut pixels[target..target + 4], color);
                }
            }
        }
    }
    if pass==0{if let (Some(plan),Some(mask))=(&outline_plan,&mut outline_buffers){plan.composite(mask,&mut pixels,outline.color,outline.opacity);}}
    }
    RgbaFrame::new(settings.width(), settings.height(), pixels)
        .map_err(|e| error("text_raster", e.to_string()))
}
fn glyph_coverage(coverage:u8,antialiasing:bool)->u8 {
    if antialiasing {coverage} else if coverage>=128 {255} else {0}
}
fn blend(pixel: &mut [u8], source: [u8; 4]) {
    let sa = f64::from(source[3]) / 255.0;
    let da = f64::from(pixel[3]) / 255.0;
    let alpha = sa + da * (1.0 - sa);
    if alpha == 0.0 {
        pixel.fill(0);
        return;
    }
    for i in 0..3 {
        pixel[i] = ((f64::from(source[i]) * sa + f64::from(pixel[i]) * da * (1.0 - sa)) / alpha)
            .round() as u8;
    }
    pixel[3] = (alpha * 255.0).round() as u8;
}
#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::TextStyle;
    fn settings(text: &str, align: TextAlignment) -> TextSource {
        TextSource::new(
            text.into(),
            320,
            180,
            36,
            TextStyle {
                foreground: [255, 0, 0, 255],
                alignment: align,
                ..TextStyle::default()
            },
        )
        .unwrap()
    }
    #[test]
    fn licensed_symbols_fallback_rasterizes_both_arrow_directions_and_budget_is_typed() {
        let left = render(&settings("←", TextAlignment::Left)).unwrap();
        let right = render(&settings("→", TextAlignment::Left)).unwrap();
        assert!(bounds(&left).is_some());
        assert!(bounds(&right).is_some());
        assert_ne!(left.pixels(), right.pixels());
        for image in [&left, &right] {
            assert!(image.pixels().chunks_exact(4).any(|pixel| pixel[3] > 0));
            assert!(
                image
                    .pixels()
                    .chunks_exact(4)
                    .filter(|pixel| pixel[3] > 0)
                    .all(|pixel| pixel[..3] == [255, 0, 0])
            );
        }
        let mut cache = TextCache::default();
        assert_eq!(
            cache
                .load(&settings("A", TextAlignment::Left), 0)
                .unwrap_err()
                .kind,
            crate::resource_issue::ResourceIssueKind::Limit
        );
    }
    fn bounds(frame: &RgbaFrame) -> Option<(usize, usize, usize, usize)> {
        let mut points = frame
            .pixels()
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, p)| p[3] > 0)
            .map(|(i, _)| (i % frame.width() as usize, i / frame.width() as usize));
        let (x, y) = points.next()?;
        Some(points.fold((x, y, x, y), |(a, b, c, d), (x, y)| {
            (a.min(x), b.min(y), c.max(x), d.max(y))
        }))
    }
    #[test]
    fn actual_unicode_shaping_transparency_color_and_horizontal_alignment() {
        for content in ["Hello", "Привіт", "مرحبا بالعالم", "שלום"] {
            let image = render(&settings(content, TextAlignment::Left)).unwrap();
            assert!(bounds(&image).is_some());
            assert_eq!(&image.pixels()[..4], &[0, 0, 0, 0]);
            assert!(
                image
                    .pixels()
                    .chunks_exact(4)
                    .any(|p| p[0] == 255 && p[1] == 0 && p[2] == 0 && p[3] > 0)
            );
        }
        let left = bounds(&render(&settings("Hello", TextAlignment::Left)).unwrap()).unwrap();
        let center = bounds(&render(&settings("Hello", TextAlignment::Center)).unwrap()).unwrap();
        let right = bounds(&render(&settings("Hello", TextAlignment::Right)).unwrap()).unwrap();
        assert!(left.0 < center.0 && center.0 < right.0);
        assert_eq!(left.2 - left.0, right.2 - right.0);
    }
    #[test]
    fn empty_background_is_authored_and_cache_budget_rejects_before_raster() {
        let value = TextSource::new(
            "".into(),
            2,
            2,
            12,
            TextStyle {
                background: [10, 20, 30, 128],
                ..TextStyle::default()
            },
        )
        .unwrap();
        assert_eq!(
            render(&value).unwrap().pixels(),
            [10, 20, 30, 128].repeat(4)
        );
        let mut cache = TextCache::default();
        assert_eq!(
            cache.load(&value, 15).unwrap_err().code,
            "text_raster_budget"
        );
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn text_native_roundtrip_and_actual_compositor_share_validated_settings() {
        use skylive_core::{Command, Session};
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetVideo {
                width: 320,
                height: 180,
                fps_num: 30,
                fps_den: 1,
            })
            .unwrap();
        session
            .apply(Command::AddTextSource {
                scene,
                name: "Caption".into(),
                settings: settings("Привіт Hello", TextAlignment::Center),
            })
            .unwrap();
        let snapshot = session.snapshot();
        let expected = render(match snapshot.sources()[0].kind() {
            skylive_core::SourceKind::Text { settings } => settings,
            _ => panic!(),
        })
        .unwrap();
        let actual = crate::preview::render(&snapshot, scene, 320, 180).unwrap();
        // CPU output composites straight RGBA over the opaque black program background.
        let i = expected
            .pixels()
            .chunks_exact(4)
            .position(|p| p[3] == 255)
            .unwrap()
            * 4;
        assert_eq!(&actual.pixels()[i..i + 4], &[255, 0, 0, 255]);
        assert_eq!(&actual.pixels()[..4], &[0, 0, 0, 255]);
        let bytes = crate::storage::encode(&snapshot).unwrap();
        let decoded = crate::storage::decode(&bytes, true).unwrap();
        assert_eq!(decoded.snapshot().sources(), snapshot.sources());
        assert_eq!(
            crate::preview::render(&decoded.snapshot(), decoded.preview(), 320, 180)
                .unwrap()
                .pixels(),
            actual.pixels()
        );
        let mut cache = TextCache::default();
        let value = TextSource::default();
        let first = cache.load(&value, MAX_TEXT_CACHE_BYTES).unwrap();
        let again = cache.load(&value, MAX_TEXT_CACHE_BYTES).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(cache.bytes, 640 * 160 * 4);
        cache.clear();
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn multiline_wrap_vertical_alignment_styles_and_missing_glyph_have_visible_outcomes() {
        let make = |text: &str, style: TextStyle| {
            TextSource::new(text.into(), 160, 180, 24, style).unwrap()
        };
        let base = TextStyle::default();
        let single = render(&make("Hello", base.clone())).unwrap();
        let multiline = render(&make("Hello\nWorld", base.clone())).unwrap();
        assert!(bounds(&multiline).unwrap().3 > bounds(&single).unwrap().3);
        let wrapped = render(&make("Hello World Hello World", base.clone())).unwrap();
        let unwrapped = render(&make(
            "Hello World Hello World",
            TextStyle {
                wrap: false,
                ..base.clone()
            },
        ))
        .unwrap();
        assert!(bounds(&wrapped).unwrap().3 > bounds(&unwrapped).unwrap().3);
        let center = render(&make(
            "Hello",
            TextStyle {
                vertical_alignment: TextVerticalAlignment::Center,
                ..base.clone()
            },
        ))
        .unwrap();
        let bottom = render(&make(
            "Hello",
            TextStyle {
                vertical_alignment: TextVerticalAlignment::Bottom,
                ..base.clone()
            },
        ))
        .unwrap();
        assert!(bounds(&single).unwrap().1 < bounds(&center).unwrap().1);
        assert!(bounds(&center).unwrap().1 < bounds(&bottom).unwrap().1);
        for style in [
            TextStyle {
                bold: true,
                ..base.clone()
            },
            TextStyle {
                italic: true,
                ..base.clone()
            },
        ] {
            assert_ne!(
                single.pixels(),
                render(&make("Hello", style)).unwrap().pixels()
            );
        }
        assert_eq!(
            render(&make("漢", base)).unwrap_err().code,
            "text_missing_glyph"
        );
    }
}
