//! Targeted Windows DirectWrite resolution for OBS GDI Text imports.
//! Native handles and file fragments stay on the import worker.
use crate::font_assets::{FontError,FontReadBudget};
use skylive_core::FontAsset;
use std::collections::HashMap;

const MAX_REQUESTS:usize=64;
const MAX_READ_BYTES:u64=64*1024*1024;
fn fail(code:&'static str,detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code,detail:detail.into()}}
fn limit(detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Limit,code:"obs_system_font_limit",detail:detail.into()}}

pub(crate) struct Resolver{
    cache:HashMap<(String,bool,bool),FontAsset>,
    budget:FontReadBudget,
    #[cfg(target_os="windows")]
    collection: ::windows::Win32::Graphics::DirectWrite::IDWriteFontCollection,
}
impl Resolver{
    #[cfg(target_os="windows")]
    pub(crate) fn new()->Result<Self,FontError>{
        use ::windows::Win32::{Foundation::BOOL,Graphics::DirectWrite::{DWriteCreateFactory,IDWriteFactory,DWRITE_FACTORY_TYPE_SHARED}};
        let factory:IDWriteFactory=unsafe{DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)}.map_err(|error|fail("obs_system_font_api",error.to_string()))?;
        let mut collection=None;
        unsafe{factory.GetSystemFontCollection(&mut collection,BOOL(0))}.map_err(|error|fail("obs_system_font_api",error.to_string()))?;
        Ok(Self{cache:HashMap::new(),budget:FontReadBudget::new(MAX_READ_BYTES),collection:collection.ok_or_else(||fail("obs_system_font_api","DirectWrite returned no system font collection."))?})
    }
    #[cfg(not(target_os="windows"))]
    pub(crate) fn new()->Result<Self,FontError>{Err(fail("obs_system_font_platform","OBS system-font resolution is currently available only on Windows."))}
    pub(crate) fn resolve(&mut self,family:&str,bold:bool,italic:bool)->Result<FontAsset,FontError>{
        if family.trim().is_empty()||family.len()>256||family.chars().any(char::is_control){return Err(fail("obs_system_font_name","Invalid OBS font family name."));}
        if let Some(asset)=self.cache.get(&(family.to_owned(),bold,italic)){return Ok(asset.clone());}
        if self.cache.len()>=MAX_REQUESTS{return Err(limit("OBS collection exceeds 64 distinct font family/style selections."));}
        #[cfg(target_os="windows")]
        {
            let asset=windows::resolve(&self.collection,&mut self.budget,family,bold,italic)?;
            self.cache.insert((family.to_owned(),bold,italic),asset.clone());Ok(asset)
        }
        #[cfg(not(target_os="windows"))]
        {Err(fail("obs_system_font_platform","OBS system-font resolution is currently available only on Windows."))}
    }
}

#[cfg(target_os="windows")]
mod windows{
    use super::*;
    use sha2::{Digest,Sha256};
    use std::{ffi::c_void,path::PathBuf};
    use ::windows::{core::{Interface,PCWSTR},Win32::{Foundation::BOOL,Graphics::DirectWrite::{IDWriteFont,IDWriteFontCollection,IDWriteFontFileStream,IDWriteLocalFontFileLoader,DWRITE_FONT_SIMULATIONS_NONE,DWRITE_FONT_STRETCH_NORMAL,DWRITE_FONT_STYLE_ITALIC,DWRITE_FONT_STYLE_NORMAL,DWRITE_FONT_WEIGHT_BOLD,DWRITE_FONT_WEIGHT_NORMAL}}};

