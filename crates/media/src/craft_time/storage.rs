// SPDX-License-Identifier: MIT
use super::*;
use std::sync::Arc;
const PAGE:usize=1024;
/// Source-owned immutable pages make transactional snapshots cheap. A write
/// copies only its touched page; no global pool or unbounded spare buffers.
#[derive(Clone)]pub(super) struct Buffer{len:usize,pages:Box<[Arc<[f32;PAGE]>]>}
impl Buffer{
 pub(super) fn bytes(len:usize)->Result<usize,CraftTimeError>{
  len.div_ceil(PAGE).checked_mul(PAGE*4+2*std::mem::size_of::<usize>()+std::mem::size_of::<Arc<[f32;PAGE]>>()).ok_or(CraftTimeError::Budget)
 }
 pub(super) fn new(len:usize)->Result<Self,CraftTimeError>{
  if len<4{return Err(CraftTimeError::InvalidBlock);}
  let count=len.div_ceil(PAGE);let mut pages=Vec::new();
  pages.try_reserve_exact(count).map_err(|_|CraftTimeError::Budget)?;
  for _ in 0..count{pages.push(Arc::new([0.0;PAGE]));}
  Ok(Self{len,pages:pages.into_boxed_slice()})
 }
 pub(super) fn len(&self)->usize{self.len}
 #[inline]pub(super) fn at(&self,index:usize)->f32{self.pages[index/PAGE][index%PAGE]}
 #[inline]pub(super) fn set(&mut self,index:usize,value:f32){Arc::make_mut(&mut self.pages[index/PAGE])[index%PAGE]=value;}
 pub(super) fn clear(&mut self){for page in &mut self.pages{Arc::make_mut(page).fill(0.0);}}
}
#[derive(Clone,Copy)]pub(super) struct Exponential{cur:f32,target:f32,coef:f32}
impl Exponential{
 pub(super) fn new(value:f32,ms:f32,rate:f32)->Self{let n=ms*0.001*rate;Self{cur:value,target:value,coef:if n<1e-3{0.0}else{(-1.0/n).exp()}}}
 pub(super) fn set(&mut self,value:f32){self.target=value;}
 pub(super) fn snap(&mut self){self.cur=self.target;}
 pub(super) fn next(&mut self)->f32{
  if self.cur!=self.target{self.cur=self.target+(self.cur-self.target)*self.coef;
   if (self.cur-self.target).abs()<=1e-6*(1.0+self.target.abs()){self.cur=self.target;}}
  self.cur
 }
}
#[derive(Clone,Copy)]pub(super) struct Linear{current:f64,target:f64,step:f64,remaining:u32,ramp:u32}
impl Linear{
 pub(super) fn new(value:f32,rate:f32,ms:f32)->Self{Self{current:value as f64,target:value as f64,step:0.0,remaining:0,ramp:(rate*ms/1000.0).round().max(0.0)as u32}}
 pub(super) fn set(&mut self,target:f32){let t=target as f64;if t==self.target{return;}self.target=t;if self.ramp==0{self.current=t;self.remaining=0;}else{self.remaining=self.ramp;self.step=(t-self.current)/self.ramp as f64;}}
 pub(super) fn snap(&mut self){self.current=self.target;self.remaining=0;}
 pub(super) fn tick(&mut self)->f32{if self.remaining>0{self.remaining-=1;if self.remaining==0{self.current=self.target;}else{self.current+=self.step;}}self.current as f32}
 pub(super) fn value(self)->f32{self.current as f32}pub(super) fn smoothing(self)->bool{self.remaining>0}
}
#[inline]pub(super) fn flush_sc(x:f32)->f32{if x.is_finite()&&x.abs()>1e-25{x}else{0.0}}
#[inline]pub(super) fn flush_fc(x:f32)->f32{if x.abs()<1e-25{0.0}else{x}}
pub(super) fn delay_parts(len:usize,delay:f32)->(usize,f32){let d=delay.clamp(1.0,(len-2)as f32);let di=d.floor();let distance=di as usize;let distance=if distance<=len{distance}else{distance%len};(distance,d-di)}
#[inline]pub(super) fn read(buffer:&Buffer,w:usize,(distance,frac):(usize,f32))->f32{
 let len=buffer.len();let i0=if w>=distance{w-distance}else{len-(distance-w)};let i1=if i0==0{len-1}else{i0-1};
 buffer.at(i0)+(buffer.at(i1)-buffer.at(i0))*frac
}
