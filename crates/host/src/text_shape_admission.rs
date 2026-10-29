//! Pre-shape admission for the explicitly selected custom-font profile.
//!
//! Partition follows locked cosmic-text 0.14.2 Buffer::set_text, ShapeLine,
//! ShapeSpan and ShapeWord (uniform source attrs). The adjusted-level routine
//! below is adapted from cosmic-text, Copyright (c) 2022 System76, MIT.
//! See assets/licenses/cosmic-text-MIT.txt. No shaping/font parser runs here.
use super::{TextError,error,limit};
use cosmic_text::{LineIter,LayoutGlyph,ShapeGlyph,Fallback};
use unicode_bidi::{BidiInfo,BidiClass,Level,Paragraph};

pub(crate) const SELECTED_FAMILY:&str="SkyLive Explicit Selected Face";
const MAX_EXPANDED_GLYPHS:usize=262_144;
const MAX_SHAPER_OPS:usize=8*1024*1024;
const MAX_METADATA_BYTES:usize=128*1024*1024;
// Bounded non-glyph line/span/bidi/script/lookup/layout bookkeeping, plus
// table-plan vectors admitted by the separate <=1024 lookup/table profile.
const FIXED_METADATA_BYTES:usize=8*1024*1024;

/// The only DB face is selected explicitly. Empty script/common lists prevent
/// substitution; forbidding its private alias prevents cosmic's final "other
/// fonts" stage from shaping the identical face a second time for glyph zero.
pub(super) struct SelectedOnly;
impl Fallback for SelectedOnly{
    fn common_fallback(&self)->&[&'static str]{&[]}
    fn forbidden_fallback(&self)->&[&'static str]{&[SELECTED_FAMILY]}
    fn script_fallback(&self,_:unicode_script::Script,_:&str)->&[&'static str]{&[]}
}

#[derive(Default)]
struct Account{glyphs:usize,operations:usize,largest:usize}
impl Account{
    fn run(&mut self,text:&str)->Result<(),TextError>{
        let n=text.chars().count();if n==0{return Ok(());}
        // Exact locked rustybuzz 0.14.1 hb_buffer_t::enter limits. ensure()
        // checks max_len before growing info/position vectors; recurse()
        // consumes max_ops. These are separate native limits, not a claim
        // that every library instruction is counted by max_ops.
        let glyphs=n.checked_mul(64).ok_or_else(fragmented)?.max(16_384);
        let operations=n.checked_mul(1024).ok_or_else(fragmented)?.max(16_384);
        self.glyphs=self.glyphs.checked_add(glyphs).ok_or_else(fragmented)?;
        self.operations=self.operations.checked_add(operations).ok_or_else(fragmented)?;
        self.largest=self.largest.max(glyphs);
        if self.glyphs>MAX_EXPANDED_GLYPHS||self.operations>MAX_SHAPER_OPS{return Err(fragmented());}
        Ok(())
    }
    fn span(&mut self,span:&str)->Result<(),TextError>{
        let mut start=0;
        for(end,_)in unicode_linebreak::linebreaks(span){
            let segment=span.get(start..end).ok_or_else(||error("text_shape_partition","Invalid Unicode line-break boundary."))?;
            let mut trailing=end;
            for(i,c)in segment.char_indices().rev(){if c.is_whitespace(){trailing=start+i;}else{break;}}
            if start<trailing{self.run(&span[start..trailing])?;}
            if trailing<end{for(i,c)in span[trailing..end].char_indices(){self.run(&span[trailing+i..trailing+i+c.len_utf8()])?;}}
            start=end;
        }
        Ok(())
    }
    fn bytes(&self,text_bytes:usize)->Result<usize,TextError>{
        // Word ShapeGlyph storage (2x Vec growth), old/rebuilt LayoutGlyph
        // storage (4x including cached layout capacity), and layout ranges /
        // clusters. Rustybuzz keeps one reusable info+position pair, each
        // 20 bytes in the locked library; allow 2x vector growth (80 bytes).
        // This is a conservative logical allocation envelope, not a promise
        // about allocator bookkeeping or a measured resident-set value.
        let retained_unit=2*std::mem::size_of::<ShapeGlyph>()
            +4*std::mem::size_of::<LayoutGlyph>()+64;
        let retained=self.glyphs.checked_mul(retained_unit).ok_or_else(fragmented)?;
        let scratch=self.largest.checked_mul(80).ok_or_else(fragmented)?;
        let bounded_text=text_bytes.checked_mul(256).ok_or_else(fragmented)?;
        let bytes=retained.checked_add(scratch).and_then(|n|n.checked_add(bounded_text))
            .and_then(|n|n.checked_add(FIXED_METADATA_BYTES)).ok_or_else(fragmented)?;
        if bytes>MAX_METADATA_BYTES{return Err(fragmented());}Ok(bytes)
    }
}
fn fragmented()->TextError{limit("text_shape_admission","Custom-font text exceeds the bounded shaping profile. Reduce word, whitespace or bidirectional fragmentation, or shorten the text.")}

// Exact cosmic ShapeLine::adjust_levels L1 policy; copying only this bounded
// metadata step lets admission count the same eventual Rustybuzz runs.
fn adjusted(para:&Paragraph<'_, '_>)->Vec<Level>{
    use BidiClass::*;
    let text=para.info.text;let mut levels=para.info.levels.clone();
    let mut reset_from=Some(0);let mut reset_to=None;
    for(i,c)in text.char_indices(){
        match para.info.original_classes[i]{
            RLE|LRE|RLO|LRO|PDF|BN=>{},
            B|S=>{reset_to=Some(i+c.len_utf8());if reset_from.is_none(){reset_from=Some(i);}},
            WS|FSI|LRI|RLI|PDI=>{if reset_from.is_none(){reset_from=Some(i);}},
            _=>reset_from=None,
        }
        if let(Some(from),Some(to))=(reset_from,reset_to){levels[from..to].fill(para.para.level);reset_from=None;reset_to=None;}
    }
    if let Some(from)=reset_from{levels[from..].fill(para.para.level);}levels
}

/// Called BEFORE font loading and Buffer::set_text (which shapes immediately).
/// Returns the maximum transient shaping reservation for this invocation.
pub(super) fn prepare(text:&str)->Result<usize,TextError>{
    if text.len()>skylive_core::MAX_TEXT_BYTES||text.chars().count()>skylive_core::MAX_TEXT_CHARS{return Err(fragmented());}
    let mut account=Account::default();
    for(range,_)in LineIter::new(text){
        let line=&text[range];let bidi=BidiInfo::new(line,None);
        let direction=bidi.paragraphs.first().map(|p|p.level.is_rtl());
        for info in &bidi.paragraphs{
            // cosmic asserts equal paragraph direction inside one CR/LF line.
            // Reject this unsupported profile before entering that assertion.
            if direction!=Some(info.level.is_rtl()){return Err(error("text_shape_direction","Custom-font paragraphs within one line require the same base direction. Insert a line break."));}
            let levels=adjusted(&Paragraph::new(&bidi,info));
            let mut start=info.range.start;let end=info.range.end;
            if start==end{continue;}
            let mut level=levels[start];
            for(i,&next)in levels.iter().enumerate().take(end).skip(start+1){
                if next!=level{
                    account.span(line.get(start..i).ok_or_else(||error("text_shape_partition","Invalid Unicode level boundary."))?)?;
                    start=i;level=next;
                }
            }
            account.span(line.get(start..end).ok_or_else(||error("text_shape_partition","Invalid Unicode level boundary."))?)?;
        }
    }
    account.bytes(text.len())
}
