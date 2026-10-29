//! Session-local derived source definition epochs; never serialized or undone.
use crate::{Error,Id,MAX_SOURCES,StudioSnapshot};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct SourceDefinitionEpoch(u64);
pub(crate) struct SourceDefinitions {entries:Vec<(Id,SourceDefinitionEpoch)>,next:u64}
impl SourceDefinitions {
    pub(crate) fn empty()->Self{Self{entries:Vec::new(),next:0}}
    pub(crate) fn epoch(&self,id:Id)->Option<SourceDefinitionEpoch>{self.entries.binary_search_by_key(&id,|entry|entry.0).ok().map(|index|self.entries[index].1)}
    /// Prepare fallibly before canonical/history publication. Only an unchanged
    /// current definition retains identity; an undone/restored definition is new.
    pub(crate) fn prepare(&self,old:Option<&StudioSnapshot>,candidate:&StudioSnapshot)->Result<Self,Error>{
        if candidate.sources().len()>MAX_SOURCES{return Err(Error::Limit("Source definition epoch table exceeds source capacity."));}
        let mut entries=Vec::new();entries.try_reserve_exact(candidate.sources().len()).map_err(|_|Error::Limit("Cannot allocate bounded source definition epochs."))?;
        let mut next=self.next;
        for source in candidate.sources(){
            let retained=old.and_then(|old|old.source(source.id())).filter(|previous|previous.kind()==source.kind()&&previous.image_settings()==source.image_settings()&&previous.same_shortcut_definition(source)).and_then(|_|self.epoch(source.id()));
            let epoch=match retained{Some(epoch)=>epoch,None=>{next=next.checked_add(1).ok_or(Error::IdentityExhausted)?;SourceDefinitionEpoch(next)}};
            entries.push((source.id(),epoch));
        }
        entries.sort_unstable_by_key(|entry|entry.0);Ok(Self{entries,next})
    }
    pub(crate) fn clear(&mut self){self.entries.clear();}
}
