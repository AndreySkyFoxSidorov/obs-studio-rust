//! Exact OBS32 ffmpeg_source local-file subset. IO only on the importing worker.
use super::*;
use skylive_core::MediaSource;
pub(super) const MAX_SOURCES:usize=skylive_core::MAX_SOURCES;
pub(super) const MAX_READ_BYTES:u64=256*1024*1024;
pub(super) struct Input {
    file:PathBuf,network:Option<String>,looping:bool,restart:bool,close:bool,clear:bool,speed:u16,prepared:Option<MediaSource>,
}
impl Input {
    pub(super) fn parse(settings:&Map<String,Value>,path:&str,base:Option<&Path>)->Result<Self,ObsError>{
        known(settings,&["is_local_file","local_file","input","input_format","looping","clear_on_media_end","restart_on_activate","close_when_inactive","hw_decode","color_range","speed_percent","ffmpeg_options","full_decode","linear_alpha","reconnect_delay_sec","buffering_mb","seekable","is_stinger","is_track_matte","log_changes"],path)?;
        let local=boolean(settings,"is_local_file",true,path)?;
        // OBS keeps network settings when the user switches back to a local
        // file. Its local branch ignores the input URL, format and reconnect
        // delay; seekable is redundant for an already-local file. Validate
        // their types without rejecting these inactive, retained values.
        for field in ["input","input_format"] {
            if settings.contains_key(field) { string(settings,field,path)?; }
        }
        if let Some(value)=settings.get("reconnect_delay_sec") {
            if value.as_i64().is_none() {return Err(issue(&format!("{path}.reconnect_delay_sec"),"invalid","expected an integer"));}
            if !local && value.as_i64()!=Some(10) {return Err(issue(path,"media_network_reconnect_unsupported","Only the default ten-second network reconnect delay is supported"));}
        }
        if boolean(settings,"seekable",false,path)? && !local {return Err(issue(path,"media_network_seekable_unsupported","Explicit OBS network seekable mode is not supported"));}
        if !local && settings.contains_key("input_format") && !string(settings,"input_format",path)?.is_empty() {return Err(issue(path,"media_network_format_unsupported","Explicit OBS network input format is not supported"));}
        // This OBS setting controls diagnostic logging only.
        boolean(settings,"log_changes",true,path)?;
        neutral(settings,"ffmpeg_options",json!(""),path)?;
        for field in ["hw_decode","full_decode","linear_alpha","is_stinger","is_track_matte"] {neutral(settings,field,json!(false),path)?;}
        for(field,value)in [("color_range",0),("buffering_mb",2)] {neutral(settings,field,json!(value),path)?;}
        let raw_speed=settings.get("speed_percent").map(|v|v.as_i64().ok_or_else(||issue(path,"invalid","speed_percent must be an integer"))).transpose()?.unwrap_or(100);
        let speed=if (1..=200).contains(&raw_speed){raw_speed as u16}else{100};
        let value=string(settings,if local{"local_file"}else{"input"},path)?;
        if !local {
            crate::media_source::network_http::Request::http_url(value)
                .map_err(|error|issue(path,"media_network_unsupported",error.to_string()))?;
            return Ok(Self {file:PathBuf::new(),network:Some(value.to_owned()),looping:boolean(settings,"looping",false,path)?,restart:boolean(settings,"restart_on_activate",true,path)?,close:boolean(settings,"close_when_inactive",false,path)?,clear:boolean(settings,"clear_on_media_end",true,path)?,speed,prepared:None});
        }
        if value.trim().is_empty() || value.len()>16384 || value.chars().any(char::is_control) || value.contains("://") {
            return Err(issue(path,"media_path_invalid","OBS local Media Source requires a nonempty filesystem path without a URL scheme or control characters"));
        }
        let file=PathBuf::from(value);let file=if file.is_absolute(){file}else{
            let base=base.filter(|base|base.is_absolute()).ok_or_else(||issue(path,"media_base_required","Relative OBS media paths require an explicit absolute import directory"))?;base.join(file)
        };
        if file.to_str().is_none_or(|value|value.len()>16384){return Err(issue(path,"media_path_invalid","OBS local Media Source requires a nonempty filesystem path without a URL scheme or control characters"));}
        Ok(Self {file,network:None,looping:boolean(settings,"looping",false,path)?,restart:boolean(settings,"restart_on_activate",true,path)?,close:boolean(settings,"close_when_inactive",false,path)?,clear:boolean(settings,"clear_on_media_end",true,path)?,speed,prepared:None})
    }
    pub(super) fn prepare(&mut self,path:&str,budget:&crate::mediafile::MediaReadBudget)->Result<(),ObsError>{
        let prepared=match &self.network {Some(url)=>crate::media_source::prepare_finite_http_with_budget(url,budget),None=>crate::media_source::prepare_local_with_budget(&self.file,budget)}.map_err(|error|issue(path,error.code(),error.to_string()))?;
        // The production reader proves container, codec profile, actual dimensions
        // and audio presence; OBS string hints never substitute for this proof.
        if prepared.info.ignored_non_av_tracks!=0 {return Err(issue(path,"media_tracks_unsupported","OBS Media Source import does not discard enabled non-audio/video tracks"));}
        self.prepared=Some(prepared.media.with_playback(self.looping,self.restart,self.close,self.clear).with_speed(self.speed).map_err(|e|issue(path,"invalid_media_speed",e.to_string()))?);Ok(())
    }
    pub(super) fn settings(&self)->Option<&MediaSource>{self.prepared.as_ref()}
    pub(super) fn dimensions(&self)->Option<(u32,u32)>{self.settings().and_then(MediaSource::video_dimensions)}
    pub(super) fn has_audio(&self)->bool{self.settings().is_some_and(MediaSource::has_audio)}
    pub(super) fn audio_only(&self)->bool{self.settings().is_some_and(|value|value.video_dimensions().is_none()&&value.has_audio())}
}
pub(super) fn export(media:&MediaSource,path:&str)->Result<Value,ObsError>{
    if crate::media_source::network_http::is_url(media.path()) {
        crate::media_source::network_http::Request::http_url(media.path())
            .map_err(|error|issue(path,"media_export_url",error.to_string()))?;
        return Ok(json!({"is_local_file":false,"local_file":"","input":media.path(),"input_format":"","looping":media.looping(),"restart_on_activate":media.restart_on_activate(),"close_when_inactive":media.close_when_inactive(),"clear_on_media_end":media.clear_on_end(),"hw_decode":false,"color_range":0,"speed_percent":media.speed_percent(),"ffmpeg_options":"","full_decode":false,"linear_alpha":false,"reconnect_delay_sec":10,"buffering_mb":2,"seekable":false,"is_stinger":false,"is_track_matte":false,"log_changes":true}));
    }
    if !Path::new(media.path()).is_absolute() || media.path().contains("://"){return Err(issue(path,"media_export_path","OBS Media Source export requires an absolute local filesystem path on this host"));}
    // Serialization uses current canonical playback settings, not imported raw
    // settings. It does not open/revalidate or promise an unchanged linked file.
    Ok(json!({"is_local_file":true,"local_file":media.path(),"looping":media.looping(),"restart_on_activate":media.restart_on_activate(),"close_when_inactive":media.close_when_inactive(),"clear_on_media_end":media.clear_on_end(),"hw_decode":false,"color_range":0,"speed_percent":media.speed_percent(),"ffmpeg_options":"","full_decode":false,"linear_alpha":false,"reconnect_delay_sec":10,"buffering_mb":2,"seekable":false,"is_stinger":false,"is_track_matte":false,"log_changes":true,"input":"","input_format":""}))
}
