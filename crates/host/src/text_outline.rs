//! Bounded raster-space round outline. This approximates OBS GDI path strokes;
//! it does not claim the same antialiasing or path geometry.
use super::{TextError,limit,blend};

const MAX_OUTLINE_WORK:usize=256*1024*1024;
pub(super) struct Plan{width:usize,height:usize,padded_width:usize,padded_height:usize,radius:usize,pixels:usize}
impl Plan{
    pub(super) fn new(width:u32,height:u32,size:u8)->Result<Self,TextError>{
        let radius=(usize::from(size)+1)/2;let width=width as usize;let height=height as usize;
        let padded_width=width.checked_add(radius*2).ok_or_else(||limit("text_outline_limit","Outline size overflow."))?;
        let padded_height=height.checked_add(radius*2).ok_or_else(||limit("text_outline_limit","Outline size overflow."))?;
        let pixels=padded_width.checked_mul(padded_height).ok_or_else(||limit("text_outline_limit","Outline pixel count overflow."))?;
        let work=pixels.checked_mul(radius*2+1).and_then(|n|n.checked_mul(4)).filter(|n|*n<=MAX_OUTLINE_WORK).ok_or_else(||limit("text_outline_limit","Outline processing exceeds its pixel-work budget."))?;
        let _=work;Ok(Self{width,height,padded_width,padded_height,radius,pixels})
    }
    pub(super) fn bytes(&self)->Result<usize,TextError>{self.pixels.checked_mul(2).and_then(|v|v.checked_add(self.padded_width.checked_mul(std::mem::size_of::<usize>())?)).ok_or_else(||limit("text_outline_limit","Outline memory overflow."))}
    pub(super) fn allocate(&self)->Result<Buffers,TextError>{
        let mut source=Vec::new();source.try_reserve_exact(self.pixels).map_err(|_|limit("text_outline_limit","Cannot allocate outline mask."))?;source.resize(self.pixels,0);
        let mut expanded=Vec::new();expanded.try_reserve_exact(self.pixels).map_err(|_|limit("text_outline_limit","Cannot allocate outline result."))?;expanded.resize(self.pixels,0);
        let mut deque=Vec::new();deque.try_reserve_exact(self.padded_width).map_err(|_|limit("text_outline_limit","Cannot allocate outline row workspace."))?;deque.resize(self.padded_width,0);
        Ok(Buffers{source,expanded,deque})
    }
    pub(super) fn add(&self,buffers:&mut Buffers,x:i32,y:i32,coverage:u8){
        let x=i64::from(x)+self.radius as i64;let y=i64::from(y)+self.radius as i64;
        if x>=0&&y>=0&&(x as usize)<self.padded_width&&(y as usize)<self.padded_height{
            let slot=&mut buffers.source[y as usize*self.padded_width+x as usize];*slot=(*slot).max(coverage);
        }
    }
    pub(super) fn composite(&self,buffers:&mut Buffers,output:&mut[u8],color:[u8;3],opacity:u8){
        let w=self.padded_width;let h=self.padded_height;let radius=self.radius as isize;
        for dy in -radius..=radius{
            let dx=((radius*radius-dy*dy) as f64).sqrt().floor() as usize;
            for sy in 0..h{
                let ty=sy as isize+dy;if ty<0||ty>=h as isize{continue;}
                let row=&buffers.source[sy*w..(sy+1)*w];let target=&mut buffers.expanded[ty as usize*w..(ty as usize+1)*w];
                let mut head=0;let mut tail=0;let mut next=0;
                for x in 0..w{
                    let right=x.saturating_add(dx).min(w-1);
                    while next<=right{while tail>head&&row[buffers.deque[tail-1]]<=row[next]{tail-=1;}buffers.deque[tail]=next;tail+=1;next+=1;}
                    let left=x.saturating_sub(dx);while head<tail&&buffers.deque[head]<left{head+=1;}
                    target[x]=target[x].max(row[buffers.deque[head]]);
                }
            }
        }
        let alpha=(u16::from(opacity)*255/100) as u8;
        for y in 0..self.height{for x in 0..self.width{
            let coverage=buffers.expanded[(y+self.radius)*w+x+self.radius];
            let a=((u16::from(coverage)*u16::from(alpha)+127)/255) as u8;
            if a!=0{let at=(y*self.width+x)*4;blend(&mut output[at..at+4],[color[0],color[1],color[2],a]);}
        }}
    }
}
pub(super) struct Buffers{source:Vec<u8>,expanded:Vec<u8>,deque:Vec<usize>}
