//! Recording worker filesystem ownership for the container's seekable spool.
//! Files are private siblings of OwnedFile's exclusively created temporary MP4.
//! No GUI IO, background helper, source read or native codec lives here.
use super::{RecordingConfig,RecordingError};
use skylive_mp4::{SpoolLimits,SpoolReceipt};
use std::{fs::{self,File,OpenOptions},io::Write,path::{Path,PathBuf}};
const DURATION:u64=24*60*60;
const FILE_BYTES:u64=64*1024*1024*1024;
const NOTE_BYTES:usize=512*1024;
pub(super) fn limits(config:&RecordingConfig)->Result<SpoolLimits,RecordingError>{
    let video=(u128::from(DURATION)*u128::from(config.video.fps_num())).div_ceil(u128::from(config.video.fps_den()));
    let audio=config.audio.map_or(0,|a|(u128::from(DURATION)*u128::from(a.sample_rate)).div_ceil(1024)+2);
    // Include endpoint rounding and bounded native AVC/AAC drain bookkeeping.
    // No raw frames or PCM are retained by the sample journal.
    let records=u64::try_from(video+audio+8).map_err(|_|RecordingError::invalid("recording metadata count overflow"))?;
    Ok(SpoolLimits {max_records:records,max_spool_bytes:SpoolLimits::required_spool_bytes(records).map_err(|e|RecordingError::native("recording metadata spool size",e))?,max_output_bytes:FILE_BYTES,max_duration_seconds:DURATION as u32,max_moov_bytes:512*1024*1024,max_scan_records:records.checked_mul(10).ok_or_else(||RecordingError::invalid("recording metadata scan count overflow"))?})
}
pub(super) struct MetadataFiles {
    media:PathBuf,spool:PathBuf,note:PathBuf,destination:PathBuf,description:String,
}
impl MetadataFiles {
    pub fn create(media:&Path,config:&RecordingConfig,limits:SpoolLimits)->Result<(Self,File),RecordingError>{
        let spool=media.with_extension("samples.spool");let note=media.with_extension("recovery.txt");
        let audio=config.audio.map_or_else(||"disabled".into(),|a|format!("AAC-LC {}Hz {}channels {}kbit/s",a.sample_rate,a.channels,a.bitrate_kbps));
        let files=Self {media:media.to_owned(),spool,note,destination:config.path.clone(),description:format!("Video: H264 {:?}, {}x{}, {}/{}FPS, {}kbit/s, no B pictures\nAudio: {audio}\nLimits: {}seconds, {}final MP4 bytes, {}sample records, {}spool bytes, {}streamed moov bytes, {}scanned records\n",config.encoder,config.video.width(),config.video.height(),config.video.fps_num(),config.video.fps_den(),config.video_bitrate_kbps,limits.max_duration_seconds,limits.max_output_bytes,limits.max_records,limits.max_spool_bytes,limits.max_moov_bytes,limits.max_scan_records)};
        let spool=OpenOptions::new().create_new(true).read(true).write(true).open(&files.spool).map_err(|e|RecordingError::io("create private recording sample journal",&files.spool,e))?;
        files.note("IN PROGRESS — the requested final destination has not been published.\n",None,true)?;
        Ok((files,spool))
    }
    pub fn annotate_tracks(&mut self,description:&str)->Result<(),RecordingError>{if description.len()>4096{return Err(RecordingError::invalid("recording track recovery description exceeds4KiB"));}self.description.push_str("\nIndependent audio tracks:\n");self.description.push_str(description);self.description.push('\n');self.note("IN PROGRESS — independent track journal; final destination unpublished.\n",None,false)}
    fn note(&self,state:&str,receipt:Option<SpoolReceipt>,create:bool)->Result<(),RecordingError>{
        let mut text=format!("SkyLive packet recording recovery\n{state}\nTemporary media: {}\nSample journal: {}\nRequested destination: {}\n{}\nThe journal is NOT a playable MP4. Keep it with the private media if recording fails.\nSKYMP4S1: version1, 64-byte little-endian limits/checksum header; two alternating128KiB recovery descriptor slots; then32-byte checksum-protected sample records.\nThe latest valid descriptor generation contains actual AVC/AAC sample entries, observed native timestamp origins and last checkpointed presentation edits. Descriptor counts may lag capture; complete valid journal records are the committed sample prefix. A partial/orphan record or a failed native drain is not successful finalization.\nNative100ns DTS/PTS intervals and AAC priming are preserved; no repair or playback success is claimed by this note.\nSee crates/mp4/src/isobmff/mux/spooled.rs for the versioned record layout.\n",self.media.display(),self.spool.display(),self.destination.display(),self.description);
        if let Some(r)=receipt{text.push_str(&format!("\nFinalized private MP4: {}bytes\nRecords: {}\nSpool: {}bytes\nMoov: offset{}, {}bytes, streamed without whole-moov allocation\nScanned records: {}\nThe complete private MP4 can be recovered independently of this journal. Publication still uses the existing no-overwrite host publication path.\n",r.output_bytes,r.records,r.spool_bytes,r.moov_start,r.moov_bytes,r.scanned_records));}
        if text.len()>NOTE_BYTES{return Err(RecordingError::invalid("recording recovery note exceeds512KiB"));}
        let mut options=OpenOptions::new();options.write(true);if create{options.create_new(true);}else{options.truncate(true);}
        let mut file=options.open(&self.note).map_err(|e|RecordingError::io("open private recording recovery note",&self.note,e))?;
        file.write_all(text.as_bytes()).and_then(|_|file.sync_all()).map_err(|e|RecordingError::io("flush private recording recovery note",&self.note,e))
    }
    pub fn completed(&self,receipt:SpoolReceipt)->Result<(),RecordingError>{self.note("COMPLETE PRIVATE MP4 — not yet published; metadata cleanup precedes publication.\n",Some(receipt),false)}
    /// Both native/container handles must be closed before this call (Windows).
    /// A cleanup error retains a complete private MP4 and reports the exact path.
    pub fn cleanup(&self)->Result<(),RecordingError>{
        fs::remove_file(&self.spool).map_err(|e|RecordingError::io("remove finalized private sample journal",&self.spool,e))?;
        fs::remove_file(&self.note).map_err(|e|RecordingError::io("remove finalized private recovery note",&self.note,e))?;Ok(())
    }
}
// No Drop deletion: failed/incomplete media and its metadata are recovery data.
// Successful cleanup is explicit on the recording owner before publication.
