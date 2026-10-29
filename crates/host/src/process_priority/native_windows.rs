//! Kernel32 process-priority calls, exclusively on the owning host worker.
//! GetCurrentProcess returns a pseudo-handle: it is never CloseHandle'd.
use super::*;
use std::{marker::PhantomData,rc::Rc};
use windows::Win32::{Foundation::{HANDLE,GetLastError},System::Threading::*};
pub(super) struct Native{process:HANDLE,original:u32,applied:Option<u32>,retired:bool,shared:Arc<Shared>,_owner:PhantomData<Rc<()>>}
fn get(process:HANDLE)->Result<u32,String>{
    // SAFETY: process is this thread owner's current-process pseudo-handle.
    let value=unsafe{GetPriorityClass(process)};
    if value==0{let code=unsafe{GetLastError()}.0;Err(format!("GetPriorityClass failed: Win32 error {code}"))}else{Ok(value)}
}
fn set(process:HANDLE,value:u32)->Result<(),String>{
    // SAFETY: only this current process is targeted. UI choices exclude real
    // time/background flags; restoration uses the exact previously read class.
    unsafe{SetPriorityClass(process,PROCESS_CREATION_FLAGS(value))}.map_err(|e|format!("SetPriorityClass({value}) failed: HRESULT {}",e.code().0))
}
fn class(priority:ProcessPriority)->u32{match priority{ProcessPriority::High=>HIGH_PRIORITY_CLASS.0,ProcessPriority::AboveNormal=>ABOVE_NORMAL_PRIORITY_CLASS.0,ProcessPriority::Normal=>NORMAL_PRIORITY_CLASS.0,ProcessPriority::BelowNormal=>BELOW_NORMAL_PRIORITY_CLASS.0,ProcessPriority::Idle=>IDLE_PRIORITY_CLASS.0}}
impl Native{
    pub fn new(shared:Arc<Shared>)->Result<Self,String>{
        // SAFETY: read-only current process pseudo-handle; no native operation
        // is performed by the GUI or for another PID/process.
        let process=unsafe{GetCurrentProcess()};let original=get(process)?;Ok(Self{process,original,applied:None,retired:false,shared,_owner:PhantomData})
    }
    pub fn apply(&mut self,priority:ProcessPriority,receipt:&mut Receipt,current:&impl Fn()->bool){
        let target=class(priority);let before=match get(self.process){Ok(value)=>value,Err(error)=>{receipt.error=Some(error);return;}};receipt.before=Some(before);
        if !current(){receipt.error=Some("Process priority request was superseded before native write.".into());return;}
        if before!=target {
            if let Err(error)=set(self.process,target){receipt.error=Some(error);return;}
            self.applied=Some(target);receipt.written=true;receipt.phase=Phase::AfterWrite;
        }
        let actual=match get(self.process){Ok(value)=>value,Err(error)=>{receipt.error=Some(error);return;}};receipt.readback=Some(actual);
        if !current(){receipt.phase=if receipt.written{Phase::AfterWrite}else{Phase::BeforeWrite};receipt.error=Some(if receipt.written{"Process priority result became obsolete; a successful external write is not undone."}else{"Process priority request was superseded before native write."}.into());return;}
        if actual!=target{receipt.error=Some(format!("Process priority readback differs: requested class {target}, actual class {actual}"));return;}
        receipt.phase=Phase::ReadBack;
    }
    pub fn restore(&mut self)->Restore{
        self.retired=true;
        let mut result=Restore{original:Some(self.original),owned:self.applied,observed:None,restored:false,skipped_external_change:false,error:None};
        let Some(owned)=self.applied else{return result;};
        let current=match get(self.process){Ok(value)=>value,Err(error)=>{result.error=Some(error);return result;}};result.observed=Some(current);
        if current!=owned{result.skipped_external_change=true;return result;}
        // WinAPI has no compare-and-set priority operation. Compare immediately
        // before Set on the sole owner; never blindly overwrite a class already
        // observed to have changed externally. No retry after foreign changes.
        if current!=self.original{if let Err(error)=set(self.process,self.original){result.error=Some(error);return result;}}
        match get(self.process){Ok(value)=>{result.observed=Some(value);if value==self.original{result.restored=true;}else{result.error=Some(format!("Priority restore readback changed: original {}, actual {value}",self.original));}},Err(error)=>result.error=Some(error)}result
    }
}

impl Drop for Native{fn drop(&mut self){if !self.retired{let result=self.restore();if let Some(error)=&result.error{eprintln!("process priority unwind restore: {error}; original={:?}; owned={:?}; observed={:?}",result.original,result.owned,result.observed);}put(&self.shared,|s|s.restore=Some(result));}}}
