//! Bounded recording-only Matroska v4 port (RFC8794/RFC9559). No demux/remux,
//! HDR, lacing, B frames, external muxer, native handles or filesystem policy.
//! Small EBML encoding conventions adapted from craft-libs (MIT); see
//! ../../third_party/CRAFT-MATROSKA-MIT.txt. Full Craft muxer is not imported:
//! its in-memory cluster/cue vectors are unsuitable for24hour recording.
use std::{collections::VecDeque,io::{Read,Write,Seek,SeekFrom,BufReader,BufWriter}};
use crate::isobmff::{Error,Result};
const SCALE:u64=100;
const PACKET_BYTES:usize=2*1024*1024;
const QUEUE_BYTES:usize=64*1024*1024;
const QUEUE_PACKETS:usize=576;
const MULTI_QUEUE_PACKETS:usize=1024;
const MAX_MULTI_PACKETS:u64=34_668_020;
const SEEK_SPACE:usize=512;
const CUE_HEADER:u64=64;
const CUE_BYTES:u64=32;
const CUE_POINT_BYTES:u64=30;
const SCRATCH:usize=64*1024;
fn bad(s:&str)->Error{Error::Mux(s.into())}
fn plus(a:u64,b:u64)->Result<u64>{a.checked_add(b).ok_or_else(||bad("Matroska count/extent overflow"))}
fn times(a:u64,b:u64)->Result<u64>{a.checked_mul(b).ok_or_else(||bad("Matroska count/extent overflow"))}
fn hash(bytes:&[u8],sequence:u64)->u64{bytes.iter().fold(0xcbf29ce484222325^sequence,|s,b|(s^u64::from(*b)).wrapping_mul(0x100000001b3))}
fn id(out:&mut Vec<u8>,value:u32){let n=if value>=0x1000000{4}else if value>=0x10000{3}else if value>=0x100{2}else{1};out.extend_from_slice(&value.to_be_bytes()[4-n..]);}
fn size(out:&mut Vec<u8>,value:u64,width:usize)->Result<()>{let mut n=width.max(1);while n<8&&value>=(1u64<<(7*n))-1{n+=1;}if n>8||value>=(1u64<<56)-1{return Err(bad("Matroska element size exceeds EBML profile"));}let encoded=value|(1u64<<(7*n));out.extend_from_slice(&encoded.to_be_bytes()[8-n..]);Ok(())}
fn element(out:&mut Vec<u8>,kind:u32,data:&[u8])->Result<usize>{id(out,kind);size(out,data.len() as u64,1)?;let start=out.len();out.extend_from_slice(data);Ok(start)}
fn uint(out:&mut Vec<u8>,kind:u32,value:u64)->Result<()>{let n=(8-value.leading_zeros() as usize/8).max(1);element(out,kind,&value.to_be_bytes()[8-n..])?;Ok(())}
fn uint8(out:&mut Vec<u8>,kind:u32,value:u64)->Result<usize>{element(out,kind,&value.to_be_bytes())}
fn void(out:&mut Vec<u8>,bytes:usize)->Result<()>{if bytes<9{return Err(bad("Matroska reserved Void is too short"));}id(out,0xec);size(out,(bytes-9) as u64,8)?;out.resize(out.len()+bytes-9,0);Ok(())}
#[derive(Clone,Debug)]
pub struct Audio {pub asc:Vec<u8>,pub rate:u32,pub channels:u16}
#[derive(Clone,Debug)]
pub struct Config {pub avcc:Vec<u8>,pub width:u32,pub height:u32,pub audio:Option<Audio>}
#[derive(Clone,Copy,Debug)]
pub struct Limits {pub packets:u64,pub video_packets:u64,pub output_bytes:u64,pub duration_100ns:u64}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct AudioId(u8);
impl AudioId{pub fn new(id:u8)->Result<Self>{if (1..=6).contains(&id){Ok(Self(id))}else{Err(bad("Matroska logical audio ID must be1..6"))}}pub fn get(self)->u8{self.0}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum Track {Video,Audio,AudioIndex(AudioId)}
impl Track {fn index(self)->usize{match self{Self::Video=>0,Self::Audio=>1,Self::AudioIndex(id)=>usize::from(id.0)}}}
#[derive(Clone,Debug)]pub struct NamedAudio{pub id:AudioId,pub name:String,pub format:Audio}
pub struct Packet<'a>{pub track:Track,pub bytes:&'a[u8],pub pts:i64,pub duration:i64,pub key:bool,pub discard_padding_ns:u64}
struct Owned {track:Track,bytes:Box<[u8]>,at:u64,duration:u64,key:bool,padding:u64,wire:u64}
#[derive(Clone,Copy,Debug)]
pub struct Receipt {pub bytes:u64,pub packets:u64,pub cues:u64,pub cue_bytes:u64,pub duration_100ns:u64}
pub struct Writer<W:Read+Write+Seek,S:Read+Write+Seek>{
    media:W,cues:S,config:Config,limits:Limits,audio_tracks:Vec<NamedAudio>,enabled:[bool;7],ended:[bool;7],multitrack:bool,queue:[VecDeque<Owned>;7],queued:usize,queued_wire:u64,pending_cues:u64,
    cue_header:[u8;64],highest:[Option<u64>;7],count:u64,written:u64,cue_count:u64,pos:u64,segment_size:u64,segment_data:u64,
    seek:u64,info:u64,tracks:u64,duration_at:u64,delay_at:[Option<u64>;6],delay_ticks:[Option<u64>;6],last_written:Option<u64>,end:u64,poisoned:bool,
}
impl<W:Read+Write+Seek,S:Read+Write+Seek> Writer<W,S>{
    pub fn new(media:W,cues:S,config:Config,limits:Limits)->Result<Self>{let audios=config.audio.clone().map(|format|NamedAudio{id:AudioId(1),name:String::new(),format}).into_iter().collect();Self::new_profile(media,cues,config,limits,audios,false)}
    pub fn new_multitrack(media:W,cues:S,config:Config,limits:Limits,audios:Vec<NamedAudio>)->Result<Self>{if config.audio.is_some(){return Err(bad("multitrack audio must be supplied as explicit independent tracks"));}Self::new_profile(media,cues,config,limits,audios,true)}
    fn new_profile(mut media:W,mut cues:S,config:Config,limits:Limits,mut audios:Vec<NamedAudio>,multitrack:bool)->Result<Self>{
        let audio_limit=if multitrack{6}else{1};
        if audios.len()>audio_limit||audios.capacity()>8{return Err(bad("Matroska audio track count/capacity"));}audios.sort_by_key(|a|a.id.get());
        for(index,track)in audios.iter().enumerate(){if track.name.len()>256||track.name.capacity()>512||track.name.chars().any(char::is_control)||index!=0&&audios[index-1].id==track.id{return Err(bad("Matroska audio name/identity"));}let a=&track.format;if a.asc.len()!=2||a.asc.capacity()>64||![44100,48000].contains(&a.rate)||!(1..=2).contains(&a.channels){return Err(bad("Matroska AAC-LC track format"));}let expected=if a.rate==48000{[0x11,0x80|((a.channels as u8)<<3)]}else{[0x12,(a.channels as u8)<<3]};if a.asc.as_slice()!=expected{return Err(bad("Matroska track ASC differs from actual format"));}}
        let mut enabled=[false;7];enabled[0]=true;for a in &audios{enabled[usize::from(a.id.get())]=true;}

        if config.avcc.len()<7||config.avcc.len()>32768||config.avcc.capacity()>65536||config.width<16||config.height<16||config.width>4096||config.height>4096||config.width%2!=0||config.height%2!=0||u64::from(config.width)*u64::from(config.height)>8_847_360{return Err(bad("Matroska AVC header/dimensions outside recording profile"));}
        let avc=crate::isobmff::AvcConfig::parse(&config.avcc)?;if !matches!(avc.profile,66|100)||avc.length_size!=4||avc.sps.is_empty()||avc.pps.is_empty(){return Err(bad("Matroska requires self-contained Baseline/High avcC with4-byte NAL lengths"));}
        if let Some(a)=&config.audio{if a.asc.len()!=2||a.asc.capacity()>64||![44100,48000].contains(&a.rate)||!(1..=2).contains(&a.channels){return Err(bad("Matroska AAC-LC header/rate/channels"));}let expected=if a.rate==48000{[0x11,0x80|((a.channels as u8)<<3)]}else{[0x12,(a.channels as u8)<<3]};if a.asc.as_slice()!=expected{return Err(bad("Matroska ASC differs from actual AAC-LC format"));}}
        let packet_limit=if multitrack{MAX_MULTI_PACKETS}else{20_000_000};
        if limits.packets==0||limits.packets>packet_limit||limits.video_packets==0||limits.video_packets>10_368_008||limits.video_packets>limits.packets||limits.output_bytes>64*1024*1024*1024||limits.output_bytes<1024||limits.duration_100ns==0||limits.duration_100ns>86400*10_000_000{return Err(bad("Matroska recording count/duration/file limits"));}
        if media.seek(SeekFrom::End(0))?!=0||cues.seek(SeekFrom::End(0))?!=0{return Err(bad("Matroska media and cue ports must be newly created/empty"));}
        let mut h=Vec::new();let mut e=Vec::new();for (kind,value) in [(0x4286,1),(0x42f7,1),(0x42f2,4),(0x42f3,8)]{uint(&mut e,kind,value)?;}element(&mut e,0x4282,b"matroska")?;uint(&mut e,0x4287,4)?;uint(&mut e,0x4285,4)?;element(&mut h,0x1a45dfa3,&e)?;
        id(&mut h,0x18538067);let segment_size=h.len() as u64;h.extend_from_slice(&[1,255,255,255,255,255,255,255]);let segment_data=h.len() as u64;let seek=segment_data;void(&mut h,SEEK_SPACE)?;
        let info=h.len() as u64;let mut i=Vec::new();uint(&mut i,0x2ad7b1,SCALE)?;element(&mut i,0x4d80,b"SkyLive Rust")?;element(&mut i,0x5741,b"SkyLive")?;let duration_local=element(&mut i,0x4489,&0f64.to_be_bytes())?;let info_data=element(&mut h,0x1549a966,&i)?;let duration_at=(info_data+duration_local) as u64;
        let tracks=h.len() as u64;let mut t=Vec::new();let mut v=Vec::new();uint(&mut v,0xd7,1)?;uint(&mut v,0x73c5,1)?;uint(&mut v,0x83,1)?;uint(&mut v,0x9c,0)?;element(&mut v,0x86,b"V_MPEG4/ISO/AVC")?;element(&mut v,0x63a2,&config.avcc)?;
        let mut video=Vec::new();uint(&mut video,0xb0,u64::from(config.width))?;uint(&mut video,0xba,u64::from(config.height))?;uint(&mut video,0x9a,2)?;let mut colour=Vec::new();for (kind,value) in [(0x55b1,1),(0x55b2,8),(0x55b9,1),(0x55ba,1),(0x55bb,1)]{uint(&mut colour,kind,value)?;}element(&mut video,0x55b0,&colour)?;element(&mut v,0xe0,&video)?;element(&mut t,0xae,&v)?;
        let mut delay_local=[None;6];
        for track in &audios{let a=&track.format;let number=u64::from(track.id.get())+1;let mut entry=Vec::new();uint(&mut entry,0xd7,number)?;uint(&mut entry,0x73c5,number)?;uint(&mut entry,0x83,2)?;uint(&mut entry,0x9c,0)?;if !track.name.is_empty(){element(&mut entry,0x536e,track.name.as_bytes())?;}element(&mut entry,0x86,b"A_AAC")?;element(&mut entry,0x63a2,&a.asc)?;let offset=uint8(&mut entry,0x56aa,0)?;
            let mut audio=Vec::new();element(&mut audio,0xb5,&f64::from(a.rate).to_be_bytes())?;uint(&mut audio,0x9f,u64::from(a.channels))?;uint(&mut audio,0x6264,16)?;element(&mut entry,0xe1,&audio)?;let start=element(&mut t,0xae,&entry)?;delay_local[usize::from(track.id.get()-1)]=Some(start+offset);
        }
        let tracks_data=element(&mut h,0x1654ae6b,&t)?;let delay_at=delay_local.map(|offset|offset.map(|n|(tracks_data+n) as u64));if h.len()>65536{return Err(bad("Matroska fixed header exceeds64KiB"));}media.write_all(&h)?;
        let mut log=[0u8;64];log[..8].copy_from_slice(b"SKYMKVC1");log[8..16].copy_from_slice(&limits.video_packets.to_le_bytes());log[16..24].copy_from_slice(&CUE_BYTES.to_le_bytes());let crc=hash(&log[..56],0);log[56..64].copy_from_slice(&crc.to_le_bytes());cues.write_all(&log)?;cues.flush()?;
        Ok(Self{media,cues,config,limits,audio_tracks:audios,enabled,ended:[false;7],multitrack,queue:std::array::from_fn(|i|VecDeque::with_capacity(if enabled[i]{if i==0||!multitrack{288}else{112}}else{0})),queued:0,queued_wire:0,pending_cues:0,cue_header:log,highest:[None;7],count:0,written:0,cue_count:0,pos:h.len() as u64,segment_size,segment_data,seek,info,tracks,duration_at,delay_at,delay_ticks:[None;6],last_written:None,end:0,poisoned:false})
    }
    pub fn audio_delay(&mut self,native_first:i64)->Result<()>{self.audio_delay_for(AudioId(1),native_first)}
    pub fn audio_delay_for(&mut self,id:AudioId,native_first:i64)->Result<()>{
        if self.poisoned{return Err(bad("interrupted Matroska output"));}let audio=self.audio_tracks.iter().find(|a|a.id==id).ok_or_else(||bad("Matroska audio track is disabled"))?;
        let ticks=if native_first<0{native_first.checked_neg().ok_or_else(||bad("AAC codec delay overflow"))? as u64}else{0};let max=(3u64*1024*10_000_000).div_ceil(u64::from(audio.format.rate))+1;if ticks>max{return Err(bad("observed AAC delay exceeds native residual/EOS profile"));}
        let i=usize::from(id.get()-1);if self.delay_ticks[i].is_some_and(|old|old!=ticks){return Err(bad("AAC codec delay changed within a track"));}let at=self.delay_at[i].ok_or_else(||bad("Matroska CodecDelay reservation missing"))?;self.media.seek(SeekFrom::Start(at))?;self.media.write_all(&times(ticks,SCALE)?.to_be_bytes())?;self.media.seek(SeekFrom::Start(self.pos))?;self.delay_ticks[i]=Some(ticks);Ok(())
    }
    /// Real producer/codec EOF releases only this track's mux watermark.
    pub fn finish_track(&mut self,track:Track)->Result<()>{let i=track.index();if !self.enabled[i]{return Err(bad("Matroska track is not selected"));}self.ended[i]=true;self.drain(false)}
    pub fn packet(&mut self,p:Packet<'_>)->Result<()>{
        if self.poisoned{return Err(bad("Matroska write previously failed; private file retained"));}
        let index=p.track.index();if !self.enabled[index]||self.ended[index]{return Err(bad("Matroska packet for disabled/ended track"));}let rate=if index==0{None}else{Some(self.audio_tracks.iter().find(|a|usize::from(a.id.get())==index).ok_or_else(||bad("Matroska audio identity"))?.format.rate)};
        if p.bytes.is_empty()||p.bytes.len()>PACKET_BYTES||p.duration<=0||p.pts.checked_add(p.duration).is_none()||index==0&&p.discard_padding_ns!=0||p.discard_padding_ns>times(p.duration as u64,SCALE)?.max(rate.map_or(0,|r|(1024u64*1_000_000_000).div_ceil(u64::from(r)))){return Err(bad("Matroska packet shape/tail padding"));}
        if index==0&&self.highest[0].is_none()&&!p.key{return Err(bad("Matroska video must start with actual IDR"));}
        let delay=if index!=0{self.delay_ticks[index-1].ok_or_else(||bad("actual native AAC delay not observed for this track"))?}else{0};
        let at=u64::try_from(i128::from(p.pts)+i128::from(delay)).map_err(|_|bad("Matroska block remains negative after actual codec delay"))?;
        if self.highest[index].is_some_and(|last|at<=last)||at>plus(self.limits.duration_100ns,20_000_000)?{return Err(bad("Matroska native timestamps/order/duration"));}
        let next=plus(self.count,1)?;if next>self.limits.packets{return Err(bad("Matroska packet count limit"));}
        let slots=if self.multitrack{MULTI_QUEUE_PACKETS}else{QUEUE_PACKETS};let track_slots=if index==0||!self.multitrack{288}else{112};
        if self.queue.iter().map(VecDeque::len).sum::<usize>()>=slots||self.queue[index].len()>=track_slots||self.queued.checked_add(p.bytes.len()).is_none_or(|n|n>QUEUE_BYTES){return Err(bad("Matroska encoded interleave byte/packet/track limit reached"));}
        let cue=p.track==Track::Video&&p.key;let future_cues=plus(plus(self.cue_count,self.pending_cues)?,u64::from(cue))?;if future_cues>self.limits.video_packets{return Err(bad("Matroska cue disk/count limit"));}
        let wire=wire_bytes(p.track,p.bytes.len() as u64,p.duration as u64,p.discard_padding_ns)?;
        if plus(plus(self.pos,plus(self.queued_wire,wire)?)?,plus(64,times(future_cues,CUE_POINT_BYTES)?)?)?>self.limits.output_bytes{return Err(bad("Matroska64GiB media/cue reservation reached"));}
        let mut bytes=Vec::new();bytes.try_reserve_exact(p.bytes.len()).map_err(|_|bad("Matroska encoded queue allocation failed"))?;bytes.extend_from_slice(p.bytes);
        self.queued+=p.bytes.len();self.queued_wire=plus(self.queued_wire,wire)?;self.pending_cues+=u64::from(cue);self.highest[index]=Some(at);self.count=next;
        self.queue[index].push_back(Owned{track:p.track,bytes:bytes.into_boxed_slice(),at,duration:p.duration as u64,key:p.key,padding:p.discard_padding_ns,wire});
        if let Err(error)=self.drain(false){self.poisoned=true;return Err(error);}Ok(())
    }
    fn drain(&mut self,eos:bool)->Result<()>{loop{
        let selected=(0..7).filter_map(|i|self.queue[i].front().map(|p|(p.at,i))).min();let Some((_,index))=selected else{return Ok(())};
        let front=self.queue[index].front().ok_or_else(||bad("Matroska queue front missing"))?;
        if !eos&&(0..7).any(|peer|peer!=index&&self.enabled[peer]&&!self.ended[peer]&&self.highest[peer].is_none_or(|mark|if peer<index{mark<=front.at}else{mark<front.at})){return Ok(());}
        let unit=self.queue[index].pop_front().ok_or_else(||bad("Matroska queue pop missing"))?;self.queued-=unit.bytes.len();self.queued_wire-=unit.wire;self.pending_cues-=u64::from(unit.track==Track::Video&&unit.key);self.emit(unit)?;
    }}
    fn emit(&mut self,p:Owned)->Result<()>{
        if self.last_written.is_some_and(|last|p.at<last){return Err(bad("Matroska cluster timestamp moved backwards"));}
        let mut prefix=Vec::with_capacity(128);let mut timestamp=Vec::new();uint8(&mut timestamp,0xe7,p.at)?;let block_relative=timestamp.len() as u64;
        let mut block=Vec::with_capacity(64);let mut header=Vec::new();id(&mut header,if p.track==Track::Video{0xa3}else{0xa1});size(&mut header,p.bytes.len() as u64+4,1)?;
        let mut suffix=Vec::new();if p.track.index()!=0{uint8(&mut suffix,0x9b,p.duration)?;if p.padding!=0{element(&mut suffix,0x75a2,&i64::try_from(p.padding).map_err(|_|bad("AAC discard padding overflow"))?.to_be_bytes())?;}id(&mut block,0xa0);size(&mut block,plus(header.len() as u64+4+p.bytes.len() as u64,suffix.len() as u64)?,1)?;}
        block.extend_from_slice(&header);block.extend_from_slice(&[0x80|(p.track.index() as u8+1),0,0,if p.track==Track::Video&&p.key{0x80}else{0}]);
        id(&mut prefix,0x1f43b675);size(&mut prefix,plus(timestamp.len() as u64+block.len() as u64+p.bytes.len() as u64,suffix.len() as u64)?,8)?;prefix.extend_from_slice(&timestamp);prefix.extend_from_slice(&block);
        let start=self.pos;self.media.write_all(&prefix)?;self.media.write_all(&p.bytes)?;self.media.write_all(&suffix)?;
        self.pos=plus(self.pos,prefix.len() as u64+p.bytes.len() as u64+suffix.len() as u64)?;if self.pos-start!=p.wire{return Err(bad("Matroska cluster extent differs from reservation"));}
        if p.track==Track::Video&&p.key{let mut record=[0u8;32];record[..8].copy_from_slice(&p.at.to_le_bytes());record[8..16].copy_from_slice(&(start-self.segment_data).to_le_bytes());record[16..24].copy_from_slice(&block_relative.to_le_bytes());let crc=hash(&record[..24],self.cue_count);record[24..].copy_from_slice(&crc.to_le_bytes());self.cues.write_all(&record)?;self.cue_count+=1;}
        self.last_written=Some(p.at);self.written+=1;self.end=self.end.max(plus(p.at,p.duration)?.saturating_sub(p.padding/SCALE));Ok(())
    }
    pub fn finish(mut self,duration:i64)->Result<(W,S,Receipt)>{
        if self.poisoned||self.count==0||self.highest[0].is_none()||self.audio_tracks.iter().any(|a|{let i=usize::from(a.id.get());self.delay_ticks[i-1].is_none()||self.highest[i].is_none()}){return Err(bad("Matroska has no complete actual AVC/AAC output"));}
        let duration=u64::try_from(duration).map_err(|_|bad("Matroska final duration"))?;if duration==0||duration>self.limits.duration_100ns{return Err(bad("Matroska presentation exceeds24hour profile"));}
        self.drain(true)?;if self.written!=self.count||self.queued!=0||self.pending_cues!=0{return Err(bad("Matroska final encoded queue coverage"));}
        self.cues.flush()?;let cue_bytes=plus(CUE_HEADER,times(self.cue_count,CUE_BYTES)?)?;if self.cues.seek(SeekFrom::End(0))?!=cue_bytes{return Err(bad("Matroska cue journal has an incomplete tail"));}
        let cue_pos=self.pos;let mut ch=Vec::new();id(&mut ch,0x1c53bb6b);size(&mut ch,times(self.cue_count,CUE_POINT_BYTES)?,8)?;self.media.write_all(&ch)?;
        self.cues.seek(SeekFrom::Start(0))?;let mut header=[0u8;64];self.cues.read_exact(&mut header)?;if header!=self.cue_header{return Err(bad("Matroska cue header changed"));}
        self.cues.seek(SeekFrom::Start(CUE_HEADER))?;let mut input=BufReader::with_capacity(SCRATCH,&mut self.cues);let mut output=BufWriter::with_capacity(SCRATCH,&mut self.media);let mut last=None;let mut cue=Vec::with_capacity(64);
        for sequence in 0..self.cue_count{let mut r=[0u8;32];input.read_exact(&mut r)?;let get=|at|u64::from_le_bytes([r[at],r[at+1],r[at+2],r[at+3],r[at+4],r[at+5],r[at+6],r[at+7]]);let time=get(0);let pos=get(8);if get(24)!=hash(&r[..24],sequence)||last.is_some_and(|v|time<=v)||pos>=cue_pos-self.segment_data||get(16)!=block_timestamp_bytes(){return Err(bad("Matroska cue journal checksum/time/position changed"));}last=Some(time);cue.clear();let mut point=Vec::with_capacity(48);uint8(&mut point,0xb3,time)?;let mut positions=Vec::with_capacity(32);uint(&mut positions,0xf7,1)?;uint8(&mut positions,0xf1,pos)?;uint(&mut positions,0xf0,get(16))?;element(&mut point,0xb7,&positions)?;element(&mut cue,0xbb,&point)?;if cue.len() as u64!=CUE_POINT_BYTES{return Err(bad("Matroska fixed cue extent"));}output.write_all(&cue)?;}
        output.flush()?;drop(output);drop(input);self.pos=plus(cue_pos,ch.len() as u64+times(self.cue_count,CUE_POINT_BYTES)?)?;if self.pos>self.limits.output_bytes||self.media.stream_position()?!=self.pos{return Err(bad("Matroska final file/cue extent"));}
        let mut seek=Vec::new();for(kind,at)in[(0x1549a966,self.info),(0x1654ae6b,self.tracks),(0x1c53bb6b,cue_pos)]{let mut e=Vec::new();let mut k=Vec::new();id(&mut k,kind);element(&mut e,0x53ab,&k)?;uint8(&mut e,0x53ac,at-self.segment_data)?;element(&mut seek,0x4dbb,&e)?;}let mut slot=Vec::new();element(&mut slot,0x114d9b74,&seek)?;let remaining=SEEK_SPACE.checked_sub(slot.len()).ok_or_else(||bad("Matroska SeekHead reservation overflow"))?;void(&mut slot,remaining)?;self.media.seek(SeekFrom::Start(self.seek))?;self.media.write_all(&slot)?;
        self.media.seek(SeekFrom::Start(self.duration_at))?;self.media.write_all(&(duration as f64).to_be_bytes())?;self.media.seek(SeekFrom::Start(self.segment_size))?;let mut segment_size=Vec::new();size(&mut segment_size,self.pos-self.segment_data,8)?;self.media.write_all(&segment_size)?;self.media.seek(SeekFrom::Start(self.pos))?;self.media.flush()?;
        let receipt=Receipt{bytes:self.pos,packets:self.written,cues:self.cue_count,cue_bytes,duration_100ns:duration};Ok((self.media,self.cues,receipt))
    }
}
fn block_timestamp_bytes()->u64{10} // E7 +1-byte EBML size +8-byte unsigned value
fn wire_bytes(track:Track,bytes:u64,duration:u64,padding:u64)->Result<u64>{
    let mut timestamp=Vec::new();uint8(&mut timestamp,0xe7,0)?;let mut h=Vec::new();id(&mut h,if track==Track::Video{0xa3}else{0xa1});size(&mut h,bytes+4,1)?;let mut suffix=Vec::new();if track.index()!=0{uint8(&mut suffix,0x9b,duration)?;if padding!=0{element(&mut suffix,0x75a2,&0i64.to_be_bytes())?;}let mut master=Vec::new();id(&mut master,0xa0);size(&mut master,h.len() as u64+4+bytes+suffix.len() as u64,1)?;h.splice(..0,master);}
    plus(12,plus(timestamp.len() as u64+h.len() as u64+4+suffix.len() as u64,bytes)?)
}

