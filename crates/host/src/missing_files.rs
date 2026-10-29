//! Explicit existence inspection and validated relink, only on a file IO owner.
use skylive_core::{ResourceSlot as Slot,ResourceValue as Value,ResourceRelink,SlideShowEntry,SlideShowEntryKind,SourceKind,StudioSnapshot};
use serde::{Serialize,Deserialize};
use std::{path::Path,sync::Arc};
pub const PAGE_SLOTS:usize=64;
pub const MAX_PAGE_METADATA:usize=2*1024*1024;
pub const MAX_PREPARE_METADATA:usize=4*1024*1024;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="snake_case")]
pub enum State{Present,Missing,Unreadable,WrongKind}
impl State{pub fn label(self)->&'static str{match self{Self::Present=>"Present (not content-checked)",Self::Missing=>"Missing",Self::Unreadable=>"Unreadable",Self::WrongKind=>"Wrong file or folder type"}}}
#[derive(Clone,Debug,Serialize)]
pub struct Row{pub ordinal:usize,pub slot:Slot,pub owner:String,pub path:String,pub directory:bool,pub state:State,pub detail:Option<String>}
#[derive(Debug,Serialize)]
pub struct Page{pub start:usize,pub next:Option<usize>,pub total:usize,pub rows:Vec<Row>}
/// Metadata only, including inactive sources/filters. No decoding/hash assertion.
pub fn scan(model:&StudioSnapshot,start:usize)->Result<Page,String>{
 let end=start.checked_add(PAGE_SLOTS).ok_or("Missing Files page cursor overflow")?;
 let mut page=Page{start,next:None,total:0,rows:Vec::new()};let mut bytes=0usize;let mut total=0usize;
 let mut visit=|ordinal:usize,slot:Slot,name:&str,path:&str,directory:bool|->Result<(),String>{
  if ordinal<start||ordinal>=end{return Ok(());}
  bytes=bytes.checked_add(std::mem::size_of::<Row>()+name.len()+path.len()+4096).filter(|n|*n<=MAX_PAGE_METADATA).ok_or("Missing Files page exceeds 2 MiB metadata; no partial page was returned")?;
  let(state,detail)=match std::fs::metadata(path){Ok(m)=>if(directory&&m.is_dir())||(!directory&&m.is_file()){(State::Present,None)}else{(State::WrongKind,None)},Err(error)=>{let state=if error.kind()==std::io::ErrorKind::NotFound{State::Missing}else{State::Unreadable};let text=error.to_string();if text.len()>4096{return Err("Missing Files diagnostic exceeds 4096 UTF-8 bytes".into());}(state,Some(text))}};
  page.rows.push(Row{ordinal,slot,owner:name.to_owned(),path:path.to_owned(),directory,state,detail});Ok(())
 };
 for source in model.sources(){
  let primary=match source.kind(){SourceKind::Text{settings}=>usize::from(settings.file().is_some()),SourceKind::Browser{browser}if matches!(browser.location(),skylive_core::BrowserLocation::LocalFile{..})=>1,SourceKind::Image{..}|SourceKind::MediaSource{..}=>1,SourceKind::ImageSlideShow{settings}=>if settings.entries().is_empty(){settings.images().len()}else{settings.entries().len()},_=>0};
  let filters=source.video_filters().iter().filter(|f|f.id().is_some()).map(|f|usize::from(f.mask_image().is_some())+usize::from(f.lut_asset().is_some())).sum::<usize>();
  let next=total.checked_add(primary+filters).ok_or("Missing Files reference count overflow")?;
  // Skip whole source ranges before visiting their authored256-image entries;
  // pagination remains O(source count + one selected source range), not a full
  // slideshow/path traversal for every64-path continuation.
  if next<=start||total>=end{total=next;continue;}
  let mut ordinal=total;let mut emit=|slot,name:&str,path:&str,directory|{let current=ordinal;ordinal+=1;visit(current,slot,name,path,directory)};
  let id=source.id();match source.kind(){
   SourceKind::Text{settings}=>if let Some(file)=settings.file(){emit(Slot::TextFile{source:id},source.name(),file.path(),false)?;},
   SourceKind::Browser{browser}=>if let skylive_core::BrowserLocation::LocalFile{path}=browser.location(){emit(Slot::BrowserHtml{source:id},source.name(),path,false)?;},
   SourceKind::Image{asset}=>emit(Slot::Image{source:id},source.name(),asset.path(),false)?,
   SourceKind::MediaSource{media}=>if !crate::media_source::network_http::is_url(media.path()){emit(Slot::Media{source:id},source.name(),media.path(),false)?;},
   SourceKind::ImageSlideShow{settings}=>if settings.entries().is_empty(){for(i,image)in settings.images().iter().enumerate(){emit(Slot::SlideShow{source:id,entry:i as u16},source.name(),image.path(),false)?;}}else{for(i,entry)in settings.entries().iter().enumerate(){emit(Slot::SlideShow{source:id,entry:i as u16},source.name(),entry.path(),entry.kind()==SlideShowEntryKind::Directory)?;}},_=>{}
  }
  for filter in source.video_filters(){if let Some(filter_id)=filter.id(){let owner=format!("{} · {}",source.name(),filter.name());if let Some(asset)=filter.mask_image(){emit(Slot::Mask{source:id,filter:filter_id},&owner,asset.path(),false)?;}if let Some(asset)=filter.lut_asset(){emit(Slot::Lut{source:id,filter:filter_id},&owner,asset.path(),false)?;}}}
  total=next;
 }
 page.total=total;if start>total{return Err("Missing Files page cursor is outside this model".into());}if end<total{page.next=Some(end);}Ok(page)
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice{pub slot:Slot,/// None explicitly clears only optional filter resources.
pub path:Option<String>}
/// Owns only an already-published immutable model Arc; never copies its closure.
pub struct Request{pub model:Arc<StudioSnapshot>,pub choice:Choice}
fn path(text:&str)->Result<&Path,String>{if text.is_empty()||text.len()>4096||text.chars().any(char::is_control)||!Path::new(text).is_absolute(){return Err("Choose an absolute replacement path of at most 4096 UTF-8 bytes".into());}Ok(Path::new(text))}
pub fn prepare(request:Request)->Result<ResourceRelink,String>{
    let choice=request.choice;
    // Model invariants cap one slideshow entry at256 image descriptors (paths<=4096).
    // Pre-admit the worst descriptor size before resource_value clones any strings.
    let source=request.model.source(choice.slot.source()).ok_or("Missing Files source was removed")?;
    let preflight=match source.kind(){SourceKind::Browser{browser}=>std::mem::size_of::<skylive_core::BrowserSource>().saturating_add(browser.css().len()).saturating_add(match browser.location(){skylive_core::BrowserLocation::LocalFile{path}=>path.len(),skylive_core::BrowserLocation::Url{url}=>url.len()}),SourceKind::ImageSlideShow{settings}=>settings.images().iter().try_fold(0usize,|n,a|n.checked_add(std::mem::size_of::<skylive_core::ImageAsset>()+a.path().len()).ok_or("Missing Files metadata overflow"))?,_=>64*1024};
    if preflight>MAX_PREPARE_METADATA{return Err("Missing Files preparation exceeds 4 MiB metadata".into());}
    let expected=request.model.resource_value(choice.slot).map_err(|e|e.to_string())?;
    let replacement=match (&expected,choice.path.as_deref()){
        (Value::Mask{..},None)=>Value::Mask{asset:None},(Value::Lut{..},None)=>Value::Lut{asset:None},
        (_,None)=>return Err("Only an optional Image Mask or LUT resource can be cleared".into()),
        (Value::TextFile{file:old},Some(text))=>{
            let file_path=path(text)?;
            let opened=std::fs::File::open(file_path).map_err(|error|format!("{text}: {error}"))?;
            let metadata=opened.metadata().map_err(|error|format!("{text}: {error}"))?;
            if !metadata.is_file(){return Err("Text replacement must be a regular readable file".into());}
            if metadata.len()>crate::text::MAX_TEXT_FILE_BYTES&&old.last_lines().is_none(){return Err("Text file exceeds 1 MiB".into());}
            Value::TextFile{file:skylive_core::TextFile::settings(text.to_owned(),old.last_lines().is_some(),old.saved_lines()).map_err(|error|error.to_string())?}
        },
        (Value::BrowserHtml{browser},Some(text))=>{
            let file_path=path(text)?;
            // Presence/readability only; do not parse, hash or execute HTML.
            let file=std::fs::File::open(file_path).map_err(|error|format!("{text}: {error}"))?;
            if !file.metadata().map_err(|error|format!("{text}: {error}"))?.is_file(){return Err("Browser HTML replacement must be a regular readable file".into());}
            drop(file);
            Value::BrowserHtml{browser:browser.clone().with_local_file(text.to_owned()).map_err(|error|error.to_string())?}
        },
        (Value::Image{..},Some(text))=>{let prepared=crate::assets::prepare_image(path(text)?).map_err(|e|e.to_string())?;let asset=prepared.asset;drop(prepared.frame);Value::Image{asset}},
        (Value::Mask{..},Some(text))=>{let prepared=crate::assets::prepare_mask_image(path(text)?).map_err(|e|e.to_string())?;let asset=prepared.asset;drop(prepared.frame);Value::Mask{asset:Some(asset)}},
        (Value::Lut{..},Some(text))=>Value::Lut{asset:Some(crate::lut_assets::prepare_lut(path(text)?).map_err(|e|e.to_string())?)},
        (Value::Media{media:old},Some(text))=>{let prepared=crate::media_source::prepare_local_with_budget(path(text)?,&crate::mediafile::MediaReadBudget::new(256*1024*1024)).map_err(|e|e.to_string())?;if !prepared.media.has_audio()&&(!source.audio_filters().is_empty()||!source.audio_properties().is_default()||!source.audio_monitoring().is_off()){return Err("replacement media cannot discard authored audio controls".into());}if prepared.media.video_dimensions().is_none()&&!source.video_filters().is_empty(){return Err("replacement media cannot discard authored video filters".into());}Value::Media{media:prepared.media.with_playback(old.looping(),old.restart_on_activate(),old.close_when_inactive(),old.clear_on_end())}},
        (Value::SlideShow{entry:old,..},Some(text))=>{
            let entry=SlideShowEntry::new(old.kind(),path(text)?.to_str().ok_or("Replacement path is not UTF-8")?.to_owned()).map_err(|e|e.to_string())?;
            let prepared=crate::image_slideshow::directories::prepare_entries(&[entry]).map_err(|e|e.to_string())?;
            let entry=prepared.entries.into_iter().next().ok_or("Slideshow preparation returned no authored entry")?;
            Value::SlideShow{entry,images:prepared.images}
        },
    };
    let prepared=ResourceRelink{slot:choice.slot,expected,replacement};
    if prepared.metadata_bytes()>MAX_PREPARE_METADATA{return Err("Missing Files prepared row exceeds 4 MiB metadata".into());}Ok(prepared)
}
