//! Installed Windows font discovery. Called by a bounded file worker or import
//! preparation, never by Core or the GUI. Results carry exact prepared faces.
use crate::font_assets::{FontError,FontReadBudget};
use skylive_core::{FontAsset,FontFaceStyle};
use std::path::PathBuf;

const MAX_VALUES:usize=4096;
const MAX_FILES:usize=2048;
const MAX_CATALOG_BYTES:usize=4*1024*1024;
const MAX_READ_BYTES:u64=256*1024*1024;
#[derive(Clone,Debug,serde::Serialize)]
pub struct SystemFontCatalog{pub faces:Vec<FontAsset>,pub skipped:u32,pub complete:bool}
fn unavailable(detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"system_font_catalog",detail:detail.into()}}
fn limit(detail:impl Into<String>)->FontError{FontError{kind:crate::resource_issue::ResourceIssueKind::Limit,code:"system_font_catalog_limit",detail:detail.into()}}

/// Exact OBS style: a standard regular/bold weight and normal/italic face.
/// Other weights, widths or oblique faces are not silently reinterpreted.
pub fn resolve_obs(catalog:&SystemFontCatalog,family:&str,bold:bool,italic:bool)->Result<FontAsset,FontError>{
    if !catalog.complete{return Err(limit("Installed font discovery was incomplete; cannot resolve an unambiguous OBS family."));}
    let mut matches=catalog.faces.iter().filter(|asset|{
        let face=asset.face();face.family().eq_ignore_ascii_case(family)&&face.weight()==if bold{700}else{400}
            &&face.stretch()==5&&face.style()==if italic{FontFaceStyle::Italic}else{FontFaceStyle::Normal}
    });
    let Some(first)=matches.next()else{return Err(unavailable(format!("Installed font family '{family}' with the requested OBS style is missing or unsupported.")));};
    if matches.next().is_some(){return Err(unavailable(format!("Installed font family '{family}' has multiple matching faces; choose one explicitly in Text Properties.")));}
    Ok(first.clone())
}

#[cfg(not(target_os="windows"))]
pub fn discover()->Result<SystemFontCatalog,FontError>{Err(unavailable("System font discovery is currently supported on Windows only."))}

#[cfg(target_os="windows")]
pub fn discover()->Result<SystemFontCatalog,FontError>{
    let (paths,mut skipped,mut complete)=windows::installed_paths()?;
    let mut budget=FontReadBudget::new(MAX_READ_BYTES);
    let mut faces=Vec::new();let mut bytes=0usize;let mut stop=false;
    for path in paths{
        match crate::font_assets::prepare_font_with_budget(&path,&mut budget){
            Ok(catalog)=>for asset in catalog.faces{
                let added=std::mem::size_of::<FontAsset>().saturating_add(asset.path().len()).saturating_add(asset.face().family().len()).saturating_add(asset.face().post_script_name().len());
                if bytes.checked_add(added).is_none_or(|n|n>MAX_CATALOG_BYTES){complete=false;stop=true;break;}
                bytes+=added;faces.push(asset);
            },
            Err(_error)=>{skipped=skipped.saturating_add(1);if budget.exhausted(){complete=false;break;}},
        }
        if stop{break;}
    }
    faces.sort_by(|a,b|a.face().family().cmp(b.face().family()).then(a.face().weight().cmp(&b.face().weight())).then(a.face().style().cmp_style(b.face().style())).then(a.path().cmp(b.path())).then(a.face().index().cmp(&b.face().index())));
    faces.dedup();
    // A skipped registration/file might contain another face of any family.
    // Registry display names are not authoritative face-family metadata, so
    // absence and uniqueness cannot be established from a partial catalog.
    if skipped!=0{complete=false;}
    Ok(SystemFontCatalog{faces,skipped,complete})
}

trait StyleOrder{fn cmp_style(self,other:Self)->std::cmp::Ordering;}
impl StyleOrder for FontFaceStyle{fn cmp_style(self,other:Self)->std::cmp::Ordering{(self as u8).cmp(&(other as u8))}}

