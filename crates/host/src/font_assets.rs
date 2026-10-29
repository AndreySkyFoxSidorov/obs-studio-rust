//! Linked font bytes/metadata. Called only by the existing IO/render workers.
use cosmic_text::{fontdb,ttf_parser};
use skylive_core::{FontAsset,FontFace,FontFaceStyle,MAX_FONT_FILE_BYTES,MAX_FONT_FACES};
use sha2::{Digest,Sha256};
use std::{fs,io::Read,path::Path,sync::Arc};
pub type FontError=crate::text::TextError;
fn invalid(code:&'static str,detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Invalid,code,detail:detail.into()}}
fn limit(detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Limit,code:"font_resource_limit",detail:detail.into()}}
fn unavailable(code:&'static str,detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code,detail:detail.into()}}
#[path="font_glyf_admission.rs"] mod glyf;
pub struct FontReadBudget{remaining:u64,table_work:u64,structure_work:usize,exhausted:bool}
impl FontReadBudget{
    pub fn new(bytes:u64)->Self{Self{remaining:bytes,table_work:128*1024*1024,structure_work:8*1024*1024,exhausted:false}}
    pub(crate) fn exhausted(&self)->bool{self.exhausted}
    /// DirectWrite stream fragments and the later owned file snapshot share
    /// this budget; charge the stream before exposing or hashing its bytes.
    pub(crate) fn charge_external(&mut self,bytes:u64)->Result<(),FontError>{
        self.remaining=self.remaining.checked_sub(bytes).ok_or_else(||{self.exhausted=true;limit("Font stream exceeds the operation read budget.")})?;
        Ok(())
    }
}
#[derive(Clone,Debug,serde::Serialize)]
pub struct FontCatalog{pub faces:Vec<FontAsset>}
pub(crate) struct Loaded{pub db:fontdb::Database,pub id:fontdb::ID,pub costs:Vec<u32>}
fn be16(bytes:&[u8],at:usize)->Result<u16,FontError>{let end=at.checked_add(2).ok_or_else(||limit("Font offset overflow."))?;let b=bytes.get(at..end).ok_or_else(||invalid("font_header","Font header is truncated."))?;Ok(u16::from_be_bytes([b[0],b[1]]))}
fn be32(bytes:&[u8],at:usize)->Result<u32,FontError>{let end=at.checked_add(4).ok_or_else(||limit("Font offset overflow."))?;let b=bytes.get(at..end).ok_or_else(||invalid("font_header","Font header is truncated."))?;Ok(u32::from_be_bytes([b[0],b[1],b[2],b[3]]))}
fn bytes_at(bytes:&[u8],at:usize,len:usize)->Result<&[u8],FontError>{let end=at.checked_add(len).ok_or_else(||limit("Font offset overflow."))?;bytes.get(at..end).ok_or_else(||invalid("font_header","Font table exceeds the owned byte snapshot."))}
// Admission precedes fontdb's TTC TinyVec and name-string allocations. Shared
// TTC tables still consume cumulative parser work for every face that reads them.
fn preflight(bytes:&[u8],budget:&mut FontReadBudget,selected:Option<u32>)->Result<(u32,Option<Vec<u32>>),FontError>{
    let mut selected_costs=None;
    let ttc=bytes.get(..4)==Some(b"ttcf".as_slice());
    let count=if ttc{
        if !matches!(be32(bytes,4)?,0x00010000|0x00020000){return Err(invalid("font_collection","Unsupported TTC version."));}
        be32(bytes,8)?
    }else{1};
    if count==0||count>MAX_FONT_FACES{return Err(limit("A font file supports at most16 faces."));}
    for index in 0..count{
        let offset=if ttc{be32(bytes,12+index as usize*4)? as usize}else{0};
        if !matches!(be32(bytes,offset)?,0x00010000|0x4f54544f){return Err(invalid("font_format","Choose a static TrueType outline font (TTF or TTC). CFF OpenType fonts are not supported."));}
        let tables=usize::from(be16(bytes,offset.checked_add(4).ok_or_else(||limit("Font offset overflow."))?)?);
        if tables==0||tables>128{return Err(limit("Font face exceeds128 tables."));}
        let directory=offset.checked_add(12).ok_or_else(||limit("Font offset overflow."))?;
        bytes_at(bytes,directory,tables*16)?;
        for table in 0..tables{
            let at=directory+table*16;let tag=be32(bytes,at)?;
            let start=be32(bytes,at+8)? as usize;let length=be32(bytes,at+12)? as usize;
            let data=bytes_at(bytes,start,length)?;
            budget.table_work=budget.table_work.checked_sub(length as u64).ok_or_else(||{budget.exhausted=true;limit("Font table parser work exceeds128MiB for this operation.")})?;
            if matches!(tag,0x43464620|0x43464632){return Err(invalid("font_outline_profile","Only static TrueType glyf outlines are supported; CFF/CFF2 OpenType fonts are not available yet."));}
            if matches!(tag,0x434f4c52|0x4350414c|0x53564720|0x43424454|0x43424c43|0x45424454|0x45424c43|0x73626978|0x62646174|0x626c6f63|0x66766172|0x67766172|0x43464632|0x61766172|0x48564152|0x56564152|0x4d564152){
                return Err(invalid("font_profile","Variable, color and bitmap font tables are not supported by the static outline profile."));
            }
            if tag==0x6e616d65{
                if data.len()>64*1024{return Err(limit("Font name table exceeds64KiB."));}
                let names=usize::from(be16(data,2)?);let storage=usize::from(be16(data,4)?);
                if names>64{return Err(limit("Font name table exceeds64 records."));}
                bytes_at(data,6,names*12)?;
                for name in 0..names{let at=6+name*12;let len=usize::from(be16(data,at+8)?);let rel=usize::from(be16(data,at+10)?);
                    if len>512{return Err(limit("Font name exceeds512 encoded bytes."));}
                    bytes_at(data,storage.checked_add(rel).ok_or_else(||limit("Font name offset overflow."))?,len)?;
                }
            }
            if matches!(tag,0x47535542|0x47504f53){
                if length>2*1024*1024{return Err(limit("Font shaping table exceeds2MiB."));}
                let lookups=usize::from(be16(data,8)?);let count=usize::from(be16(data,lookups)?);
                if count>1024{return Err(limit("Font shaping table exceeds1024 lookups."));}
                let records=lookups.checked_add(2).ok_or_else(||limit("Font lookup offset overflow."))?;bytes_at(data,records,count*2)?;
            }
        }
        let face=ttf_parser::Face::parse(bytes,index).map_err(|_|invalid("font_face","The font contains an invalid or unsupported face."))?;
        let units=face.units_per_em();let glyphs=face.number_of_glyphs();let bounds=face.global_bounding_box();
        if !(16..=16384).contains(&units)||glyphs==0||glyphs>32768
            ||[bounds.x_min,bounds.y_min,bounds.x_max,bounds.y_max].iter().any(|v|i32::from(*v).abs()>i32::from(units)*16){
            return Err(limit("Font metrics or glyph count exceed the static outline profile."));
        }
        if face.tables().glyf.is_none(){return Err(invalid("font_outline","The selected font has no supported TrueType outline."));}
        let costs=glyf::admit(&face,budget)?;if selected==Some(index){selected_costs=Some(costs);}
    }Ok((count,selected_costs))
}
fn read(path:&Path,budget:&mut FontReadBudget,expected:Option<u64>)->Result<Vec<u8>,FontError>{
    if !path.is_absolute(){return Err(unavailable("font_path","The saved font path belongs to another platform or is not absolute."));}
    let mut file=fs::File::open(path).map_err(|e|unavailable("font_io",format!("Font {}: {e}",path.display())))?;
    let metadata=file.metadata().map_err(|e|unavailable("font_io",e.to_string()))?;let len=metadata.len();
    if !metadata.is_file()||len==0||len>MAX_FONT_FILE_BYTES{return Err(limit("Font encoded input exceeds16MiB or is not a regular nonempty file."));}
    if len>=budget.remaining{budget.exhausted=true;return Err(limit("Font input and growth sentinel exceed the operation read budget."));}
    if expected.is_some_and(|expected|expected!=len){return Err(unavailable("font_changed","The saved font file length changed. Load / Refresh and Apply to accept it."));}
    let cap=(len+1).min(budget.remaining);let mut bytes=Vec::new();bytes.try_reserve_exact(cap as usize).map_err(|_|limit("Cannot allocate the bounded font byte snapshot."))?;
    let result=(&mut file).take(cap).read_to_end(&mut bytes);budget.remaining-=bytes.len() as u64;
    result.map_err(|e|unavailable("font_io",e.to_string()))?;
    if bytes.len() as u64!=len||file.metadata().map_err(|e|unavailable("font_io",e.to_string()))?.len()!=len{return Err(unavailable("font_changed","The font changed while its byte snapshot was being read."));}Ok(bytes)
}
fn parsed(bytes:Vec<u8>,budget:&mut FontReadBudget,selected:Option<u32>)->Result<(fontdb::Database,u32,Option<Vec<u32>>),FontError>{
    let(count,costs)=preflight(&bytes,budget,selected)?;let mut db=fontdb::Database::new();
    let ids=db.load_font_source(fontdb::Source::Binary(Arc::new(bytes)));
    if ids.len()!=count as usize{return Err(invalid("font_face","A font collection face could not be prepared; no partial catalog was accepted."));}
    Ok((db,count,costs))
}
fn descriptor(db:&fontdb::Database,id:fontdb::ID)->Result<FontFace,FontError>{
    let info=db.face(id).ok_or_else(||invalid("font_face","Prepared font face is unavailable."))?;
    let family=info.families.first().ok_or_else(||invalid("font_name","Font face has no family name."))?.0.clone();
    let (glyphs,units)=db.with_face_data(id,|bytes,index|ttf_parser::Face::parse(bytes,index).ok().map(|face|(face.number_of_glyphs(),face.units_per_em()))).flatten().ok_or_else(||invalid("font_face","Prepared font face metrics are invalid."))?;
    FontFace::new(info.index,family,info.post_script_name.clone(),info.weight.0,info.stretch.to_number(),match info.style{fontdb::Style::Normal=>FontFaceStyle::Normal,fontdb::Style::Italic=>FontFaceStyle::Italic,fontdb::Style::Oblique=>FontFaceStyle::Oblique},glyphs,units).map_err(|e|invalid("font_metadata",e.to_string()))
}
pub fn prepare_font(path:&Path)->Result<FontCatalog,FontError>{let mut budget=FontReadBudget::new(MAX_FONT_FILE_BYTES+1);prepare_font_with_budget(path,&mut budget)}
pub(crate) fn prepare_font_with_budget(path:&Path,budget:&mut FontReadBudget)->Result<FontCatalog,FontError>{
    if !path.is_absolute()||path.to_str().is_none_or(|p|p.len()>4096||p.chars().any(char::is_control)){return Err(invalid("font_path","Choose an absolute font path of at most4096 UTF-8 bytes."));}
    if path.extension().and_then(|v|v.to_str()).is_none_or(|v|!["ttf","otf","ttc"].iter().any(|ext|v.eq_ignore_ascii_case(ext))){return Err(invalid("font_format","Choose a static TrueType outline font (TTF or TTC). CFF OpenType fonts are not supported."));}
    let path=fs::canonicalize(path).map_err(|e|unavailable("font_io",format!("Font {}: {e}",path.display())))?;
    let path=path.to_str().filter(|p|p.len()<=4096&&!p.chars().any(char::is_control)).ok_or_else(||invalid("font_path","The resolved font path exceeds its UTF-8 path limit."))?.to_owned();
    let bytes=read(Path::new(&path),budget,None)?;
    let len=bytes.len() as u64;let hash:[u8;32]=Sha256::digest(&bytes).into();let(db,count,_)=parsed(bytes,budget,None)?;
    let mut faces=Vec::new();faces.try_reserve_exact(count as usize).map_err(|_|limit("Cannot allocate the font face catalog."))?;
    for info in db.faces(){let face=descriptor(&db,info.id)?;faces.push(FontAsset::new(path.clone(),hash,len,count,face).map_err(|e|invalid("font_metadata",e.to_string()))?);}
    faces.sort_by_key(|asset|asset.face().index());Ok(FontCatalog{faces})
}
pub(crate) fn load(asset:&FontAsset,budget:&mut FontReadBudget)->Result<Loaded,FontError>{
    let bytes=read(Path::new(asset.path()),budget,Some(asset.encoded_bytes()))?;let hash:[u8;32]=Sha256::digest(&bytes).into();
    if &hash!=asset.sha256(){return Err(unavailable("font_changed","The saved font content changed. Load / Refresh and Apply to accept it; no fallback font was used."));}
    let(db,count,costs)=parsed(bytes,budget,Some(asset.face().index()))?;
    if count!=asset.face_count(){return Err(invalid("font_metadata","Font face count disagrees with the saved identity."));}
    let info=db.faces().find(|face|face.index==asset.face().index()).ok_or_else(||unavailable("font_face","The saved font face is unavailable."))?;
    if &descriptor(&db,info.id)?!=asset.face(){return Err(invalid("font_metadata","Selected font face metadata disagrees with the saved identity."));}
    // The one exact face is the entire database. No system or Noto fallback is
    // installed, and no other TTC sibling can replace the selected face.
    let mut selected=fontdb::Database::new();let mut exact=info.clone();
    // Private lookup alias only: the actual selected metadata was verified
    // above. One fixed alias lets the shaper forbid a second fallback pass.
    exact.families=vec![(crate::text::font_shape::SELECTED_FAMILY.to_owned(),fontdb::Language::English_UnitedStates)];
    selected.push_face_info(exact);
    let id=selected.faces().next().ok_or_else(||unavailable("font_face","Selected font face could not be installed."))?.id;
    Ok(Loaded{db:selected,id,costs:costs.ok_or_else(||invalid("font_glyph","Selected font outline admission is unavailable."))?})
}
