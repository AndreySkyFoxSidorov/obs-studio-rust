//! Source-traced OBS32 slideshow v1 and explicit supported v2 settings.
//! Bounded worker-only folder expansion; no watcher, recursive scan or size guessing.
use super::*;
use skylive_core::{ImageSlideShow,SlideTransition,SlideShowCanvas,SlideShowBehavior,SlideShowMode,SlideShowRandomCompatibility,MAX_SLIDES};
pub(super) struct Input{files:Vec<PathBuf>,expansion:Option<crate::image_slideshow::directories::Expansion>,hold:u32,speed:u32,transition:SlideTransition,looping:bool,hide:bool,canvas:SlideShowCanvas,behavior:SlideShowBehavior,mode:SlideShowMode,randomize:bool,random_compatibility:SlideShowRandomCompatibility,prepared:Option<ImageSlideShow>}
fn text<'a>(map:&'a Map<String,Value>,key:&str,default:&'a str,path:&str)->Result<&'a str,ObsError>{map.get(key).map(|value|value.as_str().ok_or_else(||issue(path,"invalid_slideshow",format!("{key} must be a string")))).transpose().map(|value|value.unwrap_or(default))}
fn millis(map:&Map<String,Value>,key:&str,default:u32,path:&str)->Result<u32,ObsError>{let value=map.get(key).map(|value|value.as_u64().and_then(|v|u32::try_from(v).ok()).ok_or_else(||issue(path,"invalid_slideshow",format!("{key} must be nonnegative integer milliseconds")))).transpose()?.unwrap_or(default);if value>3_600_000{return Err(issue(path,"unsupported_slideshow",format!("{key} exceeds the supported one-hour limit")));}Ok(value)}
fn canvas(value:&str,v2:bool,path:&str)->Result<SlideShowCanvas,ObsError>{
    // Legacy OBS stores the localized label for Automatic, rather than a
    // stable token. Its parser falls back to automatic sizing whenever the
    // string does not start with a numeric resolution or aspect ratio. This
    // also covers labels containing `x`, such as Kurdish "Bixweber".
    // Keep malformed numeric settings subject to the strict validation below.
    if !v2 && !value.trim_start().starts_with(|c:char| c.is_ascii_digit() || c == '+' || c == '-') {
        return Ok(SlideShowCanvas::Auto);
    }
    let parsed=value.split_once('x').map(|v|(false,v)).or_else(||(!v2).then(||value.split_once(':').map(|v|(true,v))).flatten());
    let Some((aspect,(w,h)))=parsed else{return Err(issue(path,"unsupported_slideshow_size","slideshow size must be canonical pixels or a legacy aspect ratio; localized Auto and empty v2 canvases are not guessed"));};
    if w.is_empty()||h.is_empty()||!w.bytes().chain(h.bytes()).all(|byte|byte.is_ascii_digit()){return Err(issue(path,"invalid_slideshow_size","slideshow dimensions must be unsigned decimal integers"));}
    let width=w.parse::<u32>().map_err(|_|issue(path,"invalid_slideshow_size","slideshow width exceeds the supported integer range"))?;let height=h.parse::<u32>().map_err(|_|issue(path,"invalid_slideshow_size","slideshow height exceeds the supported integer range"))?;
    let canvas=if aspect{SlideShowCanvas::Aspect{width,height}}else{SlideShowCanvas::Resolution{width,height}};canvas.validate().map_err(|e|issue(path,"invalid_slideshow_size",e.to_string()))?;Ok(canvas)
}
impl Input{
    pub fn parse(settings:&Map<String,Value>,path:&str,base:Option<&Path>,v2:bool)->Result<Self,ObsError>{
        known(settings,&["transition_speed","use_custom_size","slide_time","transition","randomize","loop","hide","files","playback_behavior","slide_mode","playback_mode"],path)?;
        let behavior=match text(settings,"playback_behavior","always_play",path)?.to_ascii_lowercase().as_str(){"stop_restart"=>SlideShowBehavior::StopRestart,"pause_unpause"=>SlideShowBehavior::PauseUnpause,"always_play"=>SlideShowBehavior::AlwaysPlay,_=>return Err(issue(path,"unsupported_slideshow_visibility","unknown slideshow visibility behavior is not representable"))};
        let mode=match text(settings,"slide_mode","mode_auto",path)?.to_ascii_lowercase().as_str(){"mode_auto"=>SlideShowMode::Automatic,"mode_manual"=>SlideShowMode::Manual,_=>return Err(issue(path,"unsupported_slideshow_mode","unknown slideshow automatic or manual mode is not representable"))};
        let size=text(settings,"use_custom_size",if v2{"1920x1080"}else{""},path)?;let canvas=canvas(size,v2,path)?;
        let transition=match text(settings,"transition","fade",path)?.to_ascii_lowercase().as_str(){"cut"=>SlideTransition::Cut,"fade"=>SlideTransition::Fade,"swipe"=>SlideTransition::Swipe,"slide"=>SlideTransition::Slide,_=>return Err(issue(path,"unsupported_slideshow_transition","slideshow transition must be cut, fade, swipe or slide"))};
        // Both OBS slideshow versions clamp the hold before adding the
        // transition time. Preserve that effective playback for saved values
        // below the UI minimum rather than rejecting the whole collection.
        let minimum=if transition==SlideTransition::Cut{50}else{100};
        let hold=millis(settings,"slide_time",if v2{2000}else{8000},path)?.max(minimum);let speed=millis(settings,"transition_speed",700,path)?;
        ImageSlideShow::validate_timing(hold,transition,speed).map_err(|e|issue(path,"unsupported_slideshow_timing",e.to_string()))?;
        let array:&[Value]=match settings.get("files"){Some(value)=>super::array(value,&format!("{path}.files"))?.as_slice(),None=>&[]};if array.len()>MAX_SLIDES{return Err(issue(path,"slideshow_limit","slideshow import supports at most 256 file or folder entries"));}
        let mut files=Vec::with_capacity(array.len());
        for(index,value)in array.iter().enumerate(){let path=format!("{path}.files[{index}]");let file=object(value,&path)?;known(file,&["value","selected","hidden"],&path)?;boolean(file,"selected",false,&path)?;boolean(file,"hidden",false,&path)?;
            let value=string(file,"value",&path)?;if value.is_empty()||value.len()>4096||value.chars().any(char::is_control){return Err(issue(&path,"invalid_image_path","slideshow file must be an exact UTF-8 path of 1..=4096 bytes without controls"));}
            let value=PathBuf::from(value);let resolved=if value.is_absolute(){value}else{base.ok_or_else(||issue(&path,"relative_image_path","relative slideshow image requires an explicit absolute import base directory"))?.join(value)};files.push(resolved);
        }
        let (looping,randomize)=if v2{
            // OBS v2 only consults legacy flags when playback_mode has no user
            // value. Saved scenes can retain stale legacy fields after a mode
            // change, so these fields must not override the explicit mode.
            let playback=if settings.contains_key("playback_mode") {
                text(settings,"playback_mode","loop",path)?
            } else if boolean(settings,"randomize",false,path)? {
                "random"
            } else if boolean(settings,"loop",true,path)? {
                "loop"
            } else { "once" };
            match playback{"loop"=>(true,false),"once"=>(false,false),"random"=>(false,true),_=>return Err(issue(path,"unsupported_slideshow_mode","unknown slideshow playback mode is not representable"))}
        }else{if settings.contains_key("playback_mode"){return Err(issue(path,"unsupported_slideshow_mode","playback_mode belongs to slideshow_v2, not the legacy source"));}(boolean(settings,"loop",true,path)?,boolean(settings,"randomize",false,path)?)};
        let random_compatibility=if !v2&&randomize{SlideShowRandomCompatibility::LegacyV1}else{SlideShowRandomCompatibility::BufferedV2};
        Ok(Self{files,expansion:None,hold,speed,transition,looping,hide:boolean(settings,"hide",false,path)?,canvas,behavior,mode,randomize,random_compatibility,prepared:None})
    }
    pub fn file_count(&self)->usize{self.expansion.as_ref().map_or(self.files.len(),|plan|plan.image_count())}
    pub fn preflight(&mut self,path:&str,budget:&mut crate::image_slideshow::directories::Budget)->Result<(),ObsError>{let entries=crate::image_slideshow::directories::classify(&self.files,budget).map_err(|error|issue(path,"slideshow_entries",error))?;self.expansion=Some(crate::image_slideshow::directories::expand(&entries,budget).map_err(|error|issue(path,"slideshow_entries",error))?);Ok(())}
    pub fn prepare(&mut self,path:&str,reads:&mut crate::assets::ReadBudget,decoded:&mut usize,budget:&mut crate::image_slideshow::directories::Budget)->Result<(),ObsError>{
        let expansion=self.expansion.take().ok_or_else(||issue(path,"invalid_slideshow","slideshow entries were not preflighted"))?;let prepared=expansion.prepare(budget,reads,decoded,MAX_IMPORT_IMAGE_BYTES).map_err(|error|issue(path,"slideshow_entries",error))?;
        self.prepared=Some(ImageSlideShow::new_with_options(prepared.images,self.hold,self.transition,self.speed,self.looping,false,self.hide,self.canvas,self.behavior).and_then(|settings|settings.with_mode(self.mode)).and_then(|settings|settings.with_randomization(self.randomize,self.random_compatibility)).and_then(|settings|settings.with_entries(prepared.entries,prepared.counts)).map_err(|error|issue(path,"invalid_slideshow",error.to_string()))?);Ok(())
    }
    pub fn settings(&self)->Option<&ImageSlideShow>{self.prepared.as_ref()}
}
pub(super) fn versioned_id(settings:&ImageSlideShow)->&'static str{if settings.randomize()&&settings.random_compatibility()==SlideShowRandomCompatibility::BufferedV2{"slideshow_v2"}else{"slideshow"}}
pub(super) fn export(settings:&ImageSlideShow,path:&str)->Result<Value,ObsError>{
    if settings.shuffle(){return Err(issue(path,"unsupported_slideshow_random","native stable shuffle cannot be exported as OBS successive random playback"));}
    let entries=settings.authored_entries();let mut files=Vec::with_capacity(entries.len());for entry in &entries{if !Path::new(entry.path()).is_absolute(){return Err(issue(path,"image_path",format!("OBS slideshow export requires an absolute file or folder path: {}",entry.path())));}files.push(json!({"value":entry.path()}));}
    let transition=match settings.transition(){SlideTransition::Cut=>"cut",SlideTransition::Fade=>"fade",SlideTransition::Swipe=>"swipe",SlideTransition::Slide=>"slide"};
    let modern_random=settings.randomize()&&settings.random_compatibility()==SlideShowRandomCompatibility::BufferedV2;
    if modern_random&&!matches!(settings.canvas(),SlideShowCanvas::Resolution{..}){return Err(issue(path,"unsupported_slideshow_size","modern OBS random export requires an explicit pixel canvas; automatic and aspect authoring cannot be silently fixed"));}
    let canvas=match settings.canvas(){SlideShowCanvas::Auto=>String::new(),SlideShowCanvas::Resolution{width,height}=>format!("{width}x{height}"),SlideShowCanvas::Aspect{width,height}=>format!("{width}:{height}")};let behavior=match settings.behavior(){SlideShowBehavior::StopRestart=>"stop_restart",SlideShowBehavior::PauseUnpause=>"pause_unpause",SlideShowBehavior::AlwaysPlay=>"always_play"};
    let mut value=json!({"transition_speed":settings.transition_ms(),"use_custom_size":canvas,"slide_time":settings.duration_ms(),"transition":transition,"randomize":settings.randomize(),"loop":settings.looping(),"hide":settings.hide_when_done(),"files":files,"playback_behavior":behavior,"slide_mode":match settings.mode(){SlideShowMode::Automatic=>"mode_auto",SlideShowMode::Manual=>"mode_manual"}});if modern_random{value.as_object_mut().expect("object literal").insert("playback_mode".into(),json!("random"));}Ok(value)
}
