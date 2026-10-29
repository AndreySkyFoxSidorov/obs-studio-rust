//! Final publication of a closed, synced private file. Worker I/O only.
use std::{fs,io,path::Path};
pub(crate) struct Publication {source_name_remains:bool}
impl Publication {
    /// Called only after successful publication; failure is an after-publication
    /// cleanup error. A Windows move already removed the private source name.
    pub(crate) fn remove_temporary(self,source:&Path)->io::Result<()>{
        if self.source_name_remains{fs::remove_file(source)}else{Ok(())}
    }
}
pub(crate) fn publish(source:&Path,destination:&Path)->io::Result<Publication>{
    if source==destination{return Err(io::Error::new(io::ErrorKind::InvalidInput,"private and final output paths must differ"));}
    #[cfg(target_os="windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{core::PCWSTR,Win32::Storage::FileSystem::{MoveFileExW,MOVEFILE_WRITE_THROUGH}};
        // Canonical existing parents supply Windows extended-length prefixes.
        // Resolve the destination parent, never an existing destination itself.
        let from=fs::canonicalize(source)?;
        let parent=destination.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let name=destination.file_name().ok_or_else(||io::Error::new(io::ErrorKind::InvalidInput,"final output has no filename"))?;
        let to=fs::canonicalize(parent)?.join(name);
        let wide=|path:&Path|->io::Result<Vec<u16>>{let mut value:Vec<u16>=path.as_os_str().encode_wide().collect();if value.contains(&0)||value.len()>32766{return Err(io::Error::new(io::ErrorKind::InvalidInput,"output path exceeds Windows Unicode path bounds"));}value.push(0);Ok(value)};
        let from=wide(&from)?;let to=wide(&to)?;
        // No REPLACE_EXISTING or COPY_ALLOWED: destination collisions and
        // cross-volume attempts fail, retaining the complete private file.
        unsafe{MoveFileExW(PCWSTR(from.as_ptr()),PCWSTR(to.as_ptr()),MOVEFILE_WRITE_THROUGH)}.map_err(|error|{
            let value=error.code().0 as u32;
            if value&0xffff0000==0x80070000{io::Error::from_raw_os_error((value&0xffff) as i32)}else{io::Error::other(error.to_string())}
        })?;
        Ok(Publication {source_name_remains:false})
    }
    #[cfg(not(target_os="windows"))]
    {fs::hard_link(source,destination)?;Ok(Publication {source_name_remains:true})}
}
