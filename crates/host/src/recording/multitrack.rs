//! Immutable recording-only track contract. Real PCM routing is supplied by
//! the producer adapter; this module never duplicates one mix into six tracks.
use super::*;
use skylive_core::{AudioTrackId,RecordingTracks};
#[derive(Clone,Debug,PartialEq,Eq)]pub struct RecordingTrackConfig{pub id:AudioTrackId,pub name:String,pub audio:RecordingAudio}
#[derive(Clone,Debug,PartialEq)]pub struct MultitrackRecordingConfig{base:RecordingConfig,tracks:Vec<RecordingTrackConfig>,legacy:bool}
impl MultitrackRecordingConfig{
    pub fn without_audio(mut self)->Self{self.base.audio=None;self.tracks.clear();self.legacy=true;self}

    pub fn new(base:RecordingConfig,tracks:Vec<RecordingTrackConfig>)->Result<Self,RecordingError>{Self::checked(base,tracks,false)}
    fn checked(base:RecordingConfig,mut tracks:Vec<RecordingTrackConfig>,legacy:bool)->Result<Self,RecordingError>{
        base.validate()?;if tracks.len()>6||tracks.capacity()>8||base.audio.is_some()!=!tracks.is_empty(){return Err(RecordingError::invalid("recording track count/audio enable differs from captured base format"));}
        tracks.sort_by_key(|t|t.id);for index in 0..tracks.len(){
            let track=&tracks[index];
            if track.name.len()>256||track.name.chars().any(char::is_control)||index!=0&&tracks[index-1].id==track.id||![96,128,160,192].contains(&track.audio.bitrate_kbps){return Err(RecordingError::invalid("recording track identity/name/bitrate is invalid"));}
            if base.audio.is_none_or(|a|a.sample_rate!=track.audio.sample_rate||a.channels!=track.audio.channels){return Err(RecordingError::invalid("all recording mixes must use the captured project audio sample grid/channels"));}
            // Typed callers can supply a short String with large spare
            // capacity; the immutable plan retains only its admitted name.
            tracks[index].name=std::mem::take(&mut tracks[index].name).into_boxed_str().into_string();
        }
        let value=Self{base,tracks:tracks.into_boxed_slice().into_vec(),legacy};value.spool_limits()?;Ok(value)
    }
    pub fn from_session(session:&Session,path:PathBuf)->Result<Self,RecordingError>{let model=session.snapshot();let settings=model.profiles().selected().recording();let base=RecordingConfig::from_session_base(session,path)?;let tracks=Self::resolve(&base,settings.audio_tracks())?;Self::checked(base,tracks,settings.audio_tracks().is_default())}
    fn resolve(base:&RecordingConfig,settings:&RecordingTracks)->Result<Vec<RecordingTrackConfig>,RecordingError>{settings.validate().map_err(|e|RecordingError::native("recording track settings",e))?;let Some(audio)=base.audio else{return Ok(Vec::new());};let mut tracks=Vec::with_capacity(6);for id in settings.selected(){let entry=settings.entry(id);tracks.push(RecordingTrackConfig{id,name:entry.name().into(),audio:RecordingAudio{bitrate_kbps:entry.resolved_bitrate(audio.bitrate_kbps),..audio}});}Ok(tracks)}
    pub(super) fn legacy(base:&RecordingConfig)->Result<Self,RecordingError>{let tracks=base.audio.map(|audio|RecordingTrackConfig{id:AudioTrackId::ONE,name:String::new(),audio}).into_iter().collect();Self::checked(base.clone(),tracks,true)}
    pub fn base(&self)->&RecordingConfig{&self.base}pub fn tracks(&self)->&[RecordingTrackConfig]{&self.tracks}pub(super) fn legacy_names(&self)->bool{self.legacy}
    pub fn spool_limits(&self)->Result<skylive_mp4::SpoolLimits,RecordingError>{
        #[cfg(target_os="windows")]if self.legacy{return super::metadata_spool::limits(&self.base);}
        let video=(86400u128*u128::from(self.base.video.fps_num())).div_ceil(u128::from(self.base.video.fps_den()));let audio=self.tracks.iter().try_fold(0u128,|sum,t|sum.checked_add((86400u128*u128::from(t.audio.sample_rate)).div_ceil(1024)+2).ok_or_else(||RecordingError::invalid("recording sample budget overflow")))?;
        let records=u64::try_from(video+audio+8).map_err(|_|RecordingError::invalid("recording sample count overflow"))?;let count=self.tracks.len() as u64+1;
        let moov=records.checked_mul(24).and_then(|n|n.checked_add(count*(65536+512)+256)).ok_or_else(||RecordingError::invalid("recording metadata byte budget overflow"))?.max(65536).min(1024*1024*1024);
        Ok(skylive_mp4::SpoolLimits{max_records:records,max_spool_bytes:skylive_mp4::SpoolLimits::required_spool_bytes(records).map_err(|e|RecordingError::native("recording journal budget",e))?,max_output_bytes:64*1024*1024*1024,max_duration_seconds:86400,max_moov_bytes:moov,max_scan_records:records.checked_mul(1+4*count).ok_or_else(||RecordingError::invalid("recording metadata scan budget overflow"))?})
    }
}
/// Adapter for an owner that already admits typed per-track PCM. It captures
/// the complete validated plan, and cannot silently adopt another base config.
pub fn native_multitrack_factory(plan:MultitrackRecordingConfig)->RecordingFactory{
    Box::new(move|base,path|{
        if base!=plan.base(){return Err(RecordingError::invalid("recording track plan differs from admitted base config"));}
        #[cfg(target_os="windows")]{Ok(Box::new(super::native_windows::Writer::new_multitrack(&plan,path)?))}
        #[cfg(not(target_os="windows"))]{let _=(plan,path);Err(RecordingError::invalid("six-track native recording requires the Windows adapter"))}
    })
}
