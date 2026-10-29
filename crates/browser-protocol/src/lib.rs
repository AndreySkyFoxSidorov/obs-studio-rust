//! Fixed, bounded IPC between the Rust source owner and the pinned CEF helper.
//! Engine ABI is checked separately; this protocol never contains OS handles.
use std::io::{self,Read,Write};
pub const VERSION:u32=3;
pub const CEF_HASH:&str="2e66390b5f0fa21ab4667b503e76164037cd674e";
pub const CEF_COMMIT:&str="2b7d20b8cdcba6e4e9507a1015b8262515a196c6";
pub const MAX_SOURCES:usize=4;
pub const MAX_CONTROL:usize=384*1024;
pub const MAX_PIXELS:usize=8_847_360;
pub const MAX_FRAME:usize=MAX_PIXELS*4;
pub const PIXEL_BUDGET:usize=128*1024*1024;
pub const MAX_FRAMES:usize=16;
pub const PCM_BUDGET:usize=4*1024*1024;
pub const MAX_PCM_PACKETS:usize=64;
pub const MAX_PCM_FRAMES:usize=4096;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub struct AudioGrant{pub session:u64,pub clock_epoch:u64,pub generation:u64,pub serial:u64,pub start_qpc_ns:u64}
const MAGIC:[u8;8]=*b"SLCEF3\0\0";
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Source {pub id:u64,pub lease:u64,pub width:u32,pub height:u32,pub fps:u32,pub control_audio:bool,pub native_audio:bool,pub native_serial:u64,pub audio_grant:Option<AudioGrant>,pub visible:bool,pub active:bool,pub url:String,pub css:String}
impl Source {
 pub fn validate(&self)->io::Result<()> {
  frame_bytes(self.width,self.height)?;
  if self.id==0||self.lease==0||self.native_serial==0||self.control_audio&&self.native_audio||!(1..=60).contains(&self.fps)||self.url.is_empty()||self.url.len()>16384||self.url.chars().any(char::is_control)||self.css.len()>65536||self.css.contains('\0')||self.audio_grant.is_some_and(|g|!self.control_audio||g.session==0||g.clock_epoch==0||g.generation==0||g.serial==0){return Err(invalid("invalid browser source settings"));}Ok(())
 }
}
#[derive(Clone,Debug)]
pub enum Input {Move{x:i32,y:i32,modifiers:u32,leave:bool},Click{x:i32,y:i32,modifiers:u32,button:i32,up:bool,count:i32},Wheel{x:i32,y:i32,modifiers:u32,dx:i32,dy:i32},Key{kind:i32,modifiers:u32,virtual_key:i32,native_key:i32,character:u16},Focus(bool)}
#[derive(Clone,Debug)]
pub enum Command {Boot,Sync {session:u64,epoch:u64,sources:Vec<Source>},Input {session:u64,source:u64,lease:u64,event:Input},Shutdown}
#[derive(Debug)]
pub enum Event {Hello,NativeAudio {session:u64,source:u64,lease:u64,serial:u64,muted:bool},Audio{session:u64,source:u64,lease:u64,grant:AudioGrant,stream:u64,sequence:u64,pts_ms:i64,qpc_ns:u64,unix_ns:u64,dropped:u64,frames:u32,samples:Vec<f32>},AudioFormat{session:u64,source:u64,lease:u64,stream:u64,rate:u32,channels:u32,layout:i32,frames:u32},SkippedAudio,Paint {session:u64,source:u64,lease:u64,width:u32,height:u32,qpc_ns:u64,pixels:Vec<u8>},SkippedPaint,State {session:u64,source:u64,lease:u64,kind:u32,message:String},Stopped}
pub fn frame_bytes(width:u32,height:u32)->io::Result<usize>{let n=(width as usize).checked_mul(height as usize).filter(|&n|n<=MAX_PIXELS).and_then(|n|n.checked_mul(4)).filter(|_|width>0&&height>0&&width<=4096&&height<=4096).ok_or_else(||invalid("browser viewport exceeds4096-edge/8.8Mpixels"))?;Ok(n)}
pub fn invalid(s:&'static str)->io::Error{io::Error::new(io::ErrorKind::InvalidData,s)}
fn put_u32(v:&mut Vec<u8>,x:u32){v.extend_from_slice(&x.to_le_bytes());}fn put_u64(v:&mut Vec<u8>,x:u64){v.extend_from_slice(&x.to_le_bytes());}fn put_i32(v:&mut Vec<u8>,x:i32){put_u32(v,x as u32);}
fn put_grant(b:&mut Vec<u8>,g:AudioGrant){for v in [g.session,g.clock_epoch,g.generation,g.serial,g.start_qpc_ns]{put_u64(b,v);}}
fn get_grant(d:&mut Decoder<'_>)->io::Result<AudioGrant>{Ok(AudioGrant{session:d.u64()?,clock_epoch:d.u64()?,generation:d.u64()?,serial:d.u64()?,start_qpc_ns:d.u64()?})}
fn put_str(v:&mut Vec<u8>,s:&str){put_u32(v,s.len() as u32);v.extend_from_slice(s.as_bytes());}
fn header(w:&mut impl Write,kind:u8,len:usize)->io::Result<()> {if len>MAX_FRAME+128{return Err(invalid("browser IPC frame length"));}w.write_all(&MAGIC)?;w.write_all(&[kind])?;w.write_all(&(len as u32).to_le_bytes())}
fn read_header(r:&mut impl Read)->io::Result<(u8,usize)> {let mut b=[0u8;13];r.read_exact(&mut b)?;if b[..8]!=MAGIC{return Err(invalid("browser helper protocol mismatch"));}Ok((b[8],u32::from_le_bytes(b[9..13].try_into().map_err(|_|invalid("browser header"))?) as usize))}
struct Decoder<'a>{b:&'a[u8],p:usize}
impl<'a> Decoder<'a>{fn take(&mut self,n:usize)->io::Result<&'a[u8]>{let end=self.p.checked_add(n).filter(|&n|n<=self.b.len()).ok_or_else(||invalid("truncated browser IPC"))?;let b=&self.b[self.p..end];self.p=end;Ok(b)}fn u32(&mut self)->io::Result<u32>{Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_|invalid("u32"))?))}fn i32(&mut self)->io::Result<i32>{Ok(self.u32()? as i32)}fn u64(&mut self)->io::Result<u64>{Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_|invalid("u64"))?))}fn boolean(&mut self)->io::Result<bool>{match self.u32()?{0=>Ok(false),1=>Ok(true),_=>Err(invalid("browser boolean"))}}fn string(&mut self,cap:usize)->io::Result<String>{let n=self.u32()? as usize;if n>cap{return Err(invalid("browser IPC string bound"));}String::from_utf8(self.take(n)?.to_vec()).map_err(|_|invalid("browser IPC UTF8"))}fn done(self)->io::Result<()>{if self.p==self.b.len(){Ok(())}else{Err(invalid("browser IPC trailing bytes"))}}}
pub fn write_command(w:&mut impl Write,c:&Command)->io::Result<()> {
 let mut b=Vec::new();let kind=match c {
  Command::Boot=>0,
  Command::Sync {session,epoch,sources}=>{if *session==0||*epoch==0||sources.len()>MAX_SOURCES{return Err(invalid("browser source count/scope"));}put_u64(&mut b,*session);put_u64(&mut b,*epoch);put_u32(&mut b,sources.len() as u32);for s in sources {s.validate()?;put_u64(&mut b,s.id);put_u64(&mut b,s.lease);for v in [s.width,s.height,s.fps,s.control_audio as u32,s.native_audio as u32,s.visible as u32,s.active as u32]{put_u32(&mut b,v);}if s.native_serial!=*epoch{return Err(invalid("browser native playback serial"));}put_u64(&mut b,s.native_serial);put_u32(&mut b,s.audio_grant.is_some()as u32);if let Some(g)=s.audio_grant{put_grant(&mut b,g);}put_str(&mut b,&s.url);put_str(&mut b,&s.css);}1},
  Command::Input {session,source,lease,event}=>{for v in [*session,*source,*lease]{put_u64(&mut b,v);}match event {
   Input::Move{x,y,modifiers,leave}=>{put_u32(&mut b,1);put_i32(&mut b,*x);put_i32(&mut b,*y);put_u32(&mut b,*modifiers);put_u32(&mut b,*leave as u32);},
   Input::Click{x,y,modifiers,button,up,count}=>{put_u32(&mut b,2);for v in [*x,*y,*modifiers as i32,*button,*up as i32,*count]{put_i32(&mut b,v);}},
   Input::Wheel{x,y,modifiers,dx,dy}=>{put_u32(&mut b,3);for v in [*x,*y,*modifiers as i32,*dx,*dy]{put_i32(&mut b,v);}},
   Input::Key{kind,modifiers,virtual_key,native_key,character}=>{put_u32(&mut b,4);for v in [*kind,*modifiers as i32,*virtual_key,*native_key,i32::from(*character)]{put_i32(&mut b,v);}},
   Input::Focus(f)=>{put_u32(&mut b,5);put_u32(&mut b,*f as u32);}
  }2},Command::Shutdown=>3};
 if b.len()>MAX_CONTROL{return Err(invalid("browser command bytes"));}header(w,kind,b.len())?;w.write_all(&b)?;w.flush()
}
pub fn read_command(r:&mut impl Read)->io::Result<Command>{let (kind,n)=read_header(r)?;if n>MAX_CONTROL{return Err(invalid("browser command bytes"));}let mut b=vec![0;n];r.read_exact(&mut b)?;let mut d=Decoder {b:&b,p:0};let command=match kind {
 0=>Command::Boot,
 1=>{let session=d.u64()?;let epoch=d.u64()?;let count=d.u32()? as usize;if session==0||epoch==0||count>MAX_SOURCES{return Err(invalid("browser sync scope/count"));}let mut sources=Vec::with_capacity(count);for _ in 0..count {let source=Source {id:d.u64()?,lease:d.u64()?,width:d.u32()?,height:d.u32()?,fps:d.u32()?,control_audio:d.boolean()?,native_audio:d.boolean()?,visible:d.boolean()?,active:d.boolean()?,native_serial:d.u64()?,audio_grant:if d.boolean()?{Some(get_grant(&mut d)?)}else{None},url:d.string(16384)?,css:d.string(65536)?};source.validate()?;if source.native_serial!=epoch{return Err(invalid("browser native playback serial"));}if sources.iter().any(|s:&Source|s.id==source.id){return Err(invalid("duplicate browser source"));}sources.push(source);}Command::Sync {session,epoch,sources}},
 2=>{let session=d.u64()?;let source=d.u64()?;let lease=d.u64()?;let event=match d.u32()? {1=>Input::Move{x:d.i32()?,y:d.i32()?,modifiers:d.u32()?,leave:d.boolean()?},2=>Input::Click{x:d.i32()?,y:d.i32()?,modifiers:d.u32()?,button:d.i32()?,up:d.boolean()?,count:d.i32()?},3=>Input::Wheel{x:d.i32()?,y:d.i32()?,modifiers:d.u32()?,dx:d.i32()?,dy:d.i32()?},4=>Input::Key{kind:d.i32()?,modifiers:d.u32()?,virtual_key:d.i32()?,native_key:d.i32()?,character:u16::try_from(d.u32()?).map_err(|_|invalid("browser character"))?},5=>Input::Focus(d.boolean()?),_=>return Err(invalid("browser input type"))};Command::Input {session,source,lease,event}},3=>Command::Shutdown,_=>return Err(invalid("browser command type"))};d.done()?;Ok(command)}
pub fn write_event(w:&mut impl Write,event:&Event)->io::Result<()> {
 let mut b=Vec::new();match event {
  Event::NativeAudio{session,source,lease,serial,muted}=>{for v in [*session,*source,*lease,*serial]{put_u64(&mut b,v);}put_u32(&mut b,*muted as u32);header(w,107,b.len())?;w.write_all(&b)?;},
  Event::Audio{session,source,lease,grant,stream,sequence,pts_ms,qpc_ns,unix_ns,dropped,frames,samples}=>{
   if *frames==0||*frames as usize>MAX_PCM_FRAMES||samples.len()!=*frames as usize*2||samples.iter().any(|s|!s.is_finite()){return Err(invalid("browser PCM shape/finite samples"));}
   for v in [*session,*source,*lease]{put_u64(&mut b,v);}put_grant(&mut b,*grant);for v in [*stream,*sequence,*pts_ms as u64,*qpc_ns,*unix_ns,*dropped]{put_u64(&mut b,v);}put_u32(&mut b,*frames);header(w,105,b.len()+samples.len()*4)?;w.write_all(&b)?;
   let mut block=[0u8;4096];for chunk in samples.chunks(1024){for (i,value)in chunk.iter().enumerate(){block[i*4..i*4+4].copy_from_slice(&value.to_le_bytes());}w.write_all(&block[..chunk.len()*4])?;}
  },
  Event::AudioFormat{session,source,lease,stream,rate,channels,layout,frames}=>{for v in [*session,*source,*lease,*stream]{put_u64(&mut b,v);}for v in [*rate,*channels,*layout as u32,*frames]{put_u32(&mut b,v);}header(w,106,b.len())?;w.write_all(&b)?;},
  Event::SkippedAudio=>return Ok(()),
  Event::Hello=>{put_u32(&mut b,VERSION);put_str(&mut b,CEF_HASH);put_str(&mut b,CEF_COMMIT);header(w,101,b.len())?;w.write_all(&b)?;},
  Event::Paint{session,source,lease,width,height,qpc_ns,pixels}=>{if pixels.len()!=frame_bytes(*width,*height)?{return Err(invalid("browser paint shape"));}for v in [*session,*source,*lease,*qpc_ns]{put_u64(&mut b,v);}put_u32(&mut b,*width);put_u32(&mut b,*height);header(w,102,b.len()+pixels.len())?;w.write_all(&b)?;w.write_all(pixels)?;},
  Event::State{session,source,lease,kind,message}=>{if message.len()>4096{return Err(invalid("browser state bytes"));}for v in [*session,*source,*lease]{put_u64(&mut b,v);}put_u32(&mut b,*kind);put_str(&mut b,message);header(w,103,b.len())?;w.write_all(&b)?;},Event::Stopped=>header(w,104,0)?,Event::SkippedPaint=>return Ok(())};w.flush()
}
/// Reserve caller-owned payload credit before allocating a paint. A denied
/// paint is drained through a fixed8KiB buffer, without producing a fake frame.
pub fn read_event(r:&mut impl Read,mut reserve:impl FnMut(usize)->bool,mut reserve_pcm:impl FnMut(usize)->bool)->io::Result<Event>{let (kind,n)=read_header(r)?;
 if kind==102 {if n<40||n>MAX_FRAME+40{return Err(invalid("browser paint bytes"));}let mut h=[0;40];r.read_exact(&mut h)?;let mut d=Decoder{b:&h,p:0};let session=d.u64()?;let source=d.u64()?;let lease=d.u64()?;let qpc_ns=d.u64()?;let width=d.u32()?;let height=d.u32()?;let bytes=frame_bytes(width,height)?;if n!=40+bytes{return Err(invalid("browser paint shape"));}if !reserve(bytes){let mut scratch=[0u8;8192];let mut left=bytes;while left>0 {let chunk=left.min(scratch.len());r.read_exact(&mut scratch[..chunk])?;left-=chunk;}return Ok(Event::SkippedPaint);}let mut pixels=vec![0;bytes];r.read_exact(&mut pixels)?;return Ok(Event::Paint {session,source,lease,width,height,qpc_ns,pixels});}
 if kind==105 {
  if n<116||n>116+MAX_PCM_FRAMES*8{return Err(invalid("browser PCM packet bytes"));}let mut h=[0u8;116];r.read_exact(&mut h)?;let mut d=Decoder{b:&h,p:0};let session=d.u64()?;let source=d.u64()?;let lease=d.u64()?;let grant=get_grant(&mut d)?;let stream=d.u64()?;let sequence=d.u64()?;let pts_ms=d.u64()? as i64;let qpc_ns=d.u64()?;let unix_ns=d.u64()?;let dropped=d.u64()?;let frames=d.u32()?;d.done()?;
  let bytes=frames as usize*8;if frames==0||frames as usize>MAX_PCM_FRAMES||n!=116+bytes||grant.session!=session||stream==0||sequence==0{return Err(invalid("browser PCM scope/shape"));}
  if !reserve_pcm(bytes){let mut scratch=[0u8;4096];let mut left=bytes;while left>0{let count=left.min(scratch.len());r.read_exact(&mut scratch[..count])?;left-=count;}return Ok(Event::SkippedAudio);}
  let mut samples=Vec::with_capacity(frames as usize*2);let mut block=[0u8;4096];let mut left=bytes;while left>0{let count=left.min(block.len());r.read_exact(&mut block[..count])?;for v in block[..count].chunks_exact(4){let value=f32::from_le_bytes(v.try_into().map_err(|_|invalid("browser PCM float"))?);if !value.is_finite(){return Err(invalid("browser PCM nonfinite sample"));}samples.push(value);}left-=count;}
  return Ok(Event::Audio{session,source,lease,grant,stream,sequence,pts_ms,qpc_ns,unix_ns,dropped,frames,samples});
 }
 if n>8192{return Err(invalid("browser event metadata bytes"));}let mut b=vec![0;n];r.read_exact(&mut b)?;let mut d=Decoder{b:&b,p:0};let result=match kind {107=>Event::NativeAudio{session:d.u64()?,source:d.u64()?,lease:d.u64()?,serial:d.u64()?,muted:d.boolean()?},106=>Event::AudioFormat{session:d.u64()?,source:d.u64()?,lease:d.u64()?,stream:d.u64()?,rate:d.u32()?,channels:d.u32()?,layout:d.i32()?,frames:d.u32()?},101=>{if d.u32()?!=VERSION||d.string(40)?!=CEF_HASH||d.string(40)?!=CEF_COMMIT{return Err(invalid("browser helper ABI/version mismatch"));}Event::Hello},103=>Event::State {session:d.u64()?,source:d.u64()?,lease:d.u64()?,kind:d.u32()?,message:d.string(4096)?},104=>Event::Stopped,_=>return Err(invalid("browser event type"))};d.done()?;Ok(result)}
