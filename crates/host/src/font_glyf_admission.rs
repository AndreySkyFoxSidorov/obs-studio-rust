//! Raw TrueType graph/cost admission BEFORE any outline walk/allocation.
use super::*;
const MAX_GLYPH_COST:u32=16384;
struct Graph<'a,'b>{loca:&'a[u8],glyf:&'a[u8],long:bool,costs:Vec<u32>,states:Vec<u8>,heights:Vec<u8>,budget:&'b mut FontReadBudget}
impl Graph<'_,'_>{
    fn charge(&mut self,work:usize)->Result<(),FontError>{self.budget.structure_work=self.budget.structure_work.checked_sub(work).ok_or_else(||{self.budget.exhausted=true;limit("TrueType structural work exceeds8Mi operations for this request.")})?;Ok(())}
    fn offset(&self,index:usize)->Result<usize,FontError>{if self.long{Ok(be32(self.loca,index*4)? as usize)}else{Ok(usize::from(be16(self.loca,index*2)?)*2)}}
    fn cost(&mut self,glyph:usize,depth:usize)->Result<u32,FontError>{
        if depth>16{return Err(limit("TrueType composite glyph depth exceeds16."));}
        match self.states.get(glyph){Some(2)=>{
            let height=usize::from(*self.heights.get(glyph).ok_or_else(||invalid("font_glyph","Invalid glyph height index."))?);
            if depth+height>16{return Err(limit("TrueType composite glyph depth exceeds16."));}
            return self.costs.get(glyph).copied().ok_or_else(||invalid("font_glyph","Glyph index is outside the font."));
        },Some(1)=>return Err(invalid("font_glyph_cycle","TrueType composite glyphs contain a cycle.")),Some(0)=>{},_=>return Err(invalid("font_glyph","Glyph index is outside the font."))}
        self.charge(1)?;
        let start=self.offset(glyph)?;let end=self.offset(glyph+1)?;
        if end<start{return Err(invalid("font_glyph","TrueType glyph offsets are not monotonic."));}
        let data=bytes_at(self.glyf,start,end-start)?;
        *self.states.get_mut(glyph).ok_or_else(||invalid("font_glyph","Invalid glyph index."))?=1;
        let mut height=0u8;
        let result=if data.is_empty(){0}else{
            bytes_at(data,0,10)?;let contours=be16(data,0)? as i16;
            if contours>=0{
                let contours=contours as usize;
                if contours==0 {0}else{
                if contours>2048{return Err(limit("TrueType glyph exceeds2048 contours."));}
                bytes_at(data,10,contours*2)?;
                let mut previous=None;
                for contour in 0..contours{let end=be16(data,10+contour*2)?;if previous.is_some_and(|old|old>=end){return Err(invalid("font_glyph","TrueType contour endpoints are not ordered."));}previous=Some(end);}
                let points=previous.map_or(0,|end|usize::from(end)+1);
                if points>2048{return Err(limit("TrueType simple glyph exceeds2048 points."));}
                self.charge(points+contours)?;
                let instructions=10+contours*2;let len=usize::from(be16(data,instructions)?);let mut at=instructions+2;
                bytes_at(data,at,len)?;at+=len;
                let mut decoded=0usize;let mut coordinates=0usize;
                while decoded<points{
                    let flag=*data.get(at).ok_or_else(||invalid("font_glyph","TrueType point flags are truncated."))?;at+=1;
                    let repeat=if flag&8!=0{let n=usize::from(*data.get(at).ok_or_else(||invalid("font_glyph","TrueType repeat flag is truncated."))?)+1;at+=1;n}else{1};
                    decoded=decoded.checked_add(repeat).filter(|n|*n<=points).ok_or_else(||invalid("font_glyph","TrueType repeat flags exceed the declared points."))?;
                    let x=if flag&2!=0{1}else if flag&16!=0{0}else{2};let y=if flag&4!=0{1}else if flag&32!=0{0}else{2};
                    coordinates=coordinates.checked_add(repeat*(x+y)).ok_or_else(||limit("TrueType coordinate size overflow."))?;
                }
                bytes_at(data,at,coordinates)?;
                (points as u32).checked_mul(8).and_then(|n|n.checked_add(contours as u32+8)).ok_or_else(||limit("TrueType outline cost overflow."))?
                }
            }else{
                if contours!=-1{return Err(invalid("font_glyph","Unsupported TrueType composite contour marker."));}
                let mut at=10usize;let mut count=0usize;let mut result=1u32;
                loop{
                    self.charge(1)?;count+=1;if count>64{return Err(limit("TrueType glyph exceeds64 components."));}
                    let flags=be16(data,at)?;let child=usize::from(be16(data,at+2)?);at+=4;
                    let args=if flags&1!=0{4}else{2};let scales=flags&(8|64|128);
                    let transform=match scales{0=>0,8=>2,64=>4,128=>8,_=>return Err(invalid("font_glyph","Conflicting TrueType component transforms."))};
                    bytes_at(data,at,args+transform)?;at+=args+transform;
                    let child_cost=self.cost(child,depth+1)?;
                    let child_height=*self.heights.get(child).ok_or_else(||invalid("font_glyph","Invalid component height index."))?;
                    height=height.max(child_height.checked_add(1).ok_or_else(||limit("TrueType glyph height overflow."))?);
                    result=result.checked_add(child_cost.checked_add(1).ok_or_else(||limit("TrueType component cost overflow."))?).filter(|cost|*cost<=MAX_GLYPH_COST).ok_or_else(||limit("Expanded TrueType glyph exceeds16384 outline operations."))?;
                    if flags&32==0{if flags&256!=0{let len=usize::from(be16(data,at)?);bytes_at(data,at+2,len)?;}break;}
                }result
            }
        };
        if depth+usize::from(height)>16{return Err(limit("TrueType composite glyph depth exceeds16."));}
        if result>MAX_GLYPH_COST{return Err(limit("Expanded TrueType glyph exceeds16384 outline operations."));}
        *self.costs.get_mut(glyph).ok_or_else(||invalid("font_glyph","Invalid glyph index."))?=result;
        *self.heights.get_mut(glyph).ok_or_else(||invalid("font_glyph","Invalid glyph height index."))?=height;
        *self.states.get_mut(glyph).ok_or_else(||invalid("font_glyph","Invalid glyph index."))?=2;Ok(result)
    }
}
pub(super) fn admit(face:&ttf_parser::Face<'_>,budget:&mut FontReadBudget)->Result<Vec<u32>,FontError>{
    let raw=face.raw_face();let table=|tag:&[u8;4]|raw.table(ttf_parser::Tag::from_bytes(tag)).ok_or_else(||invalid("font_glyph","A required TrueType table is missing."));
    let head=table(b"head")?;let kind=be16(head,50)?;if kind>1{return Err(invalid("font_glyph","Unsupported TrueType location format."));}
    let maxp=table(b"maxp")?;if be32(maxp,0)?!=0x00010000{return Err(invalid("font_glyph","TrueType requires a version1 maxp table."));}
    for offset in [6,8,10,12]{if be16(maxp,offset)?>16384{return Err(limit("TrueType maximum outline metadata exceeds16384."));}}
    let count=usize::from(face.number_of_glyphs());let loca=table(b"loca")?;let glyf=table(b"glyf")?;bytes_at(loca,0,(count+1)*if kind==1{4}else{2})?;
    let mut costs=Vec::new();costs.try_reserve_exact(count).map_err(|_|limit("Cannot allocate bounded TrueType cost metadata."))?;costs.resize(count,0);
    let mut states=Vec::new();states.try_reserve_exact(count).map_err(|_|limit("Cannot allocate bounded TrueType visit metadata."))?;states.resize(count,0);
    let mut heights=Vec::new();heights.try_reserve_exact(count).map_err(|_|limit("Cannot allocate bounded TrueType height metadata."))?;heights.resize(count,0);
    // At most32768*(4+1+1) payload bytes, reused per face; no outline pixels exist.
    let mut graph=Graph{loca,glyf,long:kind==1,costs,states,heights,budget};for glyph in 0..count{graph.cost(glyph,0)?;}Ok(graph.costs)
}
