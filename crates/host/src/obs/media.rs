//! Exact OBS32 ffmpeg_source local-file subset. IO only on the importing worker.
use super::*;
use skylive_core::MediaSource;
pub(super) const MAX_SOURCES:usize=skylive_core::MAX_SOURCES;
pub(super) const MAX_READ_BYTES:u64=256*1024*1024;
pub(super) struct Input {
    file:PathBuf,network:Option<String>,inactive_network:Option<(String,String)>,inactive_local:Option<String>,seekable:bool,looping:bool,restart:bool,close:bool,clear:bool,speed:u16,reconnect:u8,prepared:Option<MediaSource>,
}
impl Input {
    pub(super) fn parse(settings:&Map<String,Value>,path:&str,base:Option<&Path>)->Result<Self,ObsError>{
        known(settings,&["is_local_file","local_file","input","input_format","looping","clear_on_media_end","restart_on_activate","close_when_inactive","hw_decode","color_range","speed_percent","ffmpeg_options","full_decode","linear_alpha","reconnect_delay_sec","buffering_mb","seekable","is_stinger","is_track_matte","log_changes"],path)?;
        let local=boolean(settings,"is_local_file",true,path)?;
        // OBS keeps network settings when the user switches back to a local
        // file. Its local branch ignores the input URL, format and reconnect
        // delay; seekable is redundant for an already-local file. Validate
        // their types without rejecting these inactive, retained values.
        for field in ["input","input_format","local_file"] {
            if settings.contains_key(field) { string(settings,field,path)?; }
        }
        let reconnect=match settings.get("reconnect_delay_sec") {
            None=>10,
            Some(value)=>match value.as_i64(){Some(0)=>10,Some(seconds @ 1..=60)=>seconds as u8,Some(_) if local=>10,
                Some(_)=>return Err(issue(path,"media_network_reconnect_unsupported","Reconnect delay must be 1..60 seconds")),
                None=>return Err(issue(&format!("{path}.reconnect_delay_sec"),"invalid","expected an integer"))},
        };
        let seekable=boolean(settings,"seekable",false,path)?;
        let network_format=settings.get("input_format").and_then(Value::as_str).unwrap_or("");
        // This host's finite HTTP reader parses the ISO BMFF/MOV container.
        // FFmpeg registers these names as aliases of the same MOV demuxer.
        // Preserve the exact authored value; actual media still needs to pass
        // our parser and decoder capability checks.
        if !local && !crate::media_source::network_http::supports_input_format(network_format) {return Err(issue(path,"media_network_format_unsupported","Only automatic detection and MOV-family demuxer names are supported"));}
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
            let inactive=settings.get("local_file").and_then(Value::as_str).unwrap_or("");
            if inactive.len()>16_384{return Err(issue(path,"media_local_settings_limit","Retained local file exceeds 16384 bytes"));}
            let inactive_local=(!inactive.is_empty()).then(||inactive.to_owned());
            let inactive_network=(!network_format.is_empty()).then(||(String::new(),network_format.to_owned()));
            return Ok(Self {file:PathBuf::new(),network:Some(value.to_owned()),inactive_network,inactive_local,seekable,looping:boolean(settings,"looping",false,path)?,restart:boolean(settings,"restart_on_activate",true,path)?,close:boolean(settings,"close_when_inactive",false,path)?,clear:boolean(settings,"clear_on_media_end",true,path)?,speed,reconnect,prepared:None});
        }
        if value.trim().is_empty() || value.len()>16384 || value.chars().any(char::is_control) || value.contains("://") {
            return Err(issue(path,"media_path_invalid","OBS local Media Source requires a nonempty filesystem path without a URL scheme or control characters"));
        }
        let file=PathBuf::from(value);let file=if file.is_absolute(){file}else{
            let base=base.filter(|base|base.is_absolute()).ok_or_else(||issue(path,"media_base_required","Relative OBS media paths require an explicit absolute import directory"))?;base.join(file)
        };
        if file.to_str().is_none_or(|value|value.len()>16384){return Err(issue(path,"media_path_invalid","OBS local Media Source requires a nonempty filesystem path without a URL scheme or control characters"));}
        let input=settings.get("input").and_then(Value::as_str).unwrap_or("");
        let format=settings.get("input_format").and_then(Value::as_str).unwrap_or("");
        if input.len()>16_384||format.len()>16_384{return Err(issue(path,"media_network_settings_limit","Retained network fields exceed 16384 bytes"));}
        let inactive_network=if input.is_empty()&&format.is_empty(){None}else{Some((input.to_owned(),format.to_owned()))};
        Ok(Self {file,network:None,inactive_network,inactive_local:None,seekable,looping:boolean(settings,"looping",false,path)?,restart:boolean(settings,"restart_on_activate",true,path)?,close:boolean(settings,"close_when_inactive",false,path)?,clear:boolean(settings,"clear_on_media_end",true,path)?,speed,reconnect,prepared:None})
    }
    pub(super) fn prepare(&mut self,path:&str,budget:&crate::mediafile::MediaReadBudget)->Result<(),ObsError>{
        let prepared=match &self.network {Some(url)=>crate::media_source::prepare_finite_http_with_budget(url,budget),None=>crate::media_source::prepare_local_with_budget(&self.file,budget)}.map_err(|error|issue(path,error.code(),error.to_string()))?;
        // The production reader proves container, codec profile, actual dimensions
        // and audio presence; OBS string hints never substitute for this proof.
        // OBS media-playback routes packets only to its chosen audio and video
        // decoders (get_packet_decoder); subtitle and data tracks are ignored.
        // Their presence therefore does not make the source unrepresentable.
        self.prepared=Some(prepared.media.with_playback(self.looping,self.restart,self.close,self.clear).with_speed(self.speed).and_then(|media|media.with_reconnect_delay(self.reconnect)).and_then(|media|media.with_obs_inactive_network(self.inactive_network.clone())).and_then(|media|media.with_obs_inactive_local(self.inactive_local.clone())).map(|media|media.with_obs_seekable(self.seekable)).map_err(|e|issue(path,"invalid_media_settings",e.to_string()))?);Ok(())
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
        let format=media.obs_inactive_network().map_or("",|(_,format)|format);
        if !crate::media_source::network_http::supports_input_format(format) {return Err(issue(path,"media_network_format_unsupported","Only automatic detection and MOV-family demuxer names are supported"));}
        return Ok(json!({"is_local_file":false,"local_file":media.obs_inactive_local().unwrap_or(""),"input":media.path(),"input_format":format,"looping":media.looping(),"restart_on_activate":media.restart_on_activate(),"close_when_inactive":media.close_when_inactive(),"clear_on_media_end":media.clear_on_end(),"hw_decode":false,"color_range":0,"speed_percent":media.speed_percent(),"ffmpeg_options":"","full_decode":false,"linear_alpha":false,"reconnect_delay_sec":media.reconnect_delay_sec(),"buffering_mb":2,"seekable":media.obs_seekable(),"is_stinger":false,"is_track_matte":false,"log_changes":true}));
    }
    if !Path::new(media.path()).is_absolute() || media.path().contains("://"){return Err(issue(path,"media_export_path","OBS Media Source export requires an absolute local filesystem path on this host"));}
    // Serialization uses current canonical playback settings, not imported raw
    // settings. It does not open/revalidate or promise an unchanged linked file.
    let (input,input_format)=media.obs_inactive_network().unwrap_or(("",""));
    Ok(json!({"is_local_file":true,"local_file":media.path(),"looping":media.looping(),"restart_on_activate":media.restart_on_activate(),"close_when_inactive":media.close_when_inactive(),"clear_on_media_end":media.clear_on_end(),"hw_decode":false,"color_range":0,"speed_percent":media.speed_percent(),"ffmpeg_options":"","full_decode":false,"linear_alpha":false,"reconnect_delay_sec":media.reconnect_delay_sec(),"buffering_mb":2,"seekable":media.obs_seekable(),"is_stinger":false,"is_track_matte":false,"log_changes":true,"input":input,"input_format":input_format}))
}
