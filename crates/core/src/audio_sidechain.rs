//! Weak, source-owned audio detector references. No device activation here.
use super::*;
impl Collection {
    pub(super) fn validate_audio_sidechains(&self)->Result<(),Error>{
        for source in &self.sources{
            for filter in &source.audio_filters{
                let Some(key)=filter.sidechain_source()else{continue;};
                if key==source.id{return Err(Error::Invalid("compressor sidechain cannot reference its own source"));}
                if key.get()>=self.next_id{return Err(Error::Invalid("compressor sidechain identity was never issued"));}
                if self.source(key).is_some_and(|target|!target.kind.supports_audio_settings()){
                    return Err(Error::Invalid("compressor sidechain requires an audio-capable source"));
                }
            }
        }
        let mut done=HashSet::new();
        for source in &self.sources{
            if done.contains(&source.id){continue;}
            let mut stack=vec![(source.id,0usize)];let mut visiting=HashSet::new();visiting.insert(source.id);
            while let Some((id,index))=stack.last_mut(){
                let target=self.source(*id).and_then(|node|node.audio_filters.iter().filter_map(|filter|filter.sidechain_source()).nth(*index));
                match target{
                    Some(key)=>{*index+=1;if self.source(key).is_none()||done.contains(&key){continue;}
                        if !visiting.insert(key){return Err(Error::Invalid("cyclic compressor sidechains are unsupported"));}
                        if stack.len()>=MAX_SOURCES{return Err(Error::Limit("compressor sidechain graph depth"));}
                        stack.push((key,0));
                    },
                    None=>{let (id,_)=stack.pop().unwrap();visiting.remove(&id);done.insert(id);}
                }
            }
        }
        Ok(())
    }
    pub(super) fn validate_sidechain_assignment(&self,owner:Id,filters:&[AudioFilter],retained:&[AudioFilter])->Result<(),Error>{
        for key in filters.iter().filter_map(|filter|filter.sidechain_source()){
            if key==owner{return Err(Error::Invalid("compressor sidechain cannot reference its own source"));}
            match self.source(key){
                Some(target) if target.kind.supports_audio_settings()=>{},
                Some(_)=>return Err(Error::Invalid("compressor sidechain requires an audio-capable source")),
                None if retained.iter().any(|filter|filter.sidechain_source()==Some(key))=>{},
                None=>return Err(Error::NotFound(key)),
            }
        }
        Ok(())
    }
}
