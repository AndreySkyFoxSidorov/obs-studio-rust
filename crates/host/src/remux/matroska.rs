//! Strict, bounded demux for our finalized recording profile. No decoder.
//! Untrusted sizes never become allocation sizes without an independent cap.
use std::io::{Read,Seek,SeekFrom};
use skylive_mp4::{AvcConfig,SpoolMp4Writer,WriteSample,Edit};
use skylive_h264::{params::{Sps,Pps},slice::{NalHeader,SliceHeader,SliceType}};
use skylive_codec_bits::unescape_rbsp;
type Result<T> = std::result::Result<T,String>;
pub(super) const MAX_PACKETS:u64=14_500_000;
const AU:usize=2*1024*1024;
const TICKS:u64=10_000_000;
fn bad(message:&str)->String{format!("Unsupported/incomplete recording MKV: {message}")}
fn checked(value:Option<u64>)->Result<u64>{value.ok_or_else(||bad("byte/time arithmetic overflow"))}
fn uint(data:&[u8])->Result<u64>{if data.is_empty()||data.len()>8{return Err(bad("integer width"));}Ok(data.iter().fold(0,|n,b|(n<<8)|u64::from(*b)))}
fn float(data:&[u8])->Result<f64>{let v=match data.len(){4=>f64::from(f32::from_be_bytes(data.try_into().map_err(|_|bad("float"))?)),8=>f64::from_be_bytes(data.try_into().map_err(|_|bad("float"))?),_=>return Err(bad("float width"))};if !v.is_finite(){return Err(bad("nonfinite float"));}Ok(v)}
fn vint(data:&[u8],id:bool)->Result<(u64,usize)>{let first=*data.first().ok_or_else(||bad("truncated EBML header"))?;let n=first.leading_zeros() as usize+1;if n>if id{4}else{8}||data.len()<n{return Err(bad("EBML width/truncation"));}let mut value=if id{u64::from(first)}else{u64::from(first&((1u8<<(8-n))-1))};for b in &data[1..n]{value=(value<<8)|u64::from(*b);}if !id&&value==(1u64<<(7*n))-1{return Err(bad("unknown-size element is not finalized"));}Ok((value,n))}
#[derive(Clone,Copy)]struct Element<'a>{id:u64,data:&'a[u8]}
/// Header/master fields only; bounded at admission, closed allowlist at each level.
fn fields<'a>(mut bytes:&'a[u8],allowed:&[u64])->Result<Vec<Element<'a>>>{
    if bytes.len()>65536{return Err(bad("metadata master exceeds64KiB"));}let mut out=Vec::with_capacity(16);
    while !bytes.is_empty(){if out.len()>=64{return Err(bad("master field count exceeds64"));}let(id,a)=vint(bytes,true)?;let(size,b)=vint(&bytes[a..],false)?;let n=usize::try_from(size).map_err(|_|bad("field size"))?;let start=a+b;let end=start.checked_add(n).filter(|n|*n<=bytes.len()).ok_or_else(||bad("field exceeds parent"))?;
        if !allowed.contains(&id)||out.iter().any(|e:&Element<'_>|e.id==id){return Err(bad("unknown or duplicate semantic field"));}out.push(Element{id,data:&bytes[start..end]});bytes=&bytes[end..];
    }Ok(out)
}
fn field<'a>(f:&[Element<'a>],id:u64)->Result<&'a[u8]>{f.iter().find(|e|e.id==id).map(|e|e.data).ok_or_else(||bad("required profile field absent"))}
fn optional<'a>(f:&[Element<'a>],id:u64)->Option<&'a[u8]>{f.iter().find(|e|e.id==id).map(|e|e.data)}
fn equal(f:&[Element<'_>],id:u64,value:u64)->Result<()>{if uint(field(f,id)?)?!=value{return Err(bad("profile integer differs"));}Ok(())}
fn header<R:Read+Seek>(input:&mut R,parent:u64)->Result<(u64,u64,u64)>{
    let start=input.stream_position().map_err(|e|e.to_string())?;if start>=parent{return Err(bad("unexpected end of parent"));}let mut h=[0u8;12];input.read_exact(&mut h[..1]).map_err(|e|e.to_string())?;let ni=h[0].leading_zeros() as usize+1;if ni>4||start+ni as u64>=parent{return Err(bad("ID width/parent"));}input.read_exact(&mut h[1..ni+1]).map_err(|e|e.to_string())?;let ns=h[ni].leading_zeros() as usize+1;if ns>8||start+(ni+ns) as u64>parent{return Err(bad("size width/parent"));}input.read_exact(&mut h[ni+1..ni+ns]).map_err(|e|e.to_string())?;let(id,_)=vint(&h[..ni],true)?;let(size,_)=vint(&h[ni..ni+ns],false)?;let at=start+(ni+ns) as u64;let end=checked(at.checked_add(size))?;if end>parent{return Err(bad("child exceeds parent/file extent"));}Ok((id,at,end))
}
fn read_small<R:Read>(input:&mut R,n:u64,cap:usize)->Result<Vec<u8>>{let n=usize::try_from(n).ok().filter(|n|*n<=cap).ok_or_else(||bad("bounded allocation limit"))?;let mut b=Vec::new();b.try_reserve_exact(n).map_err(|e|e.to_string())?;b.resize(n,0);input.read_exact(&mut b).map_err(|e|e.to_string())?;Ok(b)}
pub(super) struct Audio{pub asc:Vec<u8>,pub rate:u32,pub channels:u16,pub delay_ns:u64}
pub(super) struct Profile{pub avcc:AvcConfig,pub width:u32,pub height:u32,pub audio:Option<Audio>,pub duration:u64,pub segment_end:u64,pub segment_start:u64,pub data_at:u64,avc:AvcSyntax}
pub(super) fn open<R:Read+Seek>(input:&mut R,length:u64)->Result<Profile>{
    input.seek(SeekFrom::Start(0)).map_err(|e|e.to_string())?;let(id,at,end)=header(input,length)?;if id!=0x1a45dfa3{return Err(bad("EBML header missing"));}let bytes=read_small(input,end-at,65536)?;let f=fields(&bytes,&[0x4286,0x42f7,0x42f2,0x42f3,0x4282,0x4287,0x4285])?;
    for(id,value)in[(0x4286,1),(0x42f7,1),(0x42f2,4),(0x42f3,8),(0x4287,4),(0x4285,4)]{equal(&f,id,value)?;}if field(&f,0x4282)?!=b"matroska"{return Err(bad("DocType"));}
    let(id,at,segment_end)=header(input,length)?;if id!=0x18538067||segment_end!=length{return Err(bad("finalized single Segment required"));}
    let mut duration=None;let mut track_bytes=None;let mut children=0;
    while input.stream_position().map_err(|e|e.to_string())?<segment_end{children+=1;if children>8{return Err(bad("too many pre-cluster elements"));}let start=input.stream_position().map_err(|e|e.to_string())?;let(id,data,end)=header(input,segment_end)?;
        match id{
            0xec=>{if end-data>512{return Err(bad("oversized reserved Void"));}input.seek(SeekFrom::Start(end)).map_err(|e|e.to_string())?;},
            0x114d9b74=>{let bytes=read_small(input,end-data,512)?;validate_seek(&bytes,segment_end-at)?;},
            0x1549a966=>{if duration.is_some(){return Err(bad("duplicate Info"));}let bytes=read_small(input,end-data,65536)?;let f=fields(&bytes,&[0x2ad7b1,0x4d80,0x5741,0x4489])?;equal(&f,0x2ad7b1,100)?;if field(&f,0x4d80)?!=b"SkyLive Rust"||field(&f,0x5741)?!=b"SkyLive"{return Err(bad("only SkyLive recording profile is admitted"));}let n=float(field(&f,0x4489)?)?;if n<=0.0||n>86400.0*TICKS as f64||n.fract()!=0.0{return Err(bad("exact finalized100ns Duration/24h"));}duration=Some(n as u64);},
            0x1654ae6b=>{if track_bytes.is_some(){return Err(bad("duplicate Tracks"));}track_bytes=Some(read_small(input,end-data,65536)?);},
            0x1f43b675=>{input.seek(SeekFrom::Start(start)).map_err(|e|e.to_string())?;break;},
            _=>return Err(bad("unsupported pre-cluster element")),
        }
    }
    let bytes=track_bytes.ok_or_else(||bad("Tracks missing"))?;let mut rest=bytes.as_slice();let mut entries=Vec::with_capacity(2);
    while !rest.is_empty(){if entries.len()>=2{return Err(bad("more than two tracks"));}let(id,a)=vint(rest,true)?;let(n,b)=vint(&rest[a..],false)?;let end=(a+b).checked_add(n as usize).filter(|n|*n<=rest.len()).ok_or_else(||bad("TrackEntry extent"))?;if id!=0xae{return Err(bad("unknown track element"));}entries.push(&rest[a+b..end]);rest=&rest[end..];}
    let mut video=None;let mut audio=None;
    for entry in entries{let f=fields(entry,&[0xd7,0x73c5,0x83,0x9c,0x86,0x63a2,0xe0,0xe1,0x56aa])?;equal(&f,0x9c,0)?;let kind=uint(field(&f,0x83)?)?;
        match kind{
            1=>{if video.is_some()||optional(&f,0xe1).is_some()||optional(&f,0x56aa).is_some(){return Err(bad("video track profile/duplicate"));}equal(&f,0xd7,1)?;equal(&f,0x73c5,1)?;if field(&f,0x86)?!=b"V_MPEG4/ISO/AVC"{return Err(bad("video CodecID"));}let private=field(&f,0x63a2)?;if private.len()<7||private.len()>32768||private[0]!=1{return Err(bad("avcC extent/version"));}let avcc=AvcConfig::parse(private).map_err(|e|e.to_string())?;let v=fields(field(&f,0xe0)?,&[0xb0,0xba,0x9a,0x55b0])?;equal(&v,0x9a,2)?;let width=u32::try_from(uint(field(&v,0xb0)?)?).map_err(|_|bad("width"))?;let height=u32::try_from(uint(field(&v,0xba)?)?).map_err(|_|bad("height"))?;if width<16||height<16||width>4096||height>4096||width%2!=0||height%2!=0||u64::from(width)*u64::from(height)>8_847_360{return Err(bad("dimensions"));}let c=fields(field(&v,0x55b0)?,&[0x55b1,0x55b2,0x55b9,0x55ba,0x55bb])?;for(k,n)in[(0x55b1,1),(0x55b2,8),(0x55b9,1),(0x55ba,1),(0x55bb,1)]{equal(&c,k,n)?;}let syntax=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||AvcSyntax::new(&avcc,width,height))).map_err(|payload|{let detail=payload.downcast_ref::<String>().map(String::as_str).or_else(||payload.downcast_ref::<&str>().copied()).unwrap_or("non-string panic payload");eprintln!("Untrusted remux AVC parameter syntax: {detail}");bad("AVC parameter arithmetic/parser failure; exact details are in host log")})??;video=Some((avcc,width,height,syntax));},
            2=>{if audio.is_some()||optional(&f,0xe0).is_some(){return Err(bad("audio track profile/duplicate"));}equal(&f,0xd7,2)?;equal(&f,0x73c5,2)?;if field(&f,0x86)?!=b"A_AAC"{return Err(bad("audio CodecID"));}let a=fields(field(&f,0xe1)?,&[0xb5,0x9f,0x6264])?;equal(&a,0x6264,16)?;let rate=float(field(&a,0xb5)?)?;if rate!=44100.0&&rate!=48000.0{return Err(bad("AAC sample rate"));}let rate=rate as u32;let channels=u16::try_from(uint(field(&a,0x9f)?)?).map_err(|_|bad("AAC channels"))?;if !(1..=2).contains(&channels){return Err(bad("AAC mono/stereo only"));}let asc=field(&f,0x63a2)?;let expected=if rate==48000{[0x11,0x80|((channels as u8)<<3)]}else{[0x12,(channels as u8)<<3]};if asc!=expected{return Err(bad("AAC-LC ASC differs from declared format"));}let delay_ns=uint(field(&f,0x56aa)?)?;if delay_ns%100!=0||delay_ns>(3072u64*1_000_000_000).div_ceil(u64::from(rate))+100{return Err(bad("AAC actual CodecDelay profile"));}audio=Some(Audio{asc:asc.to_vec(),rate,channels,delay_ns});},
            _=>return Err(bad("unsupported track type")),
        }
    }
    let(avcc,width,height,avc)=video.ok_or_else(||bad("AVC track missing"))?;Ok(Profile{avcc,width,height,audio,duration:duration.ok_or_else(||bad("Info missing"))?,segment_end,segment_start:at,data_at:input.stream_position().map_err(|e|e.to_string())?,avc})
}
fn validate_seek(mut data:&[u8],segment_bytes:u64)->Result<()>{let mut count=0;while !data.is_empty(){count+=1;if count>3{return Err(bad("SeekHead count"));}let(id,a)=vint(data,true)?;let(n,b)=vint(&data[a..],false)?;let end=(a+b).checked_add(n as usize).filter(|n|*n<=data.len()).ok_or_else(||bad("Seek extent"))?;if id!=0x4dbb{return Err(bad("SeekHead entry"));}let f=fields(&data[a+b..end],&[0x53ab,0x53ac])?;let target=uint(field(&f,0x53ab)?)?;if ![0x1549a966,0x1654ae6b,0x1c53bb6b].contains(&target)||uint(field(&f,0x53ac)?)?>=segment_bytes{return Err(bad("Seek target/range"));}data=&data[end..];}Ok(())}
struct AvcSyntax{sps:Sps,pps:Pps,sps_bytes:Vec<u8>,pps_bytes:Vec<u8>,last_poc:Option<i64>,previous_lsb:u32,msb:i64,last_frame:Option<u32>,frame_offset:u64}
impl AvcSyntax{
    fn new(c:&AvcConfig,width:u32,height:u32)->Result<Self>{
        if !matches!(c.profile,66|100)||c.length_size!=4||c.sps.len()!=1||c.pps.len()!=1||c.sps[0].len()>16384||c.pps[0].len()>16384||c.ext.len()>4{return Err(bad("self-contained single-SPS/PPS AVC8-bit profile"));}
        let sps_bytes=c.sps[0].clone();let pps_bytes=c.pps[0].clone();if sps_bytes.first().is_none_or(|b|b&0x9f!=7)||pps_bytes.first().is_none_or(|b|b&0x9f!=8){return Err(bad("parameter NAL types"));}
        let sps=Sps::parse(&unescape_rbsp(&sps_bytes[1..])).map_err(|e|e.to_string())?;
        if sps.profile_idc!=c.profile||sps.bit_depth_luma!=8||sps.bit_depth_chroma!=8||sps.chroma_format_idc!=1||sps.separate_colour_plane||!sps.frame_mbs_only||sps.pic_width_in_mbs>256||sps.pic_height_in_map_units>256||!matches!(sps.pic_order_cnt_type,0|2){return Err(bad("AVC progressive8-bit420/POC0-or2 profile"));}
        let mut w=sps.pic_width_in_mbs.checked_mul(16).ok_or_else(||bad("SPS width overflow"))?;let mut h=sps.pic_height_in_map_units.checked_mul(16).ok_or_else(||bad("SPS height overflow"))?;if let Some((l,r,t,b))=sps.frame_crop{w=w.checked_sub(l.checked_add(r).and_then(|n|n.checked_mul(2)).ok_or_else(||bad("crop overflow"))?).ok_or_else(||bad("crop width"))?;h=h.checked_sub(t.checked_add(b).and_then(|n|n.checked_mul(2)).ok_or_else(||bad("crop overflow"))?).ok_or_else(||bad("crop height"))?;}if (w,h)!=(width,height){return Err(bad("SPS and track dimensions differ"));}
        if sps.vui.as_ref().is_some_and(|v|v.full_range||![1,2].contains(&v.colour_primaries)||![1,2].contains(&v.transfer_characteristics)||![1,2].contains(&v.matrix_coefficients)||v.sar!=(0,0)&&v.sar!=(1,1)){return Err(bad("HDR/non709/range/anamorphic SPS"));}
        let mut table:Vec<Option<Sps>>=(0..32).map(|_|None).collect();let index=sps.id as usize;if index>=32{return Err(bad("SPS identity"));}table[index]=Some(sps.clone());let pps_rbsp=unescape_rbsp(&pps_bytes[1..]);let mut prefix=skylive_codec_bits::BitReader::new(&pps_rbsp);prefix.read_ue().map_err(|e|e.to_string())?;prefix.read_ue().map_err(|e|e.to_string())?;prefix.read_flag().map_err(|e|e.to_string())?;prefix.read_flag().map_err(|e|e.to_string())?;if prefix.read_ue().map_err(|e|e.to_string())?!=0{return Err(bad("FMO map is unsupported"));}let pps=Pps::parse(&pps_rbsp,&table).map_err(|e|e.to_string())?;if pps.sps_id!=sps.id||pps.num_slice_groups!=1{return Err(bad("PPS reference/FMO"));}
        Ok(Self{sps,pps,sps_bytes,pps_bytes,last_poc:None,previous_lsb:0,msb:0,last_frame:None,frame_offset:0})
    }
    fn packet(&mut self,mut bytes:&[u8],key:bool)->Result<()>{
        let mut nal_count=0;let mut first=None::<SliceHeader>;let mut last_mb=None;
        while !bytes.is_empty(){nal_count+=1;if nal_count>4096||bytes.len()<4{return Err(bad("AU NAL count/length"));}let n=u32::from_be_bytes(bytes[..4].try_into().map_err(|_|bad("NAL length"))?) as usize;let nal=bytes.get(4..4usize.checked_add(n).ok_or_else(||bad("NAL overflow"))?).filter(|n|!n.is_empty()).ok_or_else(||bad("NAL exceeds AU"))?;bytes=&bytes[4+n..];let nh=NalHeader::parse(nal[0]).map_err(|e|e.to_string())?;
            match nh.nal_unit_type{
                1|5=>{let rbsp=unescape_rbsp(&nal[1..]);let(header,_,_)=SliceHeader::parse(&rbsp,nh,|id|if id==self.pps.id{Ok((&self.pps,&self.sps))}else{Err(skylive_h264::Error::Invalid("unknown remux PPS".into()))}).map_err(|e|e.to_string())?;
                    if !matches!(header.slice_type,SliceType::I|SliceType::P)||header.idr!=key||header.idr&&header.slice_type!=SliceType::I||header.field_pic||header.first_mb_in_slice>=self.sps.pic_width_in_mbs*self.sps.pic_height_in_map_units||header.idr&&header.nal_ref_idc==0||header.has_mmco5()||header.redundant_pic_cnt!=0{return Err(bad("B/SP/SI/reordered/field/redundant slice unsupported"));}
                    if let Some(previous)=&first{if header.frame_num!=previous.frame_num||header.pic_order_cnt_lsb!=previous.pic_order_cnt_lsb||header.idr_pic_id!=previous.idr_pic_id||header.delta_pic_order_cnt_bottom!=previous.delta_pic_order_cnt_bottom||header.nal_ref_idc!=previous.nal_ref_idc||last_mb.is_some_and(|n|header.first_mb_in_slice<=n){return Err(bad("multiple pictures or slice order within AU"));}}else{if header.first_mb_in_slice!=0{return Err(bad("AU first slice missing"));}first=Some(header.clone());}last_mb=Some(header.first_mb_in_slice);
                },
                7=>if nal!=self.sps_bytes{return Err(bad("in-band SPS change"));},8=>if nal!=self.pps_bytes{return Err(bad("in-band PPS change"));},
                6=>{let data=unescape_rbsp(&nal[1..]);let mut rest=data.as_slice();let mut count=0;while !rest.is_empty()&&rest!=[0x80]{count+=1;if count>64{return Err(bad("SEI count"));}let mut kind=0u64;let mut at=0;loop{let b=*rest.get(at).ok_or_else(||bad("SEI type"))?;at+=1;kind=checked(kind.checked_add(u64::from(b)))?;if b!=255{break;}}let mut size=0u64;loop{let b=*rest.get(at).ok_or_else(||bad("SEI size"))?;at+=1;size=checked(size.checked_add(u64::from(b)))?;if b!=255{break;}}if ![0,1,5,6].contains(&kind){return Err(bad("unsupported/HDR SEI metadata"));}let end=at.checked_add(size as usize).filter(|n|*n<=rest.len()).ok_or_else(||bad("SEI extent"))?;rest=&rest[end..];}},
                9|12=>{},_=>return Err(bad("unsupported AVC NAL kind")),
            }
        }
        let h=first.ok_or_else(||bad("AU contains no I/P picture"))?;
        let poc=if self.sps.pic_order_cnt_type==0{let modulus=1i64<<self.sps.log2_max_poc_lsb;let lsb=i64::from(h.pic_order_cnt_lsb);let prev=i64::from(self.previous_lsb);let msb=if h.idr{0}else if lsb<prev&&prev-lsb>=modulus/2{self.msb+modulus}else if lsb>prev&&lsb-prev>modulus/2{self.msb-modulus}else{self.msb};if h.nal_ref_idc!=0{self.previous_lsb=h.pic_order_cnt_lsb;self.msb=msb;}msb+lsb+i64::from(h.delta_pic_order_cnt_bottom.min(0))}else{if h.idr{self.frame_offset=0;}else if self.last_frame.is_some_and(|v|h.frame_num<v){self.frame_offset+=u64::from(self.sps.max_frame_num());}self.last_frame=Some(h.frame_num);i64::try_from(2*(self.frame_offset+u64::from(h.frame_num))).map_err(|_|bad("POC overflow"))?-i64::from(h.nal_ref_idc==0)};
        if !h.idr&&self.last_poc.is_some_and(|previous|poc<=previous){return Err(bad("actual picture reordering is unsupported"));}if self.last_poc.is_none()&&!h.idr{return Err(bad("video must start with IDR"));}self.last_poc=Some(poc);Ok(())
    }
}
struct Sample{bytes:Vec<u8>,at:u64,duration:u64,key:bool,padding:u64}
pub(super) struct Summary{pub packets:u64,pub video:u64,pub audio:u64,pub audio_valid_end_ns:Option<u64>}
pub(super) fn transfer<R:Read+Seek,W:Read+std::io::Write+Seek,S:Read+std::io::Write+Seek>(input:&mut R,p:&mut Profile,mux:&mut SpoolMp4Writer<W,S>,mut progress:impl FnMut(u64,u64)->Result<()>)->Result<Summary>{
    input.seek(SeekFrom::Start(p.data_at)).map_err(|e|e.to_string())?;let mut pending:[Option<Sample>;2]=[None,None];let mut first=[None,None];let mut last=[None,None];let mut counts=[0u64;2];let mut total=0;let mut children=0;let mut last_global=None;let mut saw_cues=false;let mut idrs=0u64;
    while input.stream_position().map_err(|e|e.to_string())?<p.segment_end{
        children+=1;if children>MAX_PACKETS+8{return Err(bad("Segment child work limit"));}let(id,data,end)=header(input,p.segment_end)?;
        if id==0x1c53bb6b{if saw_cues||total==0{return Err(bad("Cues order/duplicate"));}saw_cues=true;validate_cues(input,end,idrs,p.segment_end-p.segment_start,&mut progress)?;continue;}
        if id!=0x1f43b675||saw_cues{return Err(bad("unknown/extra/nonfinal Segment child"));}let bytes=read_small(input,end-data,AU+256)?;let(time,sample,index)=cluster(&bytes)?;
        if time>p.duration+20_000_000||last_global.is_some_and(|v|time<v){return Err(bad("cluster timestamp order/duration"));}last_global=Some(time);
        if index==1&&p.audio.is_none(){return Err(bad("undeclared audio track"));}total+=1;counts[index]+=1;if total>MAX_PACKETS||counts[0]>10_368_008{return Err(bad("packet count/work limit"));}
        if last[index].is_some_and(|v|time<=v){return Err(bad("per-track timestamps overlap/backtrack"));}
        if index==0{p.avc.packet(&sample.bytes,sample.key)?;if sample.key{idrs+=1;}}
        first[index].get_or_insert(time);
        if let Some(mut previous)=pending[index].take(){let interval=time-previous.at;if index==0{if interval<83_333{return Err(bad("video exceeds120FPS admission"));}previous.duration=interval;}else{let a=p.audio.as_ref().ok_or_else(||bad("audio profile"))?;if previous.padding!=0||previous.duration!=interval||(i128::from(interval)*i128::from(a.rate)-1024i128*i128::from(TICKS)).abs()>i128::from(a.rate){return Err(bad("AAC native1024-frame spacing/tail order"));}}write(mux,index,&previous)?;}
        last[index]=Some(time);pending[index]=Some(sample);progress(end,p.segment_end)?;
    }
    if !saw_cues||counts[0]==0||p.audio.is_some()&&counts[1]==0{return Err(bad("missing final Cues or declared track packets"));}
    let first_v=first[0].ok_or_else(||bad("video origin"))?;let mut v=pending[0].take().ok_or_else(||bad("last video missing"))?;v.duration=p.duration.checked_sub(v.at).filter(|n|*n>0).ok_or_else(||bad("last video presentation interval"))?;write(mux,0,&v)?;mux.set_recovery_origin(0,i64::try_from(first_v).map_err(|_|bad("video origin overflow"))?).map_err(|e|e.to_string())?;mux.set_track_edits(0,edits(i128::from(first_v)*100,p.duration*100,10_000_000)?).map_err(|e|e.to_string())?;
    let mut audio_valid_end_ns=None;
    if let Some(a)=&p.audio{let mut sample=pending[1].take().ok_or_else(||bad("last AAC missing"))?;let origin=i128::from(first[1].ok_or_else(||bad("audio origin"))?)*100-i128::from(a.delay_ns);let last_at=i128::from(sample.at)*100-i128::from(a.delay_ns);let coded=(1024u64*1_000_000_000).div_ceil(u64::from(a.rate));if sample.padding>coded{return Err(bad("AAC discard padding exceeds one coded frame"));}let coded_end=u64::try_from(last_at+i128::from(coded)).map_err(|_|bad("AAC coded end"))?;
        let valid=if sample.padding!=0{coded_end.checked_sub(sample.padding).ok_or_else(||bad("AAC padding cut underflow"))?}else{
            // Our writer stores max(coded_end-valid_PCM_end,0). A zero value
            // may omit a sub100ns native-rounding remainder. The source writer
            // coverage check permits native_end+one100ns tick; its floor PCM
            // anchor then adds <=99ns. Intersect that interval with Duration,
            // and admit only ONE exact integer PCM sample-grid endpoint.
            let native_end=last_at+i128::from(sample.duration)*100;
            let upper=u64::try_from(native_end+199).map_err(|_|bad("AAC native end bound"))?.min(p.duration*100+99);
            let candidate=sample_grid(coded_end,a.rate)?;let frame=(u128::from(candidate)*u128::from(a.rate)).div_ceil(1_000_000_000);let next=u64::try_from((frame+1)*1_000_000_000/u128::from(a.rate)).map_err(|_|bad("AAC sample grid overflow"))?;
            if candidate>upper||next<=upper{return Err(bad("AAC zero-padding endpoint is not uniquely recoverable on source PCM grid"));}candidate
        };if sample_grid(valid,a.rate)?!=valid||valid>p.duration*100+99||valid==0{return Err(bad("AAC valid PCM cut is outside exact source sample grid/Duration"));}
        // Last AU decodes1024 frames even if native BlockDuration is shortened.
        // Extend only that coded table span, then trim at exact ns DiscardPadding.
        // Earlier native timestamps and every nonfinal duration remain unchanged.
        sample.duration=sample.duration.checked_mul(100).ok_or_else(||bad("AAC duration overflow"))?.max(coded);write_ns_audio(mux,&sample)?;mux.set_recovery_origin(1,i64::try_from(origin).map_err(|_|bad("AAC origin overflow"))?).map_err(|e|e.to_string())?;mux.set_track_edits(1,edits(origin,valid,1_000_000_000)?).map_err(|e|e.to_string())?;audio_valid_end_ns=Some(valid);
    }
    Ok(Summary{packets:total,video:counts[0],audio:counts[1],audio_valid_end_ns})
}
fn sample_grid(ns:u64,rate:u32)->Result<u64>{let frame=(u128::from(ns)*u128::from(rate)).div_ceil(1_000_000_000);u64::try_from(frame*1_000_000_000/u128::from(rate)).map_err(|_|bad("AAC PCM grid overflow"))}
fn write<W:Read+std::io::Write+Seek,S:Read+std::io::Write+Seek>(mux:&mut SpoolMp4Writer<W,S>,track:usize,s:&Sample)->Result<()>{let duration=if track==1{checked(s.duration.checked_mul(100))?}else{s.duration};let duration=u32::try_from(duration).ok().filter(|v|*v>0).ok_or_else(||bad("sample interval exceeds MP4 timing profile"))?;mux.write_sample(track,WriteSample{data:&s.bytes,duration,composition_offset:0,is_sync:s.key}).map_err(|e|e.to_string())}
fn write_ns_audio<W:Read+std::io::Write+Seek,S:Read+std::io::Write+Seek>(mux:&mut SpoolMp4Writer<W,S>,s:&Sample)->Result<()>{let duration=u32::try_from(s.duration).ok().filter(|v|*v>0).ok_or_else(||bad("AAC final coded interval"))?;mux.write_sample(1,WriteSample{data:&s.bytes,duration,composition_offset:0,is_sync:true}).map_err(|e|e.to_string())}
fn edits(first_ns:i128,end_ns:u64,timescale:u32)->Result<Vec<Edit>>{let make=|n,at|Edit{segment_duration:n,media_time:at,media_rate:0x10000};if first_ns>=0{let first=u64::try_from(first_ns).map_err(|_|bad("track start overflow"))?;if first>=end_ns{return Err(bad("track has no positive presentation"));}if first==0{Ok(vec![make(end_ns,0)])}else{Ok(vec![make(first,-1),make(end_ns-first,0)])}}else{let skip=(-first_ns)*i128::from(timescale)/1_000_000_000;if (-first_ns)*i128::from(timescale)%1_000_000_000!=0{return Err(bad("priming is not representable in media timebase"));}Ok(vec![make(end_ns,i64::try_from(skip).map_err(|_|bad("priming edit overflow"))?)])}}
fn cluster(bytes:&[u8])->Result<(u64,Sample,usize)>{
    // Payload may be2MiB: parse only the fixed cluster envelope without using
    // the metadata fields allocator/cap. One block/one timestamp, no nesting loop.
    let(id,a)=vint(bytes,true)?;let(n,b)=vint(&bytes[a..],false)?;let split=(a+b).checked_add(n as usize).filter(|n|*n<=bytes.len()).ok_or_else(||bad("cluster timestamp extent"))?;if id!=0xe7{return Err(bad("cluster timestamp must be first"));}let at=uint(&bytes[a+b..split])?;let rest=&bytes[split..];let(id,c)=vint(rest,true)?;let(n,d)=vint(&rest[c..],false)?;let start=c+d;if checked((start as u64).checked_add(n))?!=rest.len() as u64{return Err(bad("one block per cluster required"));}let(group,index)=match id{0xa3=>(false,0),0xa0=>(true,1),_=>return Err(bad("unsupported cluster child"))};let mut payload=&rest[start..];let mut duration=0;let mut padding=0;
    if group{let(kind,a)=vint(payload,true)?;let(size,b)=vint(&payload[a..],false)?;if kind!=0xa1||size<5||size>AU as u64+4{return Err(bad("audio Block shape"));}let end=(a+b).checked_add(size as usize).filter(|n|*n<=payload.len()).ok_or_else(||bad("Block exceeds BlockGroup"))?;let tail=fields(&payload[end..],&[0x9b,0x75a2])?;duration=uint(field(&tail,0x9b)?)?;if duration==0||duration>(1024u64*TICKS).div_ceil(44100)+1{return Err(bad("AAC native BlockDuration profile"));}if let Some(value)=optional(&tail,0x75a2){if value.is_empty()||value.len()>8||value[0]&0x80!=0{return Err(bad("negative DiscardPadding is unsupported"));}padding=uint(value)?;}payload=&payload[a+b..end];}
    if payload.len()<5||payload.len()>AU+4||payload[0]!=if index==0{0x81}else{0x82}||payload[1]!=0||payload[2]!=0||!matches!(payload[3],0|0x80)||index==1&&payload[3]!=0{return Err(bad("track VINT/relative time/block flags/lacing"));}let key=index==1||payload[3]==0x80;Ok((at,Sample{bytes:payload[4..].to_vec(),at,duration,key,padding},index))
}
fn validate_cues<R:Read+Seek>(input:&mut R,end:u64,idrs:u64,segment_bytes:u64,progress:&mut impl FnMut(u64,u64)->Result<()>)->Result<()>{let mut count=0;let mut last=None;while input.stream_position().map_err(|e|e.to_string())?<end{count+=1;if count>idrs||count>10_368_008{return Err(bad("Cues count/work bound"));}let(kind,at,next)=header(input,end)?;if kind!=0xbb{return Err(bad("CuePoint kind"));}let bytes=read_small(input,next-at,128)?;let f=fields(&bytes,&[0xb3,0xb7])?;let time=uint(field(&f,0xb3)?)?;let pos=fields(field(&f,0xb7)?,&[0xf7,0xf1,0xf0])?;equal(&pos,0xf7,1)?;equal(&pos,0xf0,10)?;if uint(field(&pos,0xf1)?)?>=segment_bytes||last.is_some_and(|v|time<=v){return Err(bad("Cue timestamp/position"));}last=Some(time);if count%1024==0{progress(next,end)?;}}
    if count!=idrs{return Err(bad("Cue/actual IDR count mismatch"));}Ok(())}
