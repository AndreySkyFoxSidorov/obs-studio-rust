//! Fixed-size seeded traversal. No global RNG state, raster IO or render rerolls.
use skylive_core::SlideShowRandomCompatibility as Compatibility;
pub(crate) const MAX_SOURCE_ADVANCES:u64=4096;
pub(crate) const MAX_UPDATE_ADVANCES:u64=16384;
pub(crate) const WORK_LIMIT:&str="Random slideshow catch-up exceeds the bounded work limit; explicitly restart, stop or reseed it.";
#[derive(Clone,Copy,Debug)]
pub(crate) struct Run{pub current:usize,prev:[usize;5],next:[usize;5],pub seed:u64,state:u64,pub draws:u64,pub generation:u64,policy:Compatibility,count:usize}
impl Run{
    pub(crate) fn new(seed:u64,count:usize,policy:Compatibility)->Result<Self,&'static str>{let mut run=Self{current:0,prev:[0;5],next:[0;5],seed,state:seed,draws:0,generation:0,policy,count};run.rebuild(false)?;Ok(run)}
    fn word(&mut self)->Result<u64,&'static str>{self.draws=self.draws.checked_add(1).ok_or("Random slideshow draw identity exhausted.")?;self.state=self.state.wrapping_add(0x9e3779b97f4a7c15);let mut value=self.state;value=(value^(value>>30)).wrapping_mul(0xbf58476d1ce4e5b9);value=(value^(value>>27)).wrapping_mul(0x94d049bb133111eb);Ok(value^(value>>31))}
    fn range(&mut self,count:usize)->Result<usize,&'static str>{if count<=1{return Ok(0);}let count=count as u64;let threshold=count.wrapping_neg()%count;for _ in 0..16{let value=self.word()?;if value>=threshold{return Ok((value%count)as usize);}}Err("Random slideshow bounded range sampling exhausted.")}
    fn other(&mut self,previous:usize)->Result<usize,&'static str>{if self.count<=1{return Ok(0);}let value=self.range(self.count-1)?;Ok(if value>=previous{value+1}else{value})}
    pub(crate) fn rebuild(&mut self,activation:bool)->Result<(),&'static str>{
        self.generation=self.generation.checked_add(1).ok_or("Random slideshow run identity exhausted.")?;
        if self.policy==Compatibility::LegacyV1{self.current=if activation||self.generation==1{self.other(self.current)?}else{0};return Ok(());}
        self.current=self.range(self.count)?;let mut index=self.current;for i in 0..5{index=self.other(index)?;self.next[i]=index;}index=self.current;for i in 0..5{index=self.other(index)?;self.prev[4-i]=index;}Ok(())
    }
    pub(crate) fn advance(&mut self,forward:bool)->Result<usize,&'static str>{
        if self.policy==Compatibility::LegacyV1{self.current=self.other(self.current)?;return Ok(self.current);}
        if forward{let edge=self.other(self.next[4])?;self.prev.copy_within(1..5,0);self.prev[4]=self.current;self.current=self.next[0];self.next.copy_within(1..5,0);self.next[4]=edge;}
        else{let edge=self.other(self.prev[0])?;self.next.copy_within(0..4,1);self.next[0]=self.current;self.current=self.prev[4];self.prev.copy_within(0..4,1);self.prev[0]=edge;}
        Ok(self.current)
    }
    pub(crate) fn policy(&self)->Compatibility{self.policy}
}
