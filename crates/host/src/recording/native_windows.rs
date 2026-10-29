//! Windows packet recording: one native AVC plus independently admitted AAC
//! tracks on this one recording owner. Encoded interleave never stores raw
//! delayed FullHD frames. Native context credits survive actual COM shutdown.
use super::{PreparedVideo,RecordingConfig,RecordingError,RecordingWriter,MultitrackRecordingConfig};
use super::packet_mux::{PacketMux,AudioHeader};
use crate::{native_h264 as avc,native_aac as aac};
use skylive_core::AudioTrackId;
use skylive_media::{RgbaFrame,recording_nv12};
use skylive_mp4::Edit;
use std::{path::Path,sync::atomic::{AtomicU64,AtomicUsize,Ordering}};
static NEXT_PACKET_EPOCH:AtomicU64=AtomicU64::new(1);
static PACKET_RECORDERS:AtomicUsize=AtomicUsize::new(0);
struct PacketPermit;
impl PacketPermit{fn acquire()->Result<Self,RecordingError>{PACKET_RECORDERS.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|(n<2).then_some(n+1)).map_err(|_|RecordingError::invalid("two packet recording owners active or retiring"))?;Ok(Self)}}
impl Drop for PacketPermit{fn drop(&mut self){PACKET_RECORDERS.fetch_sub(1,Ordering::AcqRel);}}
struct AudioState{
    id:AudioTrackId,name:String,config:aac::Config,encoder:Box<dyn aac::Adapter>,track:usize,
    pending:Option<aac::AccessUnit>,first:Option<i64>,origin:Option<u64>,next:u64,valid_end:i64,valid_end_ns:u64,
}
impl AudioState{
    fn anchor(&self)->Result<i64,RecordingError>{let origin=self.origin.ok_or_else(||RecordingError::invalid("recording AAC has no actual mix anchor"))?;i64::try_from(u128::from(origin)*10_000_000/u128::from(self.config.sample_rate())).map_err(|_|RecordingError::invalid("recording AAC anchor overflow"))}
}
pub struct Writer{
    mux:Option<PacketMux>,config:RecordingConfig,multitrack:bool,
    video:Box<dyn avc::Adapter>,vc:avc::Config,header:avc::CodecConfiguration,video_track:usize,
    audios:[Option<AudioState>;6],pending_video:Option<avc::AccessUnit>,video_first:Option<i64>,video_end:i64,
    finishing:bool,completed:bool,finalization_error:Option<RecordingError>,packet_limit:u64,
    // Last: neither request_stop nor a failure releases actual native ownership.
    _permit:PacketPermit,
}
fn err(op:&str,e:impl std::fmt::Display)->RecordingError{RecordingError::native(op,e)}
impl Writer{
    pub fn new(config:&RecordingConfig,path:&Path)->Result<Self,RecordingError>{Self::new_multitrack(&MultitrackRecordingConfig::legacy(config)?,path)}
    pub fn new_multitrack(plan:&MultitrackRecordingConfig,path:&Path)->Result<Self,RecordingError>{
        let config=plan.base();config.validate()?;let limits=plan.spool_limits()?;let permit=PacketPermit::acquire()?;
        // Atomic all-track admission BEFORE any native object is constructed.
        // Unused reservations have no native objects; transferred permits live
        // through each AAC Context's MFShutdown/CoUninitialize on this owner.
        let mut reserved=aac::admission::Reservation::acquire(plan.tracks().len()).map_err(|e|err("reserve recording physical AAC contexts",e))?;
        let epoch=NEXT_PACKET_EPOCH.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|RecordingError::invalid("recording packet epoch exhausted"))?;
        let gop=u32::try_from((u64::from(config.keyframe_interval_seconds)*u64::from(config.video.fps_num()))/u64::from(config.video.fps_den())).map_err(|_|RecordingError::invalid("recording GOP overflow"))?.max(1);
        let vc=avc::Config::recording(avc::Generation{session:1,output:epoch,encoder:epoch},config.video.width(),config.video.height(),config.video.fps_num(),config.video.fps_den(),config.video_bitrate_kbps*1000,gop,config.encoder).map_err(|e|err("recording H264 configuration",e))?;
        let video=avc::create_adapter(&vc).map_err(|e|err("recording native H264 initialize",e))?;
        let header=video.configuration().cloned().ok_or_else(||RecordingError::invalid("recording native SPS/PPS unavailable"))?;
        let mut audios:[Option<AudioState>;6]=std::array::from_fn(|_|None);
        for selected in plan.tracks(){
            let ac=aac::Config::with_sample_rate(aac::Generation{session:1,output:epoch,encoder:u64::from(selected.id.get())},selected.audio.sample_rate,u16::from(selected.audio.channels),selected.audio.bitrate_kbps*1000).map_err(|e|err("recording independent AAC configuration",e))?;
            let encoder=aac::create_reserved(&ac,reserved.take().map_err(|e|err("recording AAC reservation",e))?).map_err(|e|err("recording independent native AAC initialize",e))?;
            let name=if !plan.legacy_names()&&selected.name.is_empty(){format!("Track{}",selected.id.get())}else{selected.name.clone()};
            audios[selected.id.index()]=Some(AudioState{id:selected.id,name,config:ac,encoder,track:usize::MAX,pending:None,first:None,origin:None,next:0,valid_end:0,valid_end_ns:0});
        }
        let headers=audios.iter().flatten().map(|a|AudioHeader{id:a.id,name:&a.name,config:a.encoder.configuration()}).collect::<Vec<_>>();
        let(mux,video_track,mapping)=PacketMux::new_tracks(plan,path,&header,&headers)?;drop(headers);
        for a in audios.iter_mut().flatten(){a.track=mapping[a.id.index()].ok_or_else(||RecordingError::invalid("native recording track mapping missing"))?;}
        Ok(Self{mux:Some(mux),config:config.clone(),multitrack:!plan.legacy_names(),video,vc,header,video_track,audios,pending_video:None,video_first:None,video_end:0,finishing:false,completed:false,finalization_error:None,packet_limit:limits.max_records,_permit:permit})
    }
    fn mux(&mut self)->Result<&mut PacketMux,RecordingError>{self.mux.as_mut().ok_or_else(||RecordingError::invalid("recording packet mux is closed"))}
    fn is_mkv(&self)->bool{self.mux.as_ref().is_some_and(PacketMux::is_mkv)}
    fn with_audio<T>(&mut self,id:AudioTrackId,f:impl FnOnce(&mut Self,&mut AudioState)->Result<T,RecordingError>)->Result<T,RecordingError>{
        let mut audio=self.audios[id.index()].take().ok_or_else(||RecordingError::invalid("recording PCM targets an unselected audio track"))?;
        let result=f(self,&mut audio);self.audios[id.index()]=Some(audio);result
    }
    fn sample(&mut self,track:usize,bytes:&[u8],pts:i64,duration:i64,key:bool,padding_ns:u64)->Result<(),RecordingError>{self.mux()?.packet(track,bytes,pts,duration,key,padding_ns)}
    fn video_units(&mut self,units:Vec<avc::AccessUnit>)->Result<(),RecordingError>{
        if self.video.configuration().is_none_or(|h|h.avcc()!=self.header.avcc()){return Err(RecordingError::invalid("native recording SPS/PPS changed during a track"));}
        for unit in units{
            if unit.generation()!=self.vc.generation(){return Err(RecordingError::invalid("recording AVC epoch mismatch"));}
            if self.video_first.is_none(){if !unit.keyframe(){return Err(RecordingError::invalid("first recording AVC AU is not IDR"));}self.video_first=Some(unit.pts());let track=self.video_track;if let Some(mp4)=self.mux()?.mp4(){mp4.set_recovery_origin(track,unit.pts()).map_err(|e|err("checkpoint native AVC origin",e))?;}}
            if let Some(previous)=self.pending_video.take(){let end=previous.pts().checked_add(previous.duration()).ok_or_else(||RecordingError::invalid("recording AVC interval overflow"))?;if unit.pts()<end{return Err(RecordingError::invalid("recording AVC timeline overlaps"));}self.sample(self.video_track,previous.bytes(),previous.pts(),unit.pts()-previous.pts(),previous.keyframe(),0)?;}
            self.video_end=unit.pts().checked_add(unit.duration()).ok_or_else(||RecordingError::invalid("recording AVC end overflow"))?;self.pending_video=Some(unit);
        }Ok(())
    }
    fn audio_units(&mut self,a:&mut AudioState,units:Vec<aac::AccessUnit>)->Result<(),RecordingError>{
        for unit in units{
            if unit.generation()!=a.config.generation(){return Err(RecordingError::invalid("recording AAC track/epoch mismatch"));}
            let anchor=a.anchor()?;let actual=unit.pts().checked_add(anchor).ok_or_else(||RecordingError::invalid("recording AAC packet anchor overflow"))?;
            if self.is_mkv()&&self.finishing&&actual>=a.valid_end{continue;}
            if a.first.is_none(){a.first=Some(unit.pts());self.mux()?.audio_origin(a.track,unit.pts(),actual)?;}
            if let Some(previous)=a.pending.take(){if unit.pts()<=previous.pts(){return Err(RecordingError::invalid("recording AAC track native timestamp order"));}let pts=previous.pts().checked_add(anchor).ok_or_else(||RecordingError::invalid("recording AAC anchor overflow"))?;self.sample(a.track,previous.bytes(),pts,unit.pts()-previous.pts(),true,0)?;}
            a.pending=Some(unit);
        }Ok(())
    }
    fn encode_video(&mut self,bytes:&[u8],index:u64)->Result<(),RecordingError>{
        if self.finishing{return Err(RecordingError::invalid("recording video after finalize"));}
        let(pts,duration)=self.vc.frame_timing(index).map_err(|e|err("recording AVC frame time",e))?;
        let input=avc::Nv12Input::new(&self.vc,pts,duration,bytes.to_vec().into_boxed_slice()).map_err(|e|err("recording NV12 admission",e))?;
        let units=self.video.encode(input).map_err(|e|err("recording H264 encode",e))?;self.video_units(units)
    }
    fn feed_audio(&mut self,a:&mut AudioState,samples:&[i16],first_frame:u64)->Result<(),RecordingError>{
        let channels=usize::from(a.config.channels());if samples.is_empty()||samples.len()%channels!=0||samples.len()/channels>4096{return Err(RecordingError::invalid("recording per-track PCM shape"));}
        if a.origin.is_none()&&self.audios.iter().flatten().filter_map(|peer|peer.origin).any(|origin|origin!=first_frame){return Err(RecordingError::invalid("selected recording tracks must begin on one real PCM grid"));}
        let origin=*a.origin.get_or_insert(first_frame);let relative=first_frame.checked_sub(origin).ok_or_else(||RecordingError::invalid("recording PCM precedes its track origin"))?;
        if relative<a.next||relative-a.next>u64::from(a.config.sample_rate()/10){return Err(RecordingError::invalid("recording PCM gap/overlap exceeds100ms admission"));}
        let next=relative.checked_add((samples.len()/channels) as u64).ok_or_else(||RecordingError::invalid("recording PCM grid overflow"))?;
        let end=origin.checked_add(next).ok_or_else(||RecordingError::invalid("recording PCM end overflow"))?;
        if end>u64::from(a.config.sample_rate())*86400{return Err(RecordingError::invalid("recording PCM exceeds24hour profile"));}
        // Gaps are explicit holes on this SAME logical mix grid, never another
        // mix substituted for a missing selected track.
        while a.next<relative{let frames=(relative-a.next).min(1024) as usize;let input=aac::PcmInput::new(&a.config,a.next,vec![0;frames*channels].into_boxed_slice()).map_err(|e|err("recording explicit mix gap",e))?;let units=a.encoder.encode(input).map_err(|e|err("recording mix gap AAC",e))?;a.next+=frames as u64;self.audio_units(a,units)?;}
        for chunk in samples.chunks(1024*channels){let input=aac::PcmInput::new(&a.config,a.next,chunk.to_vec().into_boxed_slice()).map_err(|e|err("recording per-track AAC input",e))?;let units=a.encoder.encode(input).map_err(|e|err("recording per-track AAC encode",e))?;a.next=a.next.checked_add((chunk.len()/channels) as u64).ok_or_else(||RecordingError::invalid("recording AAC sample position overflow"))?;self.audio_units(a,units)?;}
        a.valid_end=i64::try_from(u128::from(end)*10_000_000/u128::from(a.config.sample_rate())).map_err(|_|RecordingError::invalid("recording AAC presentation end overflow"))?;
        a.valid_end_ns=u64::try_from(u128::from(end)*1_000_000_000/u128::from(a.config.sample_rate())).map_err(|_|RecordingError::invalid("recording AAC sample cutoff overflow"))?;Ok(())
    }
    fn edits(first:i64,duration:i64)->Result<Vec<Edit>,RecordingError>{
        if duration<=0{return Err(RecordingError::invalid("recording has no positive presentation interval"));}
        let edit=|duration:u64,media_time:i64|Edit{segment_duration:duration,media_time,media_rate:0x10000};
        if first>0{let delay=first.min(duration);let mut v=vec![edit(delay as u64,-1)];if duration>delay{v.push(edit((duration-delay) as u64,0));}Ok(v)}else{Ok(vec![edit(duration as u64,first.checked_neg().ok_or_else(||RecordingError::invalid("recording priming offset overflow"))?)])}
    }
    // Movie ns preserve per-track real PCM cut; media time may use native100ns
    // (video) or ns (multi AAC). Only observed native priming is removed.
    fn edits_ns(first:i64,end:u64,media_scale:u32)->Result<Vec<Edit>,RecordingError>{
        if end==0{return Err(RecordingError::invalid("recording track has no presentation interval"));}
        let edit=|segment_duration,media_time|Edit{segment_duration,media_time,media_rate:0x10000};
        if first>0{let delay=(first as u64).min(end);let mut v=vec![edit(delay,-1)];if end>delay{v.push(edit(end-delay,0));}Ok(v)}else{let skip=first.checked_neg().ok_or_else(||RecordingError::invalid("recording AAC priming overflow"))?;let ticks=i64::try_from(i128::from(skip)*i128::from(media_scale)/1_000_000_000).map_err(|_|RecordingError::invalid("recording track edit overflow"))?;Ok(vec![edit(end,ticks)])}
    }
    fn finish_inner(&mut self)->Result<(),RecordingError>{
        let mut accepted_end=None;for a in self.audios.iter().flatten(){if a.origin.is_none(){return Err(RecordingError::invalid("selected recording track received no actual PCM"));}if accepted_end.is_some_and(|end|end!=a.valid_end_ns){return Err(RecordingError::invalid("selected recording tracks have different final PCM cutoffs; incomplete bundle cannot be published"));}accepted_end=Some(a.valid_end_ns);}
        let units=self.video.drain().map_err(|e|err("drain recording H264",e))?;self.video_units(units)?;
        for index in 0..6{if self.audios[index].is_some(){let id=AudioTrackId::new(index as u8+1).map_err(|e|err("recording track identity",e))?;self.with_audio(id,|this,a|{
            if a.origin.is_none(){return Err(RecordingError::invalid("selected recording track received no actual PCM"));}
            let units=a.encoder.drain().map_err(|e|err("drain independent recording AAC",e))?;this.audio_units(a,units)?;
            let covered=a.encoder.stats().last_native_end.and_then(|end|a.anchor().ok().and_then(|anchor|end.checked_add(anchor))).ok_or_else(||RecordingError::invalid("native AAC drain reported no presentation coverage"))?;
            if covered.saturating_add(1)<a.valid_end{return Err(RecordingError::invalid("native AAC drain ended before this track's accepted PCM cutoff"));}Ok(())
        })?;}}
        let first=self.video_first.ok_or_else(||RecordingError::invalid("recording has no native AVC AU"))?;
        let audio_end=self.audios.iter().flatten().map(|a|a.valid_end).max().unwrap_or(0);let mut duration=self.video_end.max(audio_end);
        if self.multitrack{let ns=self.audios.iter().flatten().map(|a|a.valid_end_ns).max().unwrap_or(0);duration=duration.max(i64::try_from(ns.div_ceil(100)).map_err(|_|RecordingError::invalid("recording common presentation end overflow"))?);}
        if let Some(unit)=self.pending_video.take(){let end=duration.max(unit.pts().checked_add(unit.duration()).ok_or_else(||RecordingError::invalid("recording final AVC interval overflow"))?);self.sample(self.video_track,unit.bytes(),unit.pts(),end-unit.pts(),unit.keyframe(),0)?;}let video_track=self.video_track;self.mux()?.finish_track(video_track)?;
        let mut audio_edits=Vec::with_capacity(6);
        for index in 0..6{if self.audios[index].is_some(){let id=AudioTrackId::new(index as u8+1).map_err(|e|err("recording track identity",e))?;self.with_audio(id,|this,a|{
            let native_first=a.first.ok_or_else(||RecordingError::invalid("selected recording AAC track has no actual AU"))?;let anchor=a.anchor()?;
            if let Some(unit)=a.pending.take(){let pts=unit.pts().checked_add(anchor).ok_or_else(||RecordingError::invalid("recording final AAC anchor overflow"))?;let end=pts.checked_add(unit.duration()).ok_or_else(||RecordingError::invalid("recording final AAC interval overflow"))?;
                let coded_ns=u64::try_from((1024u128*1_000_000_000).div_ceil(u128::from(a.config.sample_rate()))).map_err(|_|RecordingError::invalid("recording coded AAC span overflow"))?;let coded_end=i128::from(pts)*100+i128::from(coded_ns);let valid=i128::from(a.valid_end_ns);
                if coded_end<valid&&end.saturating_add(1)<a.valid_end{return Err(RecordingError::invalid("recording retained AAC ends before real PCM cutoff"));}
                let padding=if this.is_mkv(){u64::try_from((coded_end-valid).max(0)).map_err(|_|RecordingError::invalid("recording AAC DiscardPadding overflow"))?}else{0};
                let span=if this.multitrack{Some(coded_ns)}else{None};this.mux()?.packet_final_span(a.track,unit.bytes(),pts,unit.duration(),true,padding,span)?;
            }
            this.mux()?.finish_track(a.track)?;
            let first=anchor.checked_add(native_first).ok_or_else(||RecordingError::invalid("recording actual AAC origin overflow"))?;
            let edits=if this.multitrack{Self::edits_ns(first.checked_mul(100).ok_or_else(||RecordingError::invalid("recording AAC ns origin overflow"))?,a.valid_end_ns,1_000_000_000)?}else{Self::edits(first,duration)?};audio_edits.push((a.track,edits));Ok(())
        })?;}}
        if !self.is_mkv(){let edits=if self.multitrack{let first=first.checked_mul(100).ok_or_else(||RecordingError::invalid("recording AVC ns origin overflow"))?;let end=u64::try_from(duration).ok().and_then(|v|v.checked_mul(100)).ok_or_else(||RecordingError::invalid("recording common MP4 duration overflow"))?;Self::edits_ns(first,end,10_000_000)?}else{Self::edits(first,duration)?};let video_track=self.video_track;
            let mux=self.mux()?.mp4().ok_or_else(||RecordingError::invalid("recording MP4 container missing"))?;mux.set_track_edits(video_track,edits).map_err(|e|err("recording AVC presentation edits",e))?;for(track,edits)in audio_edits{mux.set_track_edits(track,edits).map_err(|e|err("recording independent AAC presentation edits",e))?;}
        }
        self.mux.take().ok_or_else(||RecordingError::invalid("recording container finalized twice"))?.finish(duration)
    }
}
impl RecordingWriter for Writer{
    fn video(&mut self,frame:&RgbaFrame,index:u64)->Result<(),RecordingError>{if(frame.width(),frame.height())!=(self.vc.width(),self.vc.height()){return Err(RecordingError::invalid("recording video dimensions changed"));}let bytes=recording_nv12(frame,32*1024*1024,18_000_000).map_err(|e|err("recording NV12 prepare",e))?;self.encode_video(&bytes,index)}
    fn video_prepared(&mut self,frame:&PreparedVideo,index:u64)->Result<(),RecordingError>{if(frame.image().width(),frame.image().height())!=(self.vc.width(),self.vc.height()){return Err(RecordingError::invalid("recording prepared video dimensions changed"));}self.encode_video(frame.nv12(),index)}
    fn audio(&mut self,samples:&[i16],first_frame:u64)->Result<(),RecordingError>{self.audio_track(AudioTrackId::ONE,samples,first_frame)}
    fn audio_track(&mut self,id:AudioTrackId,samples:&[i16],first_frame:u64)->Result<(),RecordingError>{if self.finishing{return Err(RecordingError::invalid("recording audio after finalize"));}self.with_audio(id,|this,a|this.feed_audio(a,samples,first_frame))}
    fn container_packet_limit(&self)->u64{self.packet_limit}
    fn finalize(&mut self)->Result<(),RecordingError>{if self.completed{return Ok(());}if let Some(error)=&self.finalization_error{return Err(error.clone());}self.finishing=true;match self.finish_inner(){Ok(())=>{self.completed=true;Ok(())},Err(error)=>{self.finalization_error=Some(error.clone());Err(error)}}}
}
