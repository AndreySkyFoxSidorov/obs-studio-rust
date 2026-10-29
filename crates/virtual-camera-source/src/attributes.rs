//! IMFActivate's native attribute store preserves FrameServer configuration.
use super::*;
impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self,key:*const GUID,value:*mut PROPVARIANT)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetItem)(a.as_raw(),key,value.cast()).ok()}}
    fn GetItemType(&self,key:*const GUID)->Result<MF_ATTRIBUTE_TYPE>{unsafe{self.attributes.resolve()?.GetItemType(key)}}
    fn CompareItem(&self,key:*const GUID,value:*const PROPVARIANT)->Result<BOOL>{unsafe{self.attributes.resolve()?.CompareItem(key,value)}}
    fn Compare(&self,theirs:Option<&IMFAttributes>,kind:MF_ATTRIBUTES_MATCH_TYPE)->Result<BOOL>{unsafe{self.attributes.resolve()?.Compare(theirs,kind)}}
    fn GetUINT32(&self,key:*const GUID)->Result<u32>{unsafe{self.attributes.resolve()?.GetUINT32(key)}}
    fn GetUINT64(&self,key:*const GUID)->Result<u64>{unsafe{self.attributes.resolve()?.GetUINT64(key)}}
    fn GetDouble(&self,key:*const GUID)->Result<f64>{unsafe{self.attributes.resolve()?.GetDouble(key)}}
    fn GetGUID(&self,key:*const GUID)->Result<GUID>{unsafe{self.attributes.resolve()?.GetGUID(key)}}
    fn GetStringLength(&self,key:*const GUID)->Result<u32>{unsafe{self.attributes.resolve()?.GetStringLength(key)}}
    fn GetString(&self,key:*const GUID,value:PWSTR,size:u32,length:*mut u32)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetString)(a.as_raw(),key,value,size,length).ok()}}
    fn GetAllocatedString(&self,key:*const GUID,value:*mut PWSTR,length:*mut u32)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetAllocatedString)(a.as_raw(),key,value,length).ok()}}
    fn GetBlobSize(&self,key:*const GUID)->Result<u32>{unsafe{self.attributes.resolve()?.GetBlobSize(key)}}
    fn GetBlob(&self,key:*const GUID,value:*mut u8,size:u32,length:*mut u32)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetBlob)(a.as_raw(),key,value,size,length).ok()}}
    fn GetAllocatedBlob(&self,key:*const GUID,value:*mut *mut u8,size:*mut u32)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetAllocatedBlob)(a.as_raw(),key,value,size).ok()}}
    fn GetUnknown(&self,key:*const GUID,iid:*const GUID,value:*mut *mut c_void)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetUnknown)(a.as_raw(),key,iid,value).ok()}}
    fn SetItem(&self,key:*const GUID,value:*const PROPVARIANT)->Result<()> {unsafe{self.attributes.resolve()?.SetItem(key,value)}}
    fn DeleteItem(&self,key:*const GUID)->Result<()> {unsafe{self.attributes.resolve()?.DeleteItem(key)}}
    fn DeleteAllItems(&self)->Result<()> {unsafe{self.attributes.resolve()?.DeleteAllItems()}}
    fn SetUINT32(&self,key:*const GUID,value:u32)->Result<()> {unsafe{self.attributes.resolve()?.SetUINT32(key,value)}}
    fn SetUINT64(&self,key:*const GUID,value:u64)->Result<()> {unsafe{self.attributes.resolve()?.SetUINT64(key,value)}}
    fn SetDouble(&self,key:*const GUID,value:f64)->Result<()> {unsafe{self.attributes.resolve()?.SetDouble(key,value)}}
    fn SetGUID(&self,key:*const GUID,value:*const GUID)->Result<()> {unsafe{self.attributes.resolve()?.SetGUID(key,value)}}
    fn SetString(&self,key:*const GUID,value:&PCWSTR)->Result<()> {unsafe{self.attributes.resolve()?.SetString(key,*value)}}
    fn SetBlob(&self,key:*const GUID,value:*const u8,size:u32)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().SetBlob)(a.as_raw(),key,value,size).ok()}}
    fn SetUnknown(&self,key:*const GUID,value:Option<&IUnknown>)->Result<()> {unsafe{self.attributes.resolve()?.SetUnknown(key,value)}}
    fn LockStore(&self)->Result<()> {unsafe{self.attributes.resolve()?.LockStore()}}
    fn UnlockStore(&self)->Result<()> {unsafe{self.attributes.resolve()?.UnlockStore()}}
    fn GetCount(&self)->Result<u32> {unsafe{self.attributes.resolve()?.GetCount()}}
    fn GetItemByIndex(&self,index:u32,key:*mut GUID,value:*mut PROPVARIANT)->Result<()> {let a=self.attributes.resolve()?;unsafe{(a.vtable().GetItemByIndex)(a.as_raw(),index,key,value.cast()).ok()}}
    fn CopyAllItems(&self,destination:Option<&IMFAttributes>)->Result<()> {unsafe{self.attributes.resolve()?.CopyAllItems(destination)}}
}
