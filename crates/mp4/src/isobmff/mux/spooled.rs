//! Disk-backed AVC/AAC sample tables. The caller owns both seekable ports;
//! this module has no paths, filesystem policy, codecs, workers or GUI calls.
//! Memory-mode Mp4Writer/FragmentedWriter remain independent and unchanged.
use super::*;
use std::io::{BufReader,BufWriter};
use crate::isobmff::codec::ColorInfo;

const MAGIC:&[u8;8]=b"SKYMP4S1";
const HEADER:u64=64;
const CHECKPOINT:u64=256*1024;
const CHECKPOINT_SLOT:u64=CHECKPOINT/2;
const RECORD_START:u64=HEADER+CHECKPOINT;
const RECORD_BYTES:u64=32;
const BUFFER:usize=64*1024;
const MAX_TRACKS:usize=7;
const MAX_MULTI_RECORDS:u64=34_668_020;
const MAX_SAMPLE:usize=2*1024*1024;

/// Independent admission for disk, media, scalar counters and final scans.
/// Recording derives max_records from24h*actual FPS and AAC sample rate.
#[derive(Clone,Copy,Debug)]
pub struct SpoolLimits {
    pub max_records:u64,
    pub max_spool_bytes:u64,
    pub max_output_bytes:u64,
    pub max_duration_seconds:u32,
    pub max_moov_bytes:u64,
    pub max_scan_records:u64,
}
impl SpoolLimits {
    pub fn required_spool_bytes(records:u64)->Result<u64>{add(RECORD_START,mul(records,RECORD_BYTES)?)}
    fn validate(self,multitrack:bool)->Result<()> {
        let record_limit=if multitrack{MAX_MULTI_RECORDS}else{20_000_000};let spool_limit=if multitrack{Self::required_spool_bytes(MAX_MULTI_RECORDS)?}else{1024*1024*1024};let moov_limit=if multitrack{1024*1024*1024}else{512*1024*1024};let scan_factor=if multitrack{29}else{12};
        if self.max_records==0||self.max_records>record_limit
            ||self.max_spool_bytes<RECORD_START||self.max_spool_bytes>spool_limit
            ||self.max_output_bytes<1024||self.max_output_bytes>64*1024*1024*1024
            ||self.max_duration_seconds==0||self.max_duration_seconds>86400
            ||self.max_moov_bytes<65536||self.max_moov_bytes>moov_limit
            ||self.max_scan_records<self.max_records||self.max_scan_records>self.max_records*scan_factor {
            return Err(mux("spooled MP4 limits exceed the declared recording profile"));
        }
        if RECORD_START+self.max_records*RECORD_BYTES>self.max_spool_bytes{return Err(mux("sample record capacity exceeds spool disk budget"));}Ok(())
    }
}
/// The sidecar is returned only after its complete checked prefix has produced
/// a flushed moov. The host can close/remove it before publishing the media.
#[derive(Clone,Copy,Debug)]
pub struct SpoolReceipt {pub records:u64,pub spool_bytes:u64,pub moov_start:u64,pub moov_bytes:u64,pub output_bytes:u64,pub scanned_records:u64}
fn mux(s:&str)->Error{Error::Mux(s.into())}
fn add(a:u64,b:u64)->Result<u64>{a.checked_add(b).ok_or_else(||mux("spooled MP4 byte/count overflow"))}
fn mul(a:u64,b:u64)->Result<u64>{a.checked_mul(b).ok_or_else(||mux("spooled MP4 byte/count overflow"))}
fn u32size(v:u64)->Result<u32>{u32::try_from(v).map_err(|_|mux("spooled MP4 box/count exceeds32bits"))}
fn checksum(bytes:&[u8],sequence:u64)->u64 {
    // Accidental truncation/corruption guard, not cryptographic authentication.
    bytes.iter().fold(0xcbf29ce484222325u64^sequence,|s,b|(s^u64::from(*b)).wrapping_mul(0x100000001b3))
}
#[derive(Clone,Copy)]
struct Record {track:u32,size:u32,duration:u32,sync:bool,offset:u64}
impl Record {
    fn encode(self,sequence:u64)->[u8;32]{let mut b=[0u8;32];b[0..4].copy_from_slice(&self.track.to_le_bytes());b[4..8].copy_from_slice(&self.size.to_le_bytes());b[8..12].copy_from_slice(&self.duration.to_le_bytes());b[12..16].copy_from_slice(&u32::from(self.sync).to_le_bytes());b[16..24].copy_from_slice(&self.offset.to_le_bytes());let crc=checksum(&b[..24],sequence);b[24..32].copy_from_slice(&crc.to_le_bytes());b}
    fn decode(b:[u8;32],sequence:u64)->Result<Self>{
        let get=|n|u32::from_le_bytes([b[n],b[n+1],b[n+2],b[n+3]]);
        let crc=u64::from_le_bytes(b[24..32].try_into().map_err(|_|mux("sample log checksum field"))?);
        if crc!=checksum(&b[..24],sequence)||get(12)>1{return Err(mux("sample log checksum/flags mismatch; private recovery files retained"));}
        Ok(Self {track:get(0),size:get(4),duration:get(8),sync:get(12)!=0,offset:u64::from_le_bytes(b[16..24].try_into().map_err(|_|mux("sample log offset field"))?)})
    }
}
#[derive(Clone,Copy,Default,PartialEq,Eq)]
struct Stats {samples:u32,duration:u64,runs:u32,last_duration:u32,sync:u32,first_size:u32,uniform:bool,co64:bool}
impl Stats {
    fn observe(&mut self,r:Record)->Result<()> {
        if self.samples==0 {self.first_size=r.size;self.uniform=true;}
        if self.samples==0||self.last_duration!=r.duration {self.runs=self.runs.checked_add(1).ok_or_else(||mux("stts count overflow"))?;}
        self.samples=self.samples.checked_add(1).ok_or_else(too_many_samples)?;
        self.duration=add(self.duration,u64::from(r.duration))?;self.last_duration=r.duration;
        if r.sync{self.sync=self.sync.checked_add(1).ok_or_else(||mux("stss count overflow"))?;}
        self.uniform&=self.first_size==r.size;self.co64|=r.offset>u64::from(u32::MAX);Ok(())
    }
    fn table_bytes(self)->Result<u64>{
        let mut n=add(16,mul(8,u64::from(self.runs))?)?;
        if self.sync!=self.samples {n=add(n,add(16,mul(4,u64::from(self.sync))?)?)?;}
        n=add(n,if self.samples==0{16}else{28})?; // one AU per chunk
        n=add(n,add(20,if self.uniform{0}else{mul(4,u64::from(self.samples))?})?)?;
        add(n,add(16,mul(if self.co64{8}else{4},u64::from(self.samples))?)?)
    }
}
struct Track {header:WTrack,stats:Stats,prefix_ceiling:u64,recovery_origin:Option<i64>}
struct TrackPlan {prefix:Vec<u8>,stats:Stats}

