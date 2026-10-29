//! Bounded Unicode adaptation of OBS Text's authored case controls.
use crate::{Error,MAX_TEXT_BYTES,MAX_TEXT_CHARS};
use serde::{Deserialize,Serialize};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum TextCaseTransform {#[default] None,Uppercase,Lowercase,Startcase}
impl TextCaseTransform {
    pub const ALL:[Self;4]=[Self::None,Self::Uppercase,Self::Lowercase,Self::Startcase];
    pub const fn label(self)->&'static str{match self{Self::None=>"None",Self::Uppercase=>"Uppercase",Self::Lowercase=>"Lowercase",Self::Startcase=>"Start Case"}}
    pub(crate) fn visit(self,text:&str,mut emit:impl FnMut(char)->Result<(),Error>)->Result<(),Error>{
        let mut first=true;
        for value in text.chars(){match self{
            Self::None=>emit(value)?,
            Self::Uppercase=>{for mapped in value.to_uppercase(){emit(mapped)?;}},
            Self::Lowercase=>{for mapped in value.to_lowercase(){emit(mapped)?;}},
            Self::Startcase=>{
                if value.to_lowercase().ne(value.to_uppercase()){
                    if first{for mapped in value.to_uppercase(){emit(mapped)?;}}
                    else{for mapped in value.to_lowercase(){emit(mapped)?;}}
                    first=false;
                }else{first=value.is_whitespace();emit(value)?;}
            },
        }}Ok(())
    }
    pub(crate) fn measure(self,text:&str)->Result<usize,Error>{
        let(mut bytes,mut chars)=(0usize,0usize);
        self.visit(text,|value|{
            bytes=bytes.checked_add(value.len_utf8()).ok_or(Error::Limit("Transformed text size overflow."))?;
            chars+=1;
            if bytes>MAX_TEXT_BYTES||chars>MAX_TEXT_CHARS{return Err(Error::Limit("Transformed text exceeds 16 KiB or 4096 characters."));}
            Ok(())
        })?;Ok(bytes)
    }
}