    fn api(error: ::windows::core::Error)->FontError{fail("obs_system_font_api",error.to_string())}
    struct Fragment<'a>{stream:&'a IDWriteFontFileStream,context:*mut c_void}
    impl Drop for Fragment<'_>{fn drop(&mut self){unsafe{self.stream.ReleaseFileFragment(self.context);}}}
    fn hash_stream(stream:&IDWriteFontFileStream,length:u64)->Result<[u8;32],FontError>{
        let mut hash=Sha256::new();let mut offset=0u64;
        while offset<length{
            let count=(length-offset).min(64*1024);let mut data=std::ptr::null_mut();let mut context=std::ptr::null_mut();
            unsafe{stream.ReadFileFragment(&mut data,offset,count,&mut context)}.map_err(api)?;
            let _fragment=Fragment{stream,context};
            if data.is_null(){return Err(fail("obs_system_font_stream","DirectWrite returned an empty font fragment."));}
            // DirectWrite guarantees this fragment remains readable until ReleaseFileFragment.
            let bytes=unsafe{std::slice::from_raw_parts(data.cast::<u8>(),count as usize)};
            hash.update(bytes);offset+=count;
        }
        Ok(hash.finalize().into())
    }
    pub(super) fn resolve(collection:&IDWriteFontCollection,budget:&mut FontReadBudget,family:&str,bold:bool,italic:bool)->Result<FontAsset,FontError>{
        let name:Vec<u16>=family.encode_utf16().chain(std::iter::once(0)).collect();let mut index=0;let mut exists=BOOL(0);
        unsafe{collection.FindFamilyName(PCWSTR(name.as_ptr()),&mut index,&mut exists)}.map_err(api)?;
        if !exists.as_bool(){return Err(fail("obs_system_font_missing",format!("The installed family '{family}' is unavailable.")));}
        let group=unsafe{collection.GetFontFamily(index)}.map_err(api)?;let count=unsafe{group.GetFontCount()};
        if count>256{return Err(limit("The selected DirectWrite family exceeds 256 faces."));}
        let weight=if bold{DWRITE_FONT_WEIGHT_BOLD}else{DWRITE_FONT_WEIGHT_NORMAL};let style=if italic{DWRITE_FONT_STYLE_ITALIC}else{DWRITE_FONT_STYLE_NORMAL};
        let mut selected:Option<IDWriteFont>=None;
        for i in 0..count{
            let font=unsafe{group.GetFont(i)}.map_err(api)?;
            if unsafe{font.GetWeight()}==weight&&unsafe{font.GetStretch()}==DWRITE_FONT_STRETCH_NORMAL&&unsafe{font.GetStyle()}==style&&unsafe{font.GetSimulations()}==DWRITE_FONT_SIMULATIONS_NONE{
                if selected.is_some(){return Err(fail("obs_system_font_ambiguous",format!("The installed family '{family}' has multiple exact matching faces.")));}
                selected=Some(font);
            }
        }
        let font=selected.ok_or_else(||fail("obs_system_font_style",format!("The installed family '{family}' has no exact non-synthetic requested style.")))?;
        let face=unsafe{font.CreateFontFace()}.map_err(api)?;
        if unsafe{face.GetSimulations()}!=DWRITE_FONT_SIMULATIONS_NONE{return Err(fail("obs_system_font_style","Synthetic font faces are not supported."));}
        let face_index=unsafe{face.GetIndex()};let glyphs=unsafe{face.GetGlyphCount()};let mut count=0;
        unsafe{face.GetFiles(&mut count,None)}.map_err(api)?;
        if count!=1{return Err(fail("obs_system_font_files","Only a single local font file per face is supported."));}
        let mut files=[None];unsafe{face.GetFiles(&mut count,Some(files.as_mut_ptr()))}.map_err(api)?;
        if count!=1{return Err(fail("obs_system_font_files","The DirectWrite font file changed during selection."));}
        let file=files[0].take().ok_or_else(||fail("obs_system_font_files","DirectWrite returned no font file."))?;
        let mut key=std::ptr::null_mut();let mut key_size=0;
        unsafe{file.GetReferenceKey(&mut key,&mut key_size)}.map_err(api)?;
        if key.is_null()||key_size==0||key_size>4096{return Err(fail("obs_system_font_files","Invalid or oversized DirectWrite file reference."));}
        let loader=unsafe{file.GetLoader()}.map_err(api)?;
        let local:IDWriteLocalFontFileLoader=loader.cast().map_err(|_|fail("obs_system_font_files","The selected system font does not expose a local file."))?;
        let length=unsafe{local.GetFilePathLengthFromKey(key,key_size)}.map_err(api)? as usize;
        if length==0||length>4096{return Err(limit("The selected system font path is too long."));}
        let mut path=vec![0u16;length+1];unsafe{local.GetFilePathFromKey(key,key_size,&mut path)}.map_err(api)?;
        if path[length]!=0||path[..length].contains(&0){return Err(fail("obs_system_font_files","The font file path is invalid."));}
        let path=String::from_utf16(&path[..length]).map_err(|_|fail("obs_system_font_files","The font file path is not valid UTF-16."))?;
        if path.len()>4096||path.chars().any(char::is_control){return Err(limit("The font path exceeds its UTF-8 metadata limit."));}
        let path=PathBuf::from(path);if !path.is_absolute(){return Err(fail("obs_system_font_files","The selected system font path is not absolute."));}
        let stream=unsafe{loader.CreateStreamFromKey(key,key_size)}.map_err(api)?;
        let bytes=unsafe{stream.GetFileSize()}.map_err(api)?;
        if bytes==0||bytes>skylive_core::MAX_FONT_FILE_BYTES{return Err(limit("The selected system font exceeds the supported 16 MiB file profile."));}
        budget.charge_external(bytes)?;
        let expected_hash=hash_stream(&stream,bytes)?;
        let catalog=crate::font_assets::prepare_font_with_budget(&path,budget)?;
        let selected=catalog.faces.into_iter().find(|asset|asset.face().index()==face_index).ok_or_else(||fail("obs_system_font_face","The DirectWrite collection face is not in the prepared file."))?;
        let exact_style=if italic{skylive_core::FontFaceStyle::Italic}else{skylive_core::FontFaceStyle::Normal};
        if selected.encoded_bytes()!=bytes||selected.sha256()!=&expected_hash||selected.face().glyphs()!=glyphs||selected.face().weight()!=if bold{700}else{400}||selected.face().stretch()!=5||selected.face().style()!=exact_style{
            return Err(fail("obs_system_font_changed","DirectWrite face and prepared font file disagree; the file may have changed."));
        }
        Ok(selected)
    }
}