/// Progressive MP4 with an append-only sample journal and streamed tables.
/// Default <=2tracks; explicit multitrack <=1AVC+6AAC, zero composition offset.
/// There is deliberately no unimplemented MOV/PCM/timecode/faststart option.
pub struct SpoolMp4Writer<W:Read+Write+Seek,S:Read+Write+Seek> {
    w:W,spool:S,opts:WriterOptions,limits:SpoolLimits,max_tracks:usize,tracks:Vec<Track>,
    pad_pos:u64,mdat_pos:u64,media_start:u64,pos:u64,records:u64,scanned:u64,checkpoint_generation:u64,header:[u8;64],poisoned:bool,
}
impl<W:Read+Write+Seek,S:Read+Write+Seek> SpoolMp4Writer<W,S> {
    /// Original/replay-compatible admission: one AVC and one AAC only.
    pub fn new(w:W,opts:WriterOptions,spool:S,limits:SpoolLimits)->Result<Self>{Self::new_profile(w,opts,spool,limits,false)}
    /// Explicit recording profile: one AVC + at most six independent AAC tracks.
    /// The caller must admit the full disk/scan plan before opening native work.
    pub fn new_multitrack(w:W,opts:WriterOptions,spool:S,limits:SpoolLimits)->Result<Self>{Self::new_profile(w,opts,spool,limits,true)}
    fn new_profile(mut w:W,opts:WriterOptions,mut spool:S,limits:SpoolLimits,multitrack:bool)->Result<Self>{
        limits.validate(multitrack)?;
        if opts.brand!=Brand::Mp4||!opts.metadata.is_empty()||opts.metadata.capacity()>16||opts.movie_timescale==0||opts.movie_timescale>1_000_000_000{return Err(mux("spooled recording requires MP4, bounded headers and an explicit movie timescale"));}
        if spool.seek(SeekFrom::End(0))?!=0{return Err(mux("metadata spool port must be newly created and empty"));}
        if w.seek(SeekFrom::End(0))?!=0{return Err(mux("spooled recording media port must be newly created and empty"));}
        let memory=Mp4Writer::new(w,opts)?;
        let mut header=[0u8;64];header[..8].copy_from_slice(MAGIC);header[8..12].copy_from_slice(&1u32.to_le_bytes());header[12..16].copy_from_slice(&(RECORD_BYTES as u32).to_le_bytes());
        for (at,value) in [(16,limits.max_records),(24,limits.max_output_bytes),(32,u64::from(limits.max_duration_seconds)),(40,limits.max_moov_bytes),(48,limits.max_scan_records)]{header[at..at+8].copy_from_slice(&value.to_le_bytes());}
        let crc=checksum(&header[..56],0);header[56..64].copy_from_slice(&crc.to_le_bytes());spool.write_all(&header)?;
        let zero=[0u8;BUFFER];for _ in 0..CHECKPOINT/(BUFFER as u64){spool.write_all(&zero)?;}spool.flush()?;
        Ok(Self {w:memory.w,spool,opts:memory.opts,limits,max_tracks:if multitrack{7}else{2},tracks:Vec::with_capacity(if multitrack{7}else{2}),pad_pos:memory.pad_pos,mdat_pos:memory.mdat_pos,media_start:memory.pos,pos:memory.pos,records:0,scanned:0,checkpoint_generation:0,header,poisoned:false})
    }
    pub fn add_track(&mut self,cfg:TrackConfig)->Result<usize>{
        if self.poisoned||self.records!=0||self.tracks.len()>=self.max_tracks{return Err(mux("spooled MP4 track admission is closed or full"));}
        bounded_track(&cfg)?;
        validate_edits(&cfg.edits,cfg.timescale,self.opts.movie_timescale,self.limits)?;
        let handler=handler_of(&cfg.entry);
        let count=self.tracks.iter().filter(|t|t.header.handler==handler).count();let audio_limit=if self.max_tracks==2{1}else{6};if handler==Handler::Video&&count!=0||handler==Handler::Audio&&count>=audio_limit{return Err(mux("spooled recording track class/count exceeds admitted profile"));}
        let index=self.tracks.len();let header=WTrack {cfg,id:index as u32+1,handler,pcm_frame_bytes:None,t:Tables::new(),tmcd_ref:None,tmcd_source:None};
        let mut prefix=BoxBuf::new();write_trak_prefix(&mut prefix,&header,self.opts.movie_timescale,false)?;
        if prefix.len()>65536{return Err(mux("spooled track prefix exceeds64KiB"));}
        // Version1 tkhd/mdhd growth plus the maximum eight explicit edits.
        // Header bytes stay fixed; only scalar counts/durations change later.
        let prefix_ceiling=prefix.len() as u64+256;
        self.tracks.push(Track {header,stats:Stats::default(),prefix_ceiling,recovery_origin:None});
        if let Err(error)=self.checkpoint(){self.poisoned=true;return Err(error);}Ok(index)
    }
    /// Observed first native DTS in this track's timescale. Recovery metadata
    /// only: it never retimes a sample or substitutes a fabricated MP4 edit.
    pub fn set_recovery_origin(&mut self,index:usize,origin:i64)->Result<()> {
        if self.poisoned{return Err(mux("interrupted sample log cannot checkpoint a native origin"));}
        let track=self.tracks.get_mut(index).ok_or(Error::NoSuchTrack(index))?;
        if track.recovery_origin.is_some_and(|old|old!=origin){return Err(mux("native track origin changed within a recording epoch"));}
        track.recovery_origin=Some(origin);if let Err(error)=self.checkpoint(){self.poisoned=true;return Err(error);}Ok(())
    }
    pub fn set_track_edits(&mut self,index:usize,edits:Vec<Edit>)->Result<()> {
        if self.poisoned{return Err(mux("spooled MP4 was interrupted; private recovery files retained"));}
        if edits.len()>8||edits.capacity()>16||edits.iter().any(|e|e.media_time < -1||e.media_rate!=0x10000){return Err(mux("unsupported spooled MP4 presentation edits"));}
        let timescale=self.tracks.get(index).ok_or(Error::NoSuchTrack(index))?.header.cfg.timescale;
        validate_edits(&edits,timescale,self.opts.movie_timescale,self.limits)?;
        let track=self.tracks.get_mut(index).ok_or(Error::NoSuchTrack(index))?;track.header.cfg.edits=edits;track.header.cfg.media_start=None;
        if let Err(error)=self.checkpoint(){self.poisoned=true;return Err(error);}Ok(())
    }
    pub fn write_sample(&mut self,index:usize,sample:WriteSample)->Result<()> {
        if self.poisoned{return Err(mux("sample log/media write previously failed; recovery files retained"));}
        let track=self.tracks.get(index).ok_or(Error::NoSuchTrack(index))?;
        if sample.data.is_empty()||sample.data.len()>MAX_SAMPLE||sample.duration==0||sample.composition_offset!=0{return Err(mux("spooled AVC/AAC AU shape, interval or no-reorder contract"));}
        if track.header.handler==Handler::Video&&track.stats.samples==0&&!sample.is_sync{return Err(mux("spooled AVC track must begin with actual IDR"));}
        let next=add(self.records,1)?;let disk=add(RECORD_START,mul(next,RECORD_BYTES)?)?;
        if next>self.limits.max_records||disk>self.limits.max_spool_bytes{return Err(mux("recording sample-count or spool disk limit reached"));}
        let record=Record {track:index as u32,size:sample.data.len() as u32,duration:sample.duration,sync:sample.is_sync,offset:self.pos};
        let mut stats=track.stats;stats.observe(record)?;
        if stats.duration>add(mul(u64::from(track.header.cfg.timescale),u64::from(self.limits.max_duration_seconds))?,native_tail(&track.header)?)?{return Err(mux("recording coded track exceeds presentation-duration plus native-tail profile"));}
        // Reserve the largest table form before accepting media bytes. Native
        //100ns durations/offsets remain exact; this is disk capacity, not RAM.
        let mut moov_bound=256u64; // maximum mvhd plus moov header
        for (i,t) in self.tracks.iter().enumerate(){let table=if i==index{stats}else{t.stats};moov_bound=add(moov_bound,add(t.prefix_ceiling,table.table_bytes()?)?)?;}
        let media_end=add(self.pos,sample.data.len() as u64)?;
        if moov_bound>self.limits.max_moov_bytes||add(media_end,moov_bound)?>self.limits.max_output_bytes{return Err(mux("recording64GiB file/moov disk budget reached"));}
        let result=(||->Result<()>{self.w.write_all(sample.data)?;self.spool.seek(SeekFrom::Start(RECORD_START+self.records*RECORD_BYTES))?;self.spool.write_all(&record.encode(self.records))?;Ok(())})();
        if let Err(error)=result{self.poisoned=true;return Err(error);}
        self.records=next;self.pos=media_end;self.tracks[index].stats=stats;Ok(())
    }
    fn checkpoint(&mut self)->Result<()> {
        // Bounded recovery descriptor: actual codec sample entries and edits,
        // not an incomplete moov masquerading as a playable MP4.
        let mut b=BoxBuf::new();b.bytes(b"SKYCFG01");b.u32(self.opts.movie_timescale);b.u32(self.tracks.len() as u32);b.u64(self.records);b.u64(self.media_start);b.u64(self.pos);b.u64(self.pad_pos);b.u64(self.mdat_pos);
        for track in &self.tracks {
            let t=&track.header;b.u32(t.id);b.u32(t.cfg.timescale);b.u32(track.stats.samples);b.u64(track.stats.duration);
            b.u8(u8::from(track.recovery_origin.is_some()));b.i64(track.recovery_origin.unwrap_or(0));
            b.u32(t.cfg.language.len() as u32);b.bytes(t.cfg.language.as_bytes());let name=t.cfg.handler_name.as_deref().unwrap_or("");b.u32(name.len() as u32);b.bytes(name.as_bytes());
            b.u32(t.cfg.edits.len() as u32);for e in &t.cfg.edits{b.u64(e.segment_duration);b.i64(e.media_time);b.i32(e.media_rate);}
            let mut entry=BoxBuf::new();t.cfg.entry.write(&mut entry,false)?;if entry.len()>65536{return Err(mux("spooled codec header exceeds64KiB"));}
            b.u32(entry.len() as u32);b.bytes(&entry.buf);
        }
        if b.len() as u64+20>CHECKPOINT_SLOT{return Err(mux("spooled recovery descriptor exceeds fixed128KiB slot"));}
        // Alternate fixed slots. A torn descriptor write cannot destroy the
        // preceding valid codec/origin checkpoint. This is not a power-loss
        // durability claim; the host syncs both files before publication.
        let generation=self.checkpoint_generation.checked_add(1).ok_or_else(||mux("sample log checkpoint generation exhausted"))?;
        let at=HEADER+(generation%2)*CHECKPOINT_SLOT;
        self.spool.seek(SeekFrom::Start(at))?;self.spool.write_all(&[0u8;12])?;
        self.spool.write_all(&b.buf)?;self.spool.write_all(&checksum(&b.buf,generation).to_le_bytes())?;
        self.spool.seek(SeekFrom::Start(at))?;self.spool.write_all(&generation.to_le_bytes())?;self.spool.write_all(&(b.len() as u32).to_le_bytes())?;
        self.spool.flush()?;self.checkpoint_generation=generation;Ok(())
    }
    pub fn finish(mut self)->Result<(W,S,SpoolReceipt)> {
        if self.poisoned||self.records==0||self.tracks.is_empty(){return Err(mux("cannot publish an empty or interrupted spooled MP4"));}
        self.spool.flush()?;
        if self.w.seek(SeekFrom::End(0))?!=self.pos{return Err(mux("private media extent changed before streamed finalization"));}
        let mut observed=[Stats::default();MAX_TRACKS];
        scan(&mut self.spool,&self.header,self.records,self.media_start,self.pos,self.tracks.len(),&mut self.scanned,self.limits,|r|{observed[r.track as usize].observe(r)})?;
        for (index,track) in self.tracks.iter_mut().enumerate(){
            if observed[index]!=track.stats{return Err(mux("sample log scalar counters differ from accepted media"));}
            track.header.t.sample_count=u64::from(track.stats.samples);track.header.t.media_duration=track.stats.duration;track.header.t.comp_end=track.stats.duration;
        }
        self.checkpoint()?;
        let movie_ts=self.opts.movie_timescale;let duration=self.tracks.iter().map(|t|presentation_duration(&t.header,movie_ts)).max().unwrap_or(0);
        if duration>mul(u64::from(movie_ts),u64::from(self.limits.max_duration_seconds))?{return Err(mux("recording presentation edits exceed duration profile"));}
        let mut mvhd=BoxBuf::new();write_mvhd(&mut mvhd,movie_ts,duration,self.tracks.len() as u32+1);
        let mut plans=Vec::with_capacity(MAX_TRACKS);let mut moov_bytes=add(8,mvhd.len() as u64)?;
        for track in &self.tracks {
            let mut prefix=BoxBuf::new();let parents=write_trak_prefix(&mut prefix,&track.header,movie_ts,false)?;
            if prefix.len() as u64>track.prefix_ceiling{return Err(mux("spooled final track header exceeds its reserved prefix"));}
            let tables=track.stats.table_bytes()?;
            for at in parents{let size=u32size(add((prefix.len()-at) as u64,tables)?)?;prefix.buf[at..at+4].copy_from_slice(&size.to_be_bytes());}
            moov_bytes=add(moov_bytes,add(prefix.len() as u64,tables)?)?;plans.push(TrackPlan {prefix:prefix.buf,stats:track.stats});
        }
        if moov_bytes>self.limits.max_moov_bytes||add(self.pos,moov_bytes)?>self.limits.max_output_bytes{return Err(mux("streamed moov/file extent exceeds admitted disk budget"));}
        let moov_size=u32size(moov_bytes)?;
        // Upgrade mdat through the existing reserved8-byte free box. Media and
        // chunk offsets stay fixed; co64 follows actual absolute file offsets.
        let size=self.pos-self.mdat_pos;
        if size<=u64::from(u32::MAX){self.w.seek(SeekFrom::Start(self.mdat_pos))?;self.w.write_all(&(size as u32).to_be_bytes())?;}
        else {self.w.seek(SeekFrom::Start(self.pad_pos))?;self.w.write_all(&1u32.to_be_bytes())?;self.w.write_all(b"mdat")?;self.w.write_all(&(self.pos-self.pad_pos).to_be_bytes())?;}
        self.w.seek(SeekFrom::Start(self.pos))?;
        let (written,digest)={
            let mut checked=DigestWriter {inner:&mut self.w,bytes:0,digest:0xcbf29ce484222325};
            let mut out=BufWriter::with_capacity(BUFFER,&mut checked);out.write_all(&moov_size.to_be_bytes())?;out.write_all(b"moov")?;out.write_all(&mvhd.buf)?;
            for (index,plan) in plans.iter().enumerate(){out.write_all(&plan.prefix)?;
                emit_tables(&mut out,&mut self.spool,&self.header,self.records,self.media_start,self.pos,self.tracks.len(),&mut self.scanned,self.limits,index as u32,plan.stats)?;
            }out.flush()?;drop(out);(checked.bytes,checked.digest)
        };
        if written!=moov_bytes{return Err(mux("streamed moov byte count differs from planned tables"));}
        let end=add(self.pos,moov_bytes)?;
        if self.w.stream_position()?!=end{return Err(mux("streamed moov extent differs from computed box sizes"));}
        self.w.flush()?;
        // Read back exactly the bounded on-disk moov, never allocate it. Box
        // extents/table counts were verified while composing. This roundtrip
        // checks the actual bytes, in addition to the original record scans.
        self.w.seek(SeekFrom::Start(self.pos))?;let mut buffer=[0u8;BUFFER];let mut left=moov_bytes;let mut observed_digest=0xcbf29ce484222325;
        while left!=0 {let n=left.min(BUFFER as u64) as usize;self.w.read_exact(&mut buffer[..n])?;hash_update(&mut observed_digest,&buffer[..n]);left-=n as u64;}
        if observed_digest!=digest{return Err(mux("streamed MP4 moov readback differs from emitted bytes"));}
        self.w.seek(SeekFrom::Start(end))?;self.spool.flush()?;
        let receipt=SpoolReceipt {records:self.records,spool_bytes:RECORD_START+self.records*RECORD_BYTES,moov_start:self.pos,moov_bytes,output_bytes:end,scanned_records:self.scanned};Ok((self.w,self.spool,receipt))
    }
}
fn hash_update(state:&mut u64,bytes:&[u8]){for &byte in bytes{*state=(*state^u64::from(byte)).wrapping_mul(0x100000001b3);}}
struct DigestWriter<W:Write>{inner:W,bytes:u64,digest:u64}
impl<W:Write> Write for DigestWriter<W>{
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{let n=self.inner.write(bytes)?;hash_update(&mut self.digest,&bytes[..n]);self.bytes=self.bytes.checked_add(n as u64).ok_or_else(||std::io::Error::other("streamed moov count overflow"))?;Ok(n)}
    fn flush(&mut self)->std::io::Result<()>{self.inner.flush()}
}
fn native_tail(track:&WTrack)->Result<u64>{
    // AAC accepts at most one residual1024-frame block plus two native EOS
    // AUs. Coded priming/padding is retained; presentation edits still obey24h.
    if let CodecConfig::Aac(aac)=&track.cfg.entry.codec {Ok((u64::from(track.cfg.timescale)*3*1024).div_ceil(u64::from(aac.sample_rate))+1)}else{Ok(0)}
}
fn validate_edits(edits:&[Edit],timescale:u32,movie_ts:u32,limits:SpoolLimits)->Result<()> {
    let total=edits.iter().try_fold(0u64,|sum,e|add(sum,e.segment_duration))?;
    if total>mul(u64::from(movie_ts),u64::from(limits.max_duration_seconds))?||edits.iter().any(|e|e.media_time>=0&&e.media_time as u128>u128::from(timescale)*u128::from(limits.max_duration_seconds)){return Err(mux("spooled presentation edits exceed duration/sample-grid profile"));}Ok(())
}
fn bounded_track(cfg:&TrackConfig)->Result<()> {
    if cfg.entry.data_reference_index!=1||cfg.timescale==0||cfg.timescale>1_000_000_000||cfg.language.len()>3||cfg.language.capacity()>32||cfg.handler_name.as_ref().is_some_and(|s|s.len()>256||s.capacity()>512)||cfg.edits.len()>8||cfg.edits.capacity()>16||cfg.media_start.is_some(){return Err(mux("spooled track metadata is outside bounded explicit-edit profile"));}
    if cfg.edits.iter().any(|e|e.media_time < -1||e.media_rate!=0x10000){return Err(mux("spooled track edit profile"));}
    match &cfg.entry.codec {
        CodecConfig::Avc(avc)=>{
            if cfg.entry.format.0!=*b"avc1"||cfg.entry.video.is_none()||cfg.entry.audio.is_some()||!matches!(avc.profile,66|100)||avc.length_size!=4||avc.sps.is_empty()||avc.pps.is_empty()||avc.sps.len()+avc.pps.len()>128||avc.sps.capacity()>31||avc.pps.capacity()>255||avc.ext.capacity()>64{return Err(mux("spooled AVC header profile"));}
            let retained=avc.sps.iter().chain(avc.pps.iter()).try_fold(avc.ext.capacity(),|sum,n|sum.checked_add(n.capacity())).ok_or_else(||mux("spooled AVC header size overflow"))?;
            if retained>32768||avc.sps.iter().chain(avc.pps.iter()).any(|n|n.is_empty()||n.len()>32768)||cfg.entry.video.as_ref().is_some_and(|v|v.compressor_name.capacity()>256||matches!(&v.color,Some(ColorInfo::Icc {..}))){return Err(mux("spooled AVC parameter memory bound"));}
        }
        CodecConfig::Aac(aac)=>{
            if cfg.entry.format.0!=*b"mp4a"||cfg.entry.audio.is_none()||cfg.entry.video.is_some()||aac.asc.len()!=2||aac.asc.capacity()>64||aac.object_type!=2||!matches!(aac.sample_rate,44100|48000)||!matches!(aac.channel_config,1|2)||cfg.entry.audio.as_ref().is_some_and(|a|a.qt_version!=0||a.channels!=u32::from(aac.channel_config)||a.sample_rate!=f64::from(aac.sample_rate)){return Err(mux("spooled AAC-LC header profile"));}
        }
        _=>return Err(mux("spooled recording supports actual AVC/AAC packet tracks only")),
    }Ok(())
}
#[allow(clippy::too_many_arguments)]
fn scan<S:Read+Seek>(spool:&mut S,expected_header:&[u8;64],records:u64,media_start:u64,media_end:u64,tracks:usize,scanned:&mut u64,limits:SpoolLimits,mut visit:impl FnMut(Record)->Result<()>)->Result<()> {
    *scanned=add(*scanned,records)?;if *scanned>limits.max_scan_records{return Err(mux("spooled MP4 finalization scan-work budget reached"));}
    let expected=add(RECORD_START,mul(records,RECORD_BYTES)?)?;
    if spool.seek(SeekFrom::End(0))?!=expected{return Err(mux("sample log has a partial/orphan/truncated record; recovery files retained"));}
    spool.seek(SeekFrom::Start(0))?;let mut header=[0u8;64];spool.read_exact(&mut header)?;if &header!=expected_header{return Err(mux("sample log header/limits changed"));}
    spool.seek(SeekFrom::Start(RECORD_START))?;let mut input=BufReader::with_capacity(BUFFER,spool);let mut offset=media_start;
    for sequence in 0..records {
        let mut bytes=[0u8;32];input.read_exact(&mut bytes)?;let r=Record::decode(bytes,sequence)?;
        if r.track as usize>=tracks||r.size==0||r.size as usize>MAX_SAMPLE||r.duration==0||r.offset!=offset{return Err(mux("sample log track/shape/media ordering differs from admitted prefix"));}
        offset=add(offset,u64::from(r.size))?;if offset>media_end{return Err(mux("sample log sample extends beyond accepted mdat"));}visit(r)?;
    }
    if offset!=media_end{return Err(mux("sample log does not cover the complete accepted mdat"));}Ok(())
}
fn full<W:Write>(out:&mut W,size:u64,kind:&[u8;4])->Result<()>{out.write_all(&u32size(size)?.to_be_bytes())?;out.write_all(kind)?;out.write_all(&0u32.to_be_bytes())?;Ok(())}
#[allow(clippy::too_many_arguments)]
fn emit_tables<W:Write,S:Read+Seek>(out:&mut W,spool:&mut S,header:&[u8;64],records:u64,media_start:u64,media_end:u64,tracks:usize,scanned:&mut u64,limits:SpoolLimits,track:u32,stats:Stats)->Result<()> {
    full(out,16+u64::from(stats.runs)*8,b"stts")?;out.write_all(&stats.runs.to_be_bytes())?;
    let (mut run_count,mut duration,mut emitted)=(0u32,0u32,0u32);
    scan(spool,header,records,media_start,media_end,tracks,scanned,limits,|r|{
        if r.track==track {if run_count!=0&&r.duration!=duration{out.write_all(&run_count.to_be_bytes())?;out.write_all(&duration.to_be_bytes())?;emitted+=1;run_count=0;}duration=r.duration;run_count=run_count.checked_add(1).ok_or_else(||mux("stts run overflow"))?;}Ok(())})?;
    if run_count!=0{out.write_all(&run_count.to_be_bytes())?;out.write_all(&duration.to_be_bytes())?;emitted+=1;}if emitted!=stats.runs{return Err(mux("streamed stts run count changed"));}
    if stats.sync!=stats.samples {
        full(out,16+u64::from(stats.sync)*4,b"stss")?;out.write_all(&stats.sync.to_be_bytes())?;let (mut index,mut emitted)=(0u32,0u32);
        scan(spool,header,records,media_start,media_end,tracks,scanned,limits,|r|{if r.track==track{index+=1;if r.sync{out.write_all(&index.to_be_bytes())?;emitted+=1;}}Ok(())})?;
        if index!=stats.samples||emitted!=stats.sync{return Err(mux("streamed stss count changed"));}
    }
    full(out,if stats.samples==0{16}else{28},b"stsc")?;out.write_all(&u32::from(stats.samples!=0).to_be_bytes())?;if stats.samples!=0{for value in [1u32,1,1]{out.write_all(&value.to_be_bytes())?;}}
    full(out,20+if stats.uniform{0}else{u64::from(stats.samples)*4},b"stsz")?;out.write_all(&(if stats.uniform{stats.first_size}else{0}).to_be_bytes())?;out.write_all(&stats.samples.to_be_bytes())?;
    if !stats.uniform {let mut emitted=0u32;scan(spool,header,records,media_start,media_end,tracks,scanned,limits,|r|{if r.track==track{out.write_all(&r.size.to_be_bytes())?;emitted+=1;}Ok(())})?;if emitted!=stats.samples{return Err(mux("streamed stsz count changed"));}}
    full(out,16+u64::from(stats.samples)*if stats.co64{8}else{4},if stats.co64{b"co64"}else{b"stco"})?;out.write_all(&stats.samples.to_be_bytes())?;let mut emitted=0u32;
    scan(spool,header,records,media_start,media_end,tracks,scanned,limits,|r|{if r.track==track{if stats.co64{out.write_all(&r.offset.to_be_bytes())?;}else{out.write_all(&u32size(r.offset)?.to_be_bytes())?;}emitted+=1;}Ok(())})?;
    if emitted!=stats.samples{return Err(mux("streamed chunk offset count changed"));}Ok(())
}
