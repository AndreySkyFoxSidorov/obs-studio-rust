//! Custom-font outline-only raster admission. No font hint programs or bitmaps.
use super::{TextError,error,limit};
use cosmic_text::{CacheKey,FontSystem,SwashImage,ttf_parser};
use swash::scale::ScaleContext;
use swash::zeno::{Mask,Format,Origin,Vector};
pub(super) struct Raster{context:ScaleContext,outline_remaining:usize,selected:cosmic_text::fontdb::ID,costs:Vec<u32>}
struct Proof{commands:usize,remaining:usize,bound:f32,invalid:bool}
impl Proof{fn point(&mut self,x:f32,y:f32){if !x.is_finite()||!y.is_finite()||x.abs()>self.bound||y.abs()>self.bound{self.invalid=true;}}
fn command(&mut self){self.commands=self.commands.saturating_add(1);if self.commands>16384||self.commands>self.remaining{self.invalid=true;}}}
impl ttf_parser::OutlineBuilder for Proof{
fn move_to(&mut self,x:f32,y:f32){self.command();self.point(x,y);}
fn line_to(&mut self,x:f32,y:f32){self.command();self.point(x,y);}
fn quad_to(&mut self,x1:f32,y1:f32,x:f32,y:f32){self.command();self.point(x1,y1);self.point(x,y);}
fn curve_to(&mut self,x1:f32,y1:f32,x2:f32,y2:f32,x:f32,y:f32){self.command();self.point(x1,y1);self.point(x2,y2);self.point(x,y);}
fn close(&mut self){self.command();}}
impl Raster{
    pub(super) fn new(selected:cosmic_text::fontdb::ID,costs:Vec<u32>)->Self{Self{context:ScaleContext::with_max_entries(1),outline_remaining:1024*1024,selected,costs}}
    pub(super) fn image(&mut self,fonts:&mut FontSystem,key:CacheKey,work:&mut usize)->Result<Option<SwashImage>,TextError>{
        if key.font_id!=self.selected{return Err(error("font_face_mismatch","Text shaping selected another font face; no fallback was accepted."));}
        let font=fonts.get_font(key.font_id).ok_or_else(||error("font_face","The selected font face is unavailable during rasterization."))?;
        let cost=self.costs.get(usize::from(key.glyph_id)).copied().ok_or_else(||error("font_glyph","Glyph has no accepted TrueType graph proof."))? as usize;
        self.outline_remaining=self.outline_remaining.checked_sub(cost).ok_or_else(||limit("text_outline_budget","Custom font expanded outline work exceeds1Mi operations."))?;
        let mut proof=Proof{commands:0,remaining:cost,bound:font.rustybuzz().units_per_em() as f32*16.0,invalid:false};
        let _=font.rustybuzz().outline_glyph(ttf_parser::GlyphId(key.glyph_id),&mut proof);
        if proof.invalid{return Err(limit("text_outline_budget","Custom font outline coordinates or command work exceed the admitted limits."));}
        if proof.commands==0{return Ok(None);}
        let size=f32::from_bits(key.font_size_bits);
        if !size.is_finite()||!(1.0..=1000.0).contains(&size){return Err(error("font_size","Custom font glyph size exceeds1..1000 pixels."));}
        let mut scaler=self.context.builder(font.as_swash()).size(size).hint(false).build();
        let outline=scaler.scale_outline(key.glyph_id).ok_or_else(||error("font_outline","The selected outline could not be scaled; no fallback was used."))?;
        if outline.points().len()>65536||outline.verbs().len()>16384||outline.points().iter().any(|p|!p.x.is_finite()||!p.y.is_finite()||p.x.abs()>16384.0||p.y.abs()>16384.0){return Err(limit("text_outline_budget","Scaled custom font outline exceeds its point or coordinate limit."));}
        let offset=Vector::new(key.x_bin.as_float(),key.y_bin.as_float());
        let mut mask=Mask::new(outline.path());mask.format(Format::Alpha).origin(Origin::BottomLeft).offset(offset).render_offset(offset);
        let mut dimensions=(0,0);mask.inspect(|_,w,h|dimensions=(w,h));let(w,h)=dimensions;
        if w==0||h==0{return Ok(None);}
        let bytes=(w as usize).checked_mul(h as usize).ok_or_else(||limit("text_glyph_budget","Custom glyph bitmap size overflow."))?;
        let next=work.checked_add(bytes).filter(|v|*v<=super::MAX_GLYPH_PIXELS).ok_or_else(||limit("text_glyph_budget","Custom glyph work exceeds16Mi pixels."))?;
        if w>4096||h>4096{return Err(limit("text_glyph_budget","Custom glyph bitmap exceeds4096 pixels per edge."));}
        // Every bitmap byte and total glyph work is admitted BEFORE allocation.
        let mut image=SwashImage::new();image.data.try_reserve_exact(bytes).map_err(|_|limit("text_glyph_budget","Cannot allocate the admitted custom glyph bitmap."))?;
        image.data.resize(bytes,0);image.placement=mask.render_into(&mut image.data,None);
        image.content=cosmic_text::SwashContent::Mask;image.source=swash::scale::Source::Outline;*work=next;Ok(Some(image))
    }
}