/// Bounded publication verification: known-size top-level Segment children,
/// complete clusters/Cues, exact64GiB extent. Encoded payload is not decoded.
pub fn verify<R:Read+Seek>(file:&mut R,max_packets:u64)->Result<()>{
    if max_packets==0||max_packets>MAX_MULTI_PACKETS{return Err(bad("Matroska verification packet bound"));}
    let length=file.seek(SeekFrom::End(0))?;if length>64*1024*1024*1024||length<32{return Err(bad("Matroska finalized file length"));}file.seek(SeekFrom::Start(0))?;
    let mut input=BufReader::with_capacity(SCRATCH,file);let(kind,size,head)=read_element(&mut input)?;if kind!=0x1a45dfa3||size>65536{return Err(bad("Matroska EBML header missing or too large"));}
    input.seek_relative(i64::try_from(size).map_err(|_|bad("EBML header extent"))?)?;let header_end=plus(head,size)?;
    let(kind,size,head)=read_element(&mut input)?;let mut at=plus(header_end,head)?;if kind!=0x18538067||plus(at,size)?!=length{return Err(bad("Matroska finalized Segment extent"));}
    let(mut info,mut tracks,mut clusters,mut cues,mut children)=(false,false,0u64,false,0u64);
    while at<length{children+=1;if children>max_packets+16{return Err(bad("Matroska top-level scan count limit"));}
        let(kind,size,head)=read_element(&mut input)?;let end=plus(plus(at,head)?,size)?;if end>length{return Err(bad("Matroska incomplete final child"));}
        match kind{0x1549a966=>info=true,0x1654ae6b=>tracks=true,0x1c53bb6b=>cues=true,0x1f43b675=>{clusters+=1;if size<8||size>PACKET_BYTES as u64+256{return Err(bad("Matroska cluster block extent"));}},0xec|0x114d9b74=>{},_=>return Err(bad("unexpected Matroska recording Segment child"))}
        input.seek_relative(i64::try_from(size).map_err(|_|bad("Matroska child seek extent"))?)?;at=end;
    }
    if !info||!tracks||!cues||clusters==0||clusters>max_packets{return Err(bad("Matroska missing finalized Info/Tracks/Clusters/Cues"));}Ok(())
}
fn read_element<R:Read>(input:&mut R)->Result<(u32,u64,u64)>{
    let mut b=[0u8;1];input.read_exact(&mut b)?;let ni=b[0].leading_zeros() as usize+1;if ni>4{return Err(bad("Matroska invalid element ID"));}let mut id=u32::from(b[0]);for _ in 1..ni{input.read_exact(&mut b)?;id=(id<<8)|u32::from(b[0]);}
    input.read_exact(&mut b)?;let ns=b[0].leading_zeros() as usize+1;if ns>8{return Err(bad("Matroska invalid element size"));}let mut size=u64::from(b[0]&((1u8<<(8-ns))-1));for _ in 1..ns{input.read_exact(&mut b)?;size=(size<<8)|u64::from(b[0]);}if size==(1u64<<(7*ns))-1{return Err(bad("Matroska unknown size is not finalized"));}Ok((id,size,(ni+ns) as u64))
}
