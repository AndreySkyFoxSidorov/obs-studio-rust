//! Typed logical track mapping over actual packet containers, no mix cloning.
use super::{RecordingConfig,RecordingError,MultitrackRecordingConfig,metadata_spool};
use crate::{native_h264,native_aac};
use skylive_core::{RecordingContainer,AudioTrackId};
use skylive_mp4::{AvcConfig,Brand,ColorInfo,SampleEntry,TrackConfig,SpoolMp4Writer,WriterOptions,WriteSample,matroska};
use std::{fs::{self,File,OpenOptions},io::Write,path::{Path,PathBuf}};
fn err(op:&str,e:impl std::fmt::Display)->RecordingError{RecordingError::native(op,e)}
pub(super) struct AudioHeader<'a>{pub id:AudioTrackId,pub name:&'a str,pub config:&'a native_aac::CodecConfiguration}
pub(super) enum PacketMux{
    Mp4{mux:SpoolMp4Writer<File,File>,metadata:metadata_spool::MetadataFiles,audio_ns:bool},
    Matroska{mux:matroska::Writer<File,File>,metadata:MkvFiles,tracks:[Option<matroska::Track>;7]},
}
impl PacketMux{
    pub fn new_tracks(plan:&MultitrackRecordingConfig,path:&Path,video:&native_h264::CodecConfiguration,audio:&[AudioHeader<'_>])->Result<(Self,usize,[Option<usize>;6]),RecordingError>{
        let config=plan.base();let multi=!plan.legacy_names();if audio.len()!=plan.tracks().len()||audio.len()>6{return Err(RecordingError::invalid("native AAC header count differs from captured recording tracks"));}
        for(h,t)in audio.iter().zip(plan.tracks()){if h.id!=t.id||h.name.len()>256||h.config.sample_rate()!=t.audio.sample_rate||h.config.channels()!=u16::from(t.audio.channels){return Err(RecordingError::invalid("native AAC header/identity differs from captured track"));}}
        let limits=plan.spool_limits()?;let file=OpenOptions::new().create_new(true).write(true).read(true).open(path).map_err(|e|RecordingError::io("create private packet recording",path,e))?;let mut mapping=[None;6];
        let tracks_note=audio.iter().zip(plan.tracks()).map(|(h,t)|format!("Track{} name={:?}: AAC-LC {}Hz {}channels {}kbit/s",h.id.get(),h.name,h.config.sample_rate(),h.config.channels(),t.audio.bitrate_kbps)).collect::<Vec<_>>().join("\n");
        match config.container()?{
            RecordingContainer::Mp4=>{
                let mut options=WriterOptions::new(Brand::Mp4);options.movie_timescale=if multi{1_000_000_000}else{10_000_000};let(mut metadata,spool)=metadata_spool::MetadataFiles::create(path,config,limits)?;if multi{metadata.annotate_tracks(&tracks_note)?;}
                let mut mux=if multi{SpoolMp4Writer::new_multitrack(file,options,spool,limits)}else{SpoolMp4Writer::new(file,options,spool,limits)}.map_err(|e|err("initialize recording MP4 sample journal",e))?;
                let mut entry=SampleEntry::avc(AvcConfig::parse(video.avcc()).map_err(|e|err("native recording avcC",e))?,config.video.width() as u16,config.video.height() as u16);if let Some(v)=&mut entry.video{v.color=Some(ColorInfo::Nclx{primaries:1,transfer:1,matrix:1,full_range:false});}let v=mux.add_track(TrackConfig::new(entry,10_000_000)).map_err(|e|err("recording AVC track",e))?;
                for h in audio{let mut cfg=TrackConfig::new(SampleEntry::aac(h.config.asc().to_vec(),u32::from(h.config.channels()),h.config.sample_rate()),if multi{1_000_000_000}else{10_000_000});if multi{cfg.handler_name=Some(h.name.into());}let index=mux.add_track(cfg).map_err(|e|err("recording AAC track",e))?;mapping[h.id.index()]=Some(index);}
                Ok((Self::Mp4{mux,metadata,audio_ns:multi},v,mapping))
            },
            RecordingContainer::Matroska=>{
                let video_packets=u64::try_from((86400u128*u128::from(config.video.fps_num())).div_ceil(u128::from(config.video.fps_den()))+8).map_err(|_|RecordingError::invalid("Matroska video count overflow"))?;
                let format=matroska::Config{avcc:video.avcc().to_vec(),width:config.video.width(),height:config.video.height(),audio:if multi{None}else{audio.first().map(|h|matroska::Audio{asc:h.config.asc().to_vec(),rate:h.config.sample_rate(),channels:h.config.channels()})}};
                let bounds=matroska::Limits{packets:limits.max_records,video_packets,output_bytes:limits.max_output_bytes,duration_100ns:86400*10_000_000};let(mut metadata,cues)=MkvFiles::create(path,config,bounds)?;if multi{metadata.description=metadata.description.replace("576packets","1024packets");metadata.description.push_str(&format!("\n{tracks_note}\n"));metadata.note("IN PROGRESS: independent recording tracks; final destination unpublished.",None,false)?;}
                let mut tracks=[None;7];tracks[0]=Some(matroska::Track::Video);let named=audio.iter().enumerate().map(|(ordinal,h)|{let id=matroska::AudioId::new(h.id.get()).map_err(|e|err("Matroska logical track identity",e))?;let index=ordinal+1;mapping[h.id.index()]=Some(index);tracks[index]=Some(if multi{matroska::Track::AudioIndex(id)}else{matroska::Track::Audio});Ok(matroska::NamedAudio{id,name:if multi{h.name.into()}else{String::new()},format:matroska::Audio{asc:h.config.asc().to_vec(),rate:h.config.sample_rate(),channels:h.config.channels()}})}).collect::<Result<Vec<_>,RecordingError>>()?;
                let mux=if multi{matroska::Writer::new_multitrack(file,cues,format,bounds,named)}else{matroska::Writer::new(file,cues,format,bounds)}.map_err(|e|err("initialize native-packet Matroska recording",e))?;Ok((Self::Matroska{mux,metadata,tracks},0,mapping))
            }
        }
    }
    pub fn is_mkv(&self)->bool{matches!(self,Self::Matroska{..})}
    pub fn mp4(&mut self)->Option<&mut SpoolMp4Writer<File,File>>{match self{Self::Mp4{mux,..}=>Some(mux),_=>None}}
    pub fn audio_origin(&mut self,track:usize,native_first:i64,absolute_first:i64)->Result<(),RecordingError>{match self{
        Self::Mp4{mux,audio_ns,..}=>{let origin=if *audio_ns{absolute_first.checked_mul(100).ok_or_else(||RecordingError::invalid("AAC recovery origin overflow"))?}else{absolute_first};mux.set_recovery_origin(track,origin).map_err(|e|err("checkpoint native AAC origin",e))},
        Self::Matroska{mux,tracks,..}=>{let id=match tracks.get(track).copied().flatten(){Some(matroska::Track::Audio)=>matroska::AudioId::new(1),Some(matroska::Track::AudioIndex(id))=>Ok(id),_=>return Err(RecordingError::invalid("Matroska audio origin targets another track"))}.map_err(|e|err("Matroska audio identity",e))?;mux.audio_delay_for(id,native_first).map_err(|e|err("write actual Matroska AAC CodecDelay",e))}
    }}
    pub fn packet(&mut self,track:usize,bytes:&[u8],pts:i64,duration:i64,key:bool,padding_ns:u64)->Result<(),RecordingError>{self.packet_final_span(track,bytes,pts,duration,key,padding_ns,None)}
    pub fn packet_final_span(&mut self,track:usize,bytes:&[u8],pts:i64,duration:i64,key:bool,padding_ns:u64,final_coded_ns:Option<u64>)->Result<(),RecordingError>{match self{
        Self::Mp4{mux,audio_ns,..}=>{let ticks=u64::try_from(duration).ok().filter(|n|*n>0).ok_or_else(||RecordingError::invalid("recording MP4 packet interval"))?;let value=if *audio_ns&&track!=0{ticks.checked_mul(100).ok_or_else(||RecordingError::invalid("AAC nanosecond interval overflow"))?.max(final_coded_ns.unwrap_or(0))}else{ticks};let duration=u32::try_from(value).map_err(|_|RecordingError::invalid("recording MP4 packet interval exceeds its timescale"))?;mux.write_sample(track,WriteSample{data:bytes,duration,composition_offset:0,is_sync:key}).map_err(|e|err("write recording encoded MP4 sample",e))},
        Self::Matroska{mux,tracks,..}=>{let track=tracks.get(track).copied().flatten().ok_or_else(||RecordingError::invalid("Matroska recording track identity"))?;mux.packet(matroska::Packet{track,bytes,pts,duration,key,discard_padding_ns:padding_ns}).map_err(|e|err("write recording encoded Matroska packet",e))}
    }}
    pub fn finish_track(&mut self,track:usize)->Result<(),RecordingError>{if let Self::Matroska{mux,tracks,..}=self{let track=tracks.get(track).copied().flatten().ok_or_else(||RecordingError::invalid("Matroska recording EOF identity"))?;mux.finish_track(track).map_err(|e|err("Matroska track EOF",e))?;}Ok(())}
    pub fn finish(self,duration:i64)->Result<(),RecordingError>{match self{
        Self::Mp4{mux,metadata,..}=>{let(mut file,mut spool,receipt)=mux.finish().map_err(|e|err("stream recording MP4 sample tables",e))?;file.flush().and_then(|_|file.sync_all()).map_err(|e|err("flush recording MP4",e))?;spool.flush().and_then(|_|spool.sync_all()).map_err(|e|err("flush recording sample journal",e))?;drop(spool);drop(file);metadata.completed(receipt)?;metadata.cleanup()},
        Self::Matroska{mux,metadata,..}=>{let(mut file,mut cues,receipt)=mux.finish(duration).map_err(|e|err("finish recording Matroska clusters/cues",e))?;file.flush().and_then(|_|file.sync_all()).map_err(|e|err("flush recording Matroska",e))?;cues.flush().and_then(|_|cues.sync_all()).map_err(|e|err("flush recording cue journal",e))?;drop(cues);drop(file);metadata.completed(receipt)?;metadata.cleanup()}
    }}
}
pub(super) struct MkvFiles{media:PathBuf,cues:PathBuf,note:PathBuf,destination:PathBuf,description:String}
impl MkvFiles {
    fn create(media:&Path,config:&RecordingConfig,limits:matroska::Limits)->Result<(Self,File),RecordingError>{
        let files=Self{media:media.to_owned(),cues:media.with_extension("cues.spool"),note:media.with_extension("recovery.txt"),destination:config.path.clone(),description:format!("Matroska v4; AVC {:?} {}x{} {}/{}FPS; AAC {:?}; native100ns ticks;64MiB encoded interleave,576packets; maximum{}packets,{}video/cue records,{}output bytes.\n",config.encoder,config.video.width(),config.video.height(),config.video.fps_num(),config.video.fps_den(),config.audio,limits.packets,limits.video_packets,limits.output_bytes)};
        let file=OpenOptions::new().create_new(true).write(true).read(true).open(&files.cues).map_err(|e|RecordingError::io("create private Matroska cue journal",&files.cues,e))?;
        files.note("IN PROGRESS: final destination is unpublished. Completed clusters remain in the private MKV; an interrupted tail may be incomplete. No repair/playback acceptance is claimed.",None,true)?;Ok((files,file))
    }
    fn note(&self,state:&str,receipt:Option<matroska::Receipt>,create:bool)->Result<(),RecordingError>{
        let mut text=format!("SkyLive Matroska recording recovery\n{state}\nPrivate MKV: {}\nCue journal: {}\nRequested destination: {}\n{}\nThe MKV contains actual AVC avcC, AAC ASC, SDR709 video colour and complete known-size clusters. A live Segment/Duration may not yet be finalized. The cue journal is a private index, not a media file. Keep both files if finalization fails.\nAAC CodecDelay uses only the observed negative native codec origin; Program origin is retained separately in block timestamps. Last-block DiscardPadding trims actual coded tail. No automatic recovery/remux or playback is promised.\n",self.media.display(),self.cues.display(),self.destination.display(),self.description);
        if let Some(r)=receipt{text.push_str(&format!("COMPLETE PRIVATE MKV: {}bytes; {}packets; {}cues; {}cue journal bytes; duration{}native ticks. Metadata cleanup precedes no-overwrite publication.\n",r.bytes,r.packets,r.cues,r.cue_bytes,r.duration_100ns));}
        if text.len()>512*1024{return Err(RecordingError::invalid("Matroska recovery note exceeds512KiB"));}let mut options=OpenOptions::new();options.write(true);if create{options.create_new(true);}else{options.truncate(true);}let mut file=options.open(&self.note).map_err(|e|RecordingError::io("open private Matroska recovery note",&self.note,e))?;file.write_all(text.as_bytes()).and_then(|_|file.sync_all()).map_err(|e|RecordingError::io("flush private Matroska recovery note",&self.note,e))
    }
    fn completed(&self,receipt:matroska::Receipt)->Result<(),RecordingError>{self.note("COMPLETE PRIVATE MKV — awaiting publication.",Some(receipt),false)}
    fn cleanup(&self)->Result<(),RecordingError>{fs::remove_file(&self.cues).map_err(|e|RecordingError::io("remove finalized private Matroska cue journal",&self.cues,e))?;fs::remove_file(&self.note).map_err(|e|RecordingError::io("remove finalized private Matroska recovery note",&self.note,e))?;Ok(())}
}
// Deliberately no Drop deletion: failed recordings are recovery data.
