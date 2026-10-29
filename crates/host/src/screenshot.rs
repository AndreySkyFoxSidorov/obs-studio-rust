//! Explicit immutable render-to-PNG operation. Invoke only on a host I/O worker.
use crate::{recording::RecordingFrame,render_gpu::RenderGpu,preview::FilterPreview};
use skylive_core::{Id,VideoFilterPreviewTarget};
use image::ImageEncoder;
use std::{fs::{self,File,OpenOptions},io::{self,Write},path::{Path,PathBuf},sync::atomic::{AtomicBool,AtomicU64,Ordering}};

#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize,serde::Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Target {Program,Preview,Scene {scene:Id},Source {source:Id}}
impl Target {pub fn label(self)->&'static str{match self {Self::Program=>"Save Program Screenshot",Self::Preview=>"Save Preview Screenshot",Self::Scene {..}=>"Save Scene Screenshot",Self::Source {..}=>"Save Source Screenshot"}}}
static ACTIVE:AtomicBool=AtomicBool::new(false);
static NEXT:AtomicU64=AtomicU64::new(1);
const MAX_PNG:usize=64*1024*1024;
struct Permit;impl Drop for Permit{fn drop(&mut self){ACTIVE.store(false,Ordering::Release);}}
pub struct Request {pub path:PathBuf,pub target:Target,pub frame:RecordingFrame,pub preview:Id,pub studio:bool,pub gpu:RenderGpu}
#[derive(serde::Serialize)]
pub struct Receipt {pub path:PathBuf,pub width:u32,pub height:u32,pub bytes:u64,pub session:u64,pub revision:u64,pub time_ns:u64,pub target:Target}

pub fn validate_path(path:&Path)->Result<(),String>{
    let text=path.to_str().ok_or("Screenshot path must be valid UTF-8.")?;
    if text.is_empty()||text.len()>4096||text.chars().any(char::is_control)||!path.is_absolute()
        ||!path.extension().and_then(|ext|ext.to_str()).is_some_and(|ext|ext.eq_ignore_ascii_case("png")){return Err("Choose an absolute PNG path, at most4096 bytes and without control characters.".into());}
    Ok(())
}
pub fn save(request:Request)->Result<Receipt,String>{
    validate_path(&request.path)?;
    match fs::symlink_metadata(&request.path) {
        Ok(_)=>return Err("Screenshot destination already exists. Choose another name.".into()),
        Err(error) if error.kind()==io::ErrorKind::NotFound=>{},
        Err(error)=>return Err(format!("{}: {error}",request.path.display())),
    }
    ACTIVE.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_|"Another screenshot is still rendering or publishing.".to_owned())?;let _permit=Permit;
    let _services=crate::preview::retain_render_services();
    let Request {path,target,frame,preview,studio,gpu}=request;
    if frame.playback.session()!=frame.session||frame.capture.session()!=frame.session||frame.playback.revision()!=frame.model.revision()||frame.playback.now()!=frame.time{return Err("Screenshot inputs belong to another document, revision or time.".into());}
    let mut renderer=gpu.output_renderer();renderer.resource_generation(frame.resource_generation);
    let projection=match target {
        Target::Scene {scene}=>Some(VideoFilterPreviewTarget::Scene(scene)),
        Target::Source {source}=>Some(VideoFilterPreviewTarget::Source(source)),
        _=>None,
    };
    let image=if let Some(target)=projection {
        let filters=match target {VideoFilterPreviewTarget::Source(id)=>frame.model.source(id).ok_or("Screenshot source no longer exists.")?.video_filters().to_vec(),VideoFilterPreviewTarget::Scene(id)=>frame.model.scene_source(id).map_or_else(Vec::new,|source|source.video_filters().to_vec())};
        FilterPreview::prepare(frame.session,&frame.model,target,filters,&frame.capture).map_err(|e|e.to_string())?.render_image(&mut renderer,&frame.playback,&frame.capture).map_err(|e|e.to_string())?
    }else{
        let output=if target==Target::Preview&&studio {crate::transition::FrameComposition::Scene(crate::transition::FrameScene::Live(preview))}else{frame.output.clone()};
        let result=renderer.render_live(&frame.model,&output,frame.model.video().width(),frame.model.video().height(),&frame.playback,&frame.capture).map_err(|e|e.to_string())?;
        if !result.diagnostics.is_empty(){return Err(format!("Screenshot has unavailable sources: {:?}",result.diagnostics));}
        result.into_straight()
    };
    if image.byte_len()>40*1024*1024{return Err("Screenshot exceeds the40MiB SDR render profile.".into());}
    let (session,revision,time_ns)=(frame.session,frame.model.revision(),frame.time.0);
    // The saved image is owned. Slow PNG/filesystem work must not keep the
    // input CaptureSnapshot or render-cache source/native buffer credits alive.
    drop(renderer);drop(frame);drop(gpu);
    let parent=path.parent().ok_or("Screenshot has no parent directory.")?;
    let mut reserved=None;
    for _ in 0..32 {let serial=NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|"Screenshot identity exhausted.")?;let folder=parent.join(format!(".skylive-screenshot-{}-{serial}",std::process::id()));match fs::create_dir(&folder){Ok(())=>{reserved=Some(folder);break;},Err(e)if e.kind()==io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(format!("{}: {e}",folder.display()))}}
    let folder=reserved.ok_or("Screenshot temporary-directory collision limit.")?;let temporary=folder.join("image.part.png");
    let encoded=(||->Result<u64,String>{let file=OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e|e.to_string())?;
        let mut writer=Bounded {file,bytes:0};
        image::codecs::png::PngEncoder::new_with_quality(&mut writer,image::codecs::png::CompressionType::Fast,image::codecs::png::FilterType::Adaptive)
            .write_image(image.pixels(),image.width(),image.height(),image::ExtendedColorType::Rgba8).map_err(|e|e.to_string())?;
        writer.file.sync_all().map_err(|e|e.to_string())?;Ok(writer.bytes as u64)})();
    let bytes=match encoded {Ok(bytes)=>bytes,Err(error)=>{let _=fs::remove_file(&temporary);let _=fs::remove_dir(&folder);return Err(format!("PNG encoding failed before publication: {error}"));}};
    let publication=crate::private_output::publish(&temporary,&path).map_err(|e|format!("Screenshot was not published at{}: {e}. Complete PNG retained at{}",path.display(),temporary.display()))?;
    publication.remove_temporary(&temporary).and_then(|()|fs::remove_dir(&folder)).map_err(|e|format!("Screenshot saved at{}, but temporary cleanup failed at{}: {e}",path.display(),folder.display()))?;
    Ok(Receipt {path,width:image.width(),height:image.height(),bytes,session,revision,time_ns,target})
}
struct Bounded {file:File,bytes:usize}
impl Write for Bounded {fn write(&mut self,bytes:&[u8])->io::Result<usize>{if bytes.len()>MAX_PNG.saturating_sub(self.bytes){return Err(io::Error::other("PNG exceeds64MiB encoded output budget"));}let written=self.file.write(bytes)?;self.bytes+=written;Ok(written)}fn flush(&mut self)->io::Result<()>{self.file.flush()}}