#[cfg(target_os="windows")]
mod windows{
    use super::*;
    use ::windows::{core::{PCWSTR,PWSTR},Win32::{Foundation::{ERROR_FILE_NOT_FOUND,ERROR_NO_MORE_ITEMS,ERROR_SUCCESS},System::Registry::{RegCloseKey,RegEnumValueW,RegOpenKeyExW,HKEY,HKEY_CURRENT_USER,HKEY_LOCAL_MACHINE,KEY_READ,REG_SZ}}};
    struct Key(HKEY);
    impl Drop for Key{fn drop(&mut self){unsafe{let _=RegCloseKey(self.0);}}}
    fn root(name:&str,suffix:&str)->Result<PathBuf,FontError>{let value=std::env::var_os(name).ok_or_else(||unavailable(format!("Missing {name} for installed font directory.")))?;let base=PathBuf::from(value);if !base.is_absolute(){return Err(unavailable("Installed font directory is not absolute."));}Ok(base.join(suffix))}
    pub(super) fn installed_paths()->Result<(Vec<PathBuf>,u32,bool),FontError>{
        let machine=root("WINDIR","Fonts")?;let user=root("LOCALAPPDATA","Microsoft/Windows/Fonts").ok();
        let sub:Vec<u16>="SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts\0".encode_utf16().collect();
        let mut paths=Vec::new();let mut skipped=0u32;let mut count=0usize;let mut bytes=0usize;let mut complete=true;
        for (hive,folder) in [(HKEY_LOCAL_MACHINE,Some(&machine)),(HKEY_CURRENT_USER,user.as_ref())]{
            let mut raw=HKEY::default();let result=unsafe{RegOpenKeyExW(hive,PCWSTR(sub.as_ptr()),0,KEY_READ,&mut raw)};
            if result==ERROR_FILE_NOT_FOUND&&hive==HKEY_CURRENT_USER{continue;}
            if result!=ERROR_SUCCESS{return Err(unavailable(format!("Cannot open Windows font registration: {}",result.0)));}
            let key=Key(raw);
            for index in 0..=MAX_VALUES{
                if count>=MAX_VALUES{complete=false;break;}
                let mut name=[0u16;1024];let mut value=[0u8;8192];let mut name_len=name.len() as u32;let mut value_len=value.len() as u32;let mut kind=0u32;
                let status=unsafe{RegEnumValueW(key.0,index as u32,PWSTR(name.as_mut_ptr()),&mut name_len,None,Some(&mut kind),Some(value.as_mut_ptr()),Some(&mut value_len))};
                if status==ERROR_NO_MORE_ITEMS{break;}
                count+=1;
                if status!=ERROR_SUCCESS||kind!=REG_SZ.0||value_len%2!=0||value_len as usize>value.len(){skipped=skipped.saturating_add(1);continue;}
                let mut wide:Vec<u16>=value[..value_len as usize].chunks_exact(2).map(|b|u16::from_le_bytes([b[0],b[1]])).collect();
                if wide.last()!=Some(&0)||wide[..wide.len().saturating_sub(1)].contains(&0){skipped=skipped.saturating_add(1);continue;}
                wide.pop();
                let Ok(text)=String::from_utf16(&wide)else{skipped=skipped.saturating_add(1);continue;};
                if text.is_empty()||text.len()>4096||text.chars().any(char::is_control){skipped=skipped.saturating_add(1);continue;}
                let file=PathBuf::from(&text);let file=if file.is_absolute(){file}else{
                    if file.components().count()!=1{skipped=skipped.saturating_add(1);continue;}
                    let Some(folder)=folder else{skipped=skipped.saturating_add(1);continue;};folder.join(file)
                };
                let Some(display)=file.to_str()else{skipped=skipped.saturating_add(1);continue;};
                if paths.len()>=MAX_FILES||bytes.saturating_add(display.len())>2*1024*1024{complete=false;break;}
                if !paths.iter().any(|p:&PathBuf|p==&file){bytes+=display.len();paths.push(file);}
            }
            if !complete{break;}
        }
        Ok((paths,skipped,complete))
    }
}
