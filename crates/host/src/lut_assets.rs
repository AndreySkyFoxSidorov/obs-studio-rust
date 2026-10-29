//! Bounded LUT I/O/parser, called only on an existing host-owned worker.
//! Exact byte snapshot SHA/length is part of the authored resource identity.
use skylive_core::{LutAsset,LutFormat};
use skylive_media::{ColorLut,LutKind};
use sha2::{Digest,Sha256};
use image::{ImageDecoder,DynamicImage,ColorType};
use std::{fs,io::{Read,Cursor},path::Path,sync::Arc};
const FILE_BYTES:u64=32*1024*1024;
#[derive(Debug,Clone)]
pub struct LutError {kind:crate::resource_issue::ResourceIssueKind,code:&'static str,detail:String}
impl LutError {pub fn kind(&self)->crate::resource_issue::ResourceIssueKind{self.kind}pub fn code(&self)->&'static str{self.code}}
impl std::fmt::Display for LutError {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(&self.detail)}}
impl std::error::Error for LutError {}
fn invalid(code:&'static str,detail:impl Into<String>)->LutError{LutError {kind:crate::resource_issue::ResourceIssueKind::Invalid,code,detail:detail.into()}}
fn unavailable(code:&'static str,detail:impl Into<String>)->LutError{LutError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,code,detail:detail.into()}}
fn limit(detail:impl Into<String>)->LutError{LutError {kind:crate::resource_issue::ResourceIssueKind::Limit,code:"lut_resource_limit",detail:detail.into()}}
fn io_error(path:&Path,error:std::io::Error)->LutError{unavailable("lut_io",format!("LUT {}: {error}",path.display()))}
pub struct LutReadBudget {remaining:u64}
impl LutReadBudget {pub fn new(bytes:u64)->Self{Self {remaining:bytes}}}
fn read(path:&Path,budget:&mut LutReadBudget,expected:Option<u64>)->Result<Vec<u8>,LutError>{
    if !path.is_absolute(){return Err(invalid("lut_absolute_path","LUT path must be absolute"));}
    let mut file=fs::File::open(path).map_err(|error|io_error(path,error))?;
    let len=file.metadata().map_err(|error|io_error(path,error))?.len();
    if len==0||len>FILE_BYTES||len>budget.remaining{return Err(limit("LUT encoded input exceeds the per-file or operation budget"));}
    if expected.is_some_and(|expected|expected!=len){return Err(unavailable("lut_changed",format!("LUT length changed: expected {expected:?}, actual {len}")));}
    let cap=(len+1).min(budget.remaining);let mut bytes=Vec::new();bytes.try_reserve_exact(cap as usize).map_err(|_|limit("LUT encoded snapshot allocation"))?;
    let result=(&mut file).take(cap).read_to_end(&mut bytes);budget.remaining-=bytes.len() as u64;
    result.map_err(|error|io_error(path,error))?;
    if bytes.len() as u64!=len||file.metadata().map_err(|error|io_error(path,error))?.len()!=len{return Err(unavailable("lut_changed","LUT changed while reading its byte snapshot"));}
    Ok(bytes)
}
pub fn prepare_lut(path:&Path)->Result<LutAsset,LutError>{prepare_lut_with_budget(path,&mut LutReadBudget::new(FILE_BYTES+1))}
pub fn prepare_lut_with_budget(path:&Path,budget:&mut LutReadBudget)->Result<LutAsset,LutError>{
    if !path.is_absolute(){return Err(invalid("lut_absolute_path","LUT path must be absolute"));}
    if path.to_str().is_none_or(|text|text.len()>4096||text.chars().any(char::is_control)){return Err(invalid("lut_path","LUT path exceeds4096 UTF-8 bytes or contains controls"));}
    let path=fs::canonicalize(path).map_err(|error|io_error(path,error))?;
    let text=path.to_str().filter(|text|text.len()<=4096&&!text.chars().any(char::is_control)).ok_or_else(||invalid("lut_path","LUT path exceeds4096 UTF-8 bytes or contains controls"))?;
    let bytes=read(&path,budget,None)?;let hash:[u8;32]=Sha256::digest(&bytes).into();
    let (format,size)=if extension(&path,"cube"){let parsed=cube(&bytes,false)?;(if parsed.shaper_size>0{LutFormat::CubeShaper3d}else if parsed.kind==LutKind::OneD{LutFormat::Cube1d}else{LutFormat::Cube3d},parsed.size)}else if extension(&path,"png"){let _=png(&bytes,false)?;(LutFormat::ObsPng64,64)}else{return Err(invalid("lut_format","Apply LUT supports .cube and static OBS 64³ PNG atlases only"));};
    let asset=LutAsset::new(text.to_owned(),hash,format,size,bytes.len() as u64).map_err(|error|invalid("lut_metadata",error.to_string()))?;
    if asset.table_bytes()>24*1024*1024{return Err(limit("LUT table exceeds the 24 MiB retained-data budget"));}
    Ok(asset)
}
pub(crate) fn load_lut(asset:&LutAsset,reads:&mut LutReadBudget,cached:Option<&Arc<ColorLut>>)->Result<Arc<ColorLut>,LutError>{
    let bytes=read(Path::new(asset.path()),reads,Some(asset.encoded_bytes()))?;
    let hash:[u8;32]=Sha256::digest(&bytes).into();
    if &hash!=asset.sha256(){return Err(unavailable("lut_changed",format!("LUT content changed: expected {:?}, actual {:?}; Refresh and Apply to accept the new contents",asset.sha256(),hash)));}
    if let Some(cached)=cached{return Ok(cached.clone());}
    let table=match asset.format(){
        LutFormat::Cube1d|LutFormat::Cube3d|LutFormat::CubeShaper3d=>{
            let data=cube(&bytes,true)?;
            let actual=if data.shaper_size>0{LutFormat::CubeShaper3d}else if data.kind==LutKind::OneD{LutFormat::Cube1d}else{LutFormat::Cube3d};
            if data.size!=asset.size()||actual!=asset.format(){return Err(invalid("lut_dimensions_changed","LUT dimensions disagree with the recorded resource"));}
            let values=data.values.ok_or_else(||invalid("lut_values","LUT value table missing"))?;
            if data.shaper_size>0 {ColorLut::new_shaper_3d(data.size,data.min[0],data.max[0],values,data.shaper_min,data.shaper_max,data.shaper_values.ok_or_else(||invalid("lut_values","CUBE shaper missing"))?)}
            else if data.resolve_single {ColorLut::new_resolve_single(data.kind,data.size,data.min[0],data.max[0],values)}
            else {ColorLut::new(data.kind,data.size,data.min,data.max,values)}.map_err(|error|invalid("lut_values",error.to_string()))?
        },
        LutFormat::ObsPng64=>ColorLut::new(LutKind::ThreeD,64,[0.0;3],[1.0;3],png(&bytes,true)?.ok_or_else(||invalid("lut_values","PNG LUT value table missing"))?).map_err(|error|invalid("lut_values",error.to_string()))?,
    };
    if table.byte_len()>asset.table_bytes(){return Err(limit("LUT allocation exceeds the admitted resource budget"));}
    Ok(Arc::new(table))
}
fn extension(path:&Path,ext:&str)->bool{path.extension().and_then(|value|value.to_str()).is_some_and(|value|value.eq_ignore_ascii_case(ext))}
struct Cube {kind:LutKind,size:u32,min:[f32;3],max:[f32;3],values:Option<Vec<[f32;3]>>,shaper_size:u32,shaper_min:f32,shaper_max:f32,shaper_values:Option<Vec<[f32;3]>>,resolve_single:bool}
fn range(line:&str)->Result<(f32,f32),LutError>{let mut v=line.split_whitespace();let a=v.next().and_then(|s|s.parse::<f32>().ok()).filter(|v|v.is_finite()).ok_or_else(||invalid("lut_cube_range","Invalid CUBE input range"))?;let b=v.next().and_then(|s|s.parse::<f32>().ok()).filter(|v|v.is_finite()).ok_or_else(||invalid("lut_cube_range","Invalid CUBE input range"))?;if v.next().is_some()||a>=b||!(b-a).is_finite(){return Err(invalid("lut_cube_range","CUBE input range must be finite and increasing"));}Ok((a,b))}
fn triple(line:&str)->Result<[f32;3],LutError>{
    let mut tokens=line.split_whitespace();let mut values=[0.0;3];
    for value in &mut values{let token=tokens.next().filter(|value|value.len()<=64).ok_or_else(||invalid("lut_cube_row","CUBE row requires three finite numbers"))?;*value=token.parse::<f32>().ok().filter(|v|v.is_finite()).ok_or_else(||invalid("lut_cube_row","CUBE row requires three finite numbers"))?;}
    if tokens.next().is_some(){return Err(invalid("lut_cube_row","CUBE row has unsupported extra fields"));}Ok(values)
}
fn clean(line:&str)->&str {line.split_once('#').map_or(line,|(prefix,_)|prefix).trim()}
fn cube(bytes:&[u8],decode:bool)->Result<Cube,LutError>{
    let text=std::str::from_utf8(bytes).map_err(|_|invalid("lut_cube_encoding","CUBE file must be UTF-8 text"))?;
    let mut size1=None;let mut size3=None;let mut min=None;let mut max=None;let mut range1=None;let mut range3=None;let mut data=false;let mut count=0usize;let mut start=0usize;
    for (index,line) in text.lines().enumerate(){
        if line.len()>4096{return Err(limit("CUBE line exceeds4096 bytes"));}
        let line=clean(line);if line.is_empty(){continue;}
        let (name,rest)=line.split_once(char::is_whitespace).map_or((line,""),|(a,b)|(a,b.trim()));
        if !data {
            match name {
                "TITLE"=>{if rest.len()>1024{return Err(limit("CUBE title exceeds1024 bytes"));}continue;},
                "LUT_1D_SIZE"|"LUT_3D_SIZE"=>{let size=rest.parse::<u32>().map_err(|_|invalid("lut_cube_size","Invalid CUBE size directive"))?;let target=if name=="LUT_1D_SIZE"{&mut size1}else{&mut size3};if target.replace(size).is_some(){return Err(invalid("lut_cube_header","Duplicate CUBE size directive"));}continue;},
                "DOMAIN_MIN"|"DOMAIN_MAX"=>{let target=if name=="DOMAIN_MIN"{&mut min}else{&mut max};if target.replace(triple(rest)?).is_some(){return Err(invalid("lut_cube_header","Duplicate CUBE domain directive"));}continue;},
                "LUT_1D_INPUT_RANGE"|"LUT_3D_INPUT_RANGE"=>{let target=if name=="LUT_1D_INPUT_RANGE"{&mut range1}else{&mut range3};if target.replace(range(rest)?).is_some(){return Err(invalid("lut_cube_header","Duplicate CUBE range directive"));}continue;},
                _=>{},
            }
            if name.chars().next().is_some_and(|c|c.is_ascii_alphabetic()){return Err(invalid("lut_cube_unsupported","Unsupported CUBE directive or combined format"));}
            start=index;data=true;
        }
        let values=triple(line)?;
        if !(size1.is_some()&&size3.is_some()&&count<size1.unwrap_or(0) as usize){for value in values{skylive_media::cube_lut_linear_value(value).map_err(|_|invalid("lut_cube_range","CUBE values must produce finite binary16 linear colors"))?;}}
        count=count.checked_add(1).filter(|count|*count<=128usize.pow(3)+65536).ok_or_else(||limit("CUBE row count exceeds bound"))?;
    }
    let (kind,size,shaper_size)=match (size1,size3){(Some(size),None)if(2..=65536).contains(&size)=>(LutKind::OneD,size,0),(None,Some(size))if(2..=128).contains(&size)=>(LutKind::ThreeD,size,0),(Some(one),Some(three))if(2..=65536).contains(&one)&&(2..=128).contains(&three)=>(LutKind::ThreeD,three,one),_=>return Err(invalid("lut_cube_size","CUBE requires supported 1D and/or 3D sizes"))};
    // Explicit Resolve ranges require clamping and nonlinear interpolation,
    // distinct from OBS's existing Iridas out-of-domain behavior.
    let resolve_single=shaper_size==0&&(range1.is_some()||range3.is_some());
    if (min.is_some()||max.is_some())&&(range1.is_some()||range3.is_some()) {return Err(invalid("lut_cube_header","Cannot combine DOMAIN_MIN/MAX with Resolve input ranges"));}
    if shaper_size>0 && (min.is_some()||max.is_some()) {return Err(invalid("lut_cube_header","Combined CUBE requires Resolve input ranges"));}
    if range1.is_some()&&size1.is_none()||range3.is_some()&&size3.is_none(){return Err(invalid("lut_cube_header","CUBE range has no corresponding table"));}
    let expected=(if kind==LutKind::OneD{size as usize}else{(size as usize).pow(3)})+shaper_size as usize;
    if !data||count!=expected{return Err(invalid("lut_cube_rows",format!("CUBE row count mismatch: expected {expected}, actual {count}")));}
    let selected_range=if kind==LutKind::OneD{range1}else{range3};
    let min=min.unwrap_or([selected_range.map_or(0.0,|v|v.0);3]);let max=max.unwrap_or([selected_range.map_or(1.0,|v|v.1);3]);
    for i in 0..3 {let scale=(size-1) as f32/(max[i]-min[i]);let offset=(-min[i]*scale+0.5)/size as f32;if min[i]>=max[i]||!scale.is_finite()||!offset.is_finite(){return Err(invalid("lut_cube_domain","CUBE domain must be finite, increasing and representable"));}}
    let (values,shaper_values)=if decode {
        let mut values=Vec::new();values.try_reserve_exact(expected-shaper_size as usize).map_err(|_|limit("CUBE table allocation"))?;
        let mut shaper=Vec::new();shaper.try_reserve_exact(shaper_size as usize).map_err(|_|limit("CUBE shaper allocation"))?;
        for line in text.lines().skip(start){let line=clean(line);if line.is_empty(){continue;}let row=triple(line)?;if shaper.len()<shaper_size as usize{shaper.push(row);continue;}if shaper_size>0||resolve_single{values.push(row);}else{let mut linear=[0.0;3];for i in 0..3{linear[i]=skylive_media::cube_lut_linear_value(row[i]).map_err(|_|invalid("lut_cube_range","CUBE values must produce finite binary16 linear colors"))?;}values.push(linear);}}
        (Some(values),Some(shaper))
    }else{(None,None)};
    let (shaper_min,shaper_max)=range1.unwrap_or((0.0,1.0));
    Ok(Cube {kind,size,min,max,values,shaper_size,shaper_min,shaper_max,shaper_values,resolve_single})
}
fn png(bytes:&[u8],decode:bool)->Result<Option<Vec<[f32;3]>>,LutError>{
    let mut limits=image::Limits::default();limits.max_image_width=Some(4096);limits.max_image_height=Some(4096);limits.max_alloc=Some(4*1024*1024);
    let mut decoder=image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes),limits.clone()).map_err(|error|invalid("lut_png",error.to_string()))?;
    let (width,height)=decoder.dimensions();
    // OBS packs 64 blue slices into any 64-pixel tile grid with 64³ pixels.
    let color=decoder.color_type();
    let high_precision=matches!(color,ColorType::L16|ColorType::La16|ColorType::Rgb16|ColorType::Rgba16);
    if decoder.is_apng().map_err(|error|invalid("lut_png",error.to_string()))?||width%64!=0||height%64!=0||u64::from(width)*u64::from(height)!=64u64.pow(3)||!(high_precision||matches!(color,ColorType::L8|ColorType::La8|ColorType::Rgb8|ColorType::Rgba8)){return Err(invalid("lut_png_profile","Image LUT requires a static 8- or 16-bit PNG with dimensions divisible by 64 and 64³ pixels"));}
    if decoder.total_bytes()>512*512*8{return Err(limit("PNG LUT native raster exceeds 2 MiB"));}
    decoder.set_limits(limits).map_err(|error|invalid("lut_png",error.to_string()))?;
    let image=DynamicImage::from_decoder(decoder).map_err(|error|invalid("lut_png",error.to_string()))?;
    if !decode{return Ok(None);}
    let mut table=Vec::new();table.try_reserve_exact(64usize.pow(3)).map_err(|_|limit("PNG LUT table allocation"))?;
    // OBS lays out blue slices left to right, then down; red and green
    // index the pixel inside each 64×64 tile.
    let width=width as usize;let columns=width/64;
    if high_precision {
        let rgba=image.into_rgba16();
        for z in 0..64usize{let x=(z%columns)*64;let y=(z/columns)*64;for g in 0..64usize{for red in 0..64usize{let at=((y+g)*width+x+red)*4;let pixel=&rgba.as_raw()[at..at+4];table.push([skylive_media::png_lut_linear_value_16(pixel[0]),skylive_media::png_lut_linear_value_16(pixel[1]),skylive_media::png_lut_linear_value_16(pixel[2])]);}}}
    } else {
        let rgba=image.into_rgba8();
        for z in 0..64usize{let x=(z%columns)*64;let y=(z/columns)*64;for g in 0..64usize{for red in 0..64usize{let at=((y+g)*width+x+red)*4;let pixel=&rgba.as_raw()[at..at+4];table.push([skylive_media::png_lut_linear_value(pixel[0]),skylive_media::png_lut_linear_value(pixel[1]),skylive_media::png_lut_linear_value(pixel[2])]);}}}
    }
    Ok(Some(table))
}
