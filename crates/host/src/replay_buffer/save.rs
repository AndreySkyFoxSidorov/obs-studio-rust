//! One explicit MP4 snapshot. Encoded bytes are borrowed from pinned ring AUs;
//! no raw frames, transcode, continuous disk recording or overwrite publication.
use super::{ring::Clip,*};
use skylive_mp4::{AvcConfig,Brand,ColorInfo,Edit,Mp4Writer,SampleEntry,TrackConfig,WriteSample,WriterOptions};
use std::{fs::{self,OpenOptions},io::Write,path::Path};
static NEXT_TEMP:AtomicU64=AtomicU64::new(1);
fn io(operation:&str,path:&Path,e:impl std::fmt::Display)->ReplayError{ReplayError::Detail(format!("{operation} {}: {e}",path.display()))}
fn cancelled(cancel:&AtomicBool)->Result<(),ReplayError>{if cancel.load(Ordering::Acquire){Err(ReplayError::Cancelled)}else{Ok(())}}
pub(super) fn write(clip:Clip,path:PathBuf,cancel:Arc<AtomicBool>)->Result<ReplayReceipt,ReplayError>{
    cancelled(&cancel)?;
    if path.as_os_str().is_empty()||path.as_os_str().len()>32768||!path.extension().is_some_and(|s|s.eq_ignore_ascii_case("mp4")){return Err(ReplayError::Invalid("replay destination requires an MP4 path"));}
    let parent=path.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(Path::new("."));fs::create_dir_all(parent).map_err(|e|io("create replay directory",parent,e))?;
    let mut name=path.file_name().ok_or(ReplayError::Invalid("replay destination filename"))?.to_os_string();let serial=NEXT_TEMP.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|ReplayError::Invalid("replay temporary identity exhausted"))?;
    name.push(format!(".replay-{}-{serial}.part",std::process::id()));let temporary=parent.join(name);
    let mut created=false;
    let result=(||{
        let file=OpenOptions::new().create_new(true).write(true).read(true).open(&temporary).map_err(|e|io("create replay temporary MP4",&temporary,e))?;
        created=true;
        let mut options=WriterOptions::new(Brand::Mp4);options.movie_timescale=10_000_000;
        let mut writer=Mp4Writer::new(file,options).map_err(|e|io("initialize replay MP4",&temporary,e))?;
        let avc=AvcConfig::parse(clip.avc.avcc()).map_err(|e|io("read replay AVC header",&temporary,e))?;
        let mut entry=SampleEntry::avc(avc,clip.config.video.width() as u16,clip.config.video.height() as u16);
        if let Some(video)=&mut entry.video {video.color=Some(ColorInfo::Nclx {primaries:1,transfer:1,matrix:1,full_range:false});}
        let duration=u64::try_from(clip.end-clip.start).map_err(|_|ReplayError::Invalid("replay duration"))?;
        let mut track=TrackConfig::new(entry,10_000_000);track.edits.push(Edit {segment_duration:duration,media_time:0,media_rate:0x10000});
        let video=writer.add_track(track).map_err(|e|io("add replay AVC track",&temporary,e))?;
        let audio=if let (Some(configuration),Some(first))=(&clip.aac,clip.audio.first()) {
            let mut track=TrackConfig::new(SampleEntry::aac(configuration.asc().to_vec(),u32::from(configuration.channels()),configuration.sample_rate()),10_000_000);
            if first.pts()>clip.start {let delay=(first.pts()-clip.start) as u64;track.edits.push(Edit {segment_duration:delay,media_time:-1,media_rate:0x10000});track.edits.push(Edit {segment_duration:duration.saturating_sub(delay),media_time:0,media_rate:0x10000});}
            else {track.edits.push(Edit {segment_duration:duration,media_time:clip.start-first.pts(),media_rate:0x10000});}
            Some(writer.add_track(track).map_err(|e|io("add replay AAC track",&temporary,e))?)
        }else{None};
        for (index,packet) in clip.video.iter().enumerate(){cancelled(&cancel)?;
            let end=clip.video.get(index+1).map_or(packet.end(),|next|next.pts()).min(clip.end);
            let delta=u32::try_from(end-packet.pts()).map_err(|_|ReplayError::Invalid("replay video sample duration exceeds MP4 admission"))?;
            if delta==0{return Err(ReplayError::Invalid("zero replay video sample duration"));}
            writer.write_sample(video,WriteSample {data:packet.bytes(),duration:delta,composition_offset:0,is_sync:packet.key()}).map_err(|e|io("write replay AVC sample",&temporary,e))?;
        }
        if let Some(track)=audio {for (index,packet) in clip.audio.iter().enumerate() {cancelled(&cancel)?;let end=clip.audio.get(index+1).map_or(packet.end(),|next|next.pts());let duration=u32::try_from(end-packet.pts()).map_err(|_|ReplayError::Invalid("replay AAC duration"))?;writer.write_sample(track,WriteSample {data:packet.bytes(),duration,composition_offset:0,is_sync:true}).map_err(|e|io("write replay AAC sample",&temporary,e))?;}}
        cancelled(&cancel)?;let mut file=writer.finish().map_err(|e|io("finish replay MP4",&temporary,e))?;
        file.flush().and_then(|_|file.sync_all()).map_err(|e|io("flush replay MP4",&temporary,e))?;
        let bytes=file.metadata().map_err(|e|io("measure replay MP4",&temporary,e))?.len();drop(file);
        cancelled(&cancel)?;
        // The shared publisher refuses replacement. Windows commits a
        // same-volume move; other targets retain the hard-link policy.
        let publication=crate::private_output::publish(&temporary,&path).map_err(|e|io("publish replay without overwrite",&path,e))?;
        if let Err(error)=publication.remove_temporary(&temporary){return Err(ReplayError::AfterPublication {path:path.clone(),detail:format!("Replay MP4 is published; removing temporary {} failed: {error}",temporary.display())});}
        Ok(ReplayReceipt {key_session:clip.key.session,key_id:clip.key.id,path:path.clone(),bytes,duration_100ns:duration,video_samples:clip.video.len(),audio_samples:clip.audio.len()})
    })();
    if created&&result.is_err()&&!matches!(&result,Err(ReplayError::AfterPublication {..})) {if let Err(cleanup)=fs::remove_file(&temporary){if cleanup.kind()!=std::io::ErrorKind::NotFound {return Err(ReplayError::Detail(format!("{}; cleanup of {} failed: {cleanup}",result.as_ref().err().unwrap(),temporary.display())));}}}
    result
}
