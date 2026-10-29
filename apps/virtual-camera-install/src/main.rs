//! Explicit future UAC installer entry; never launched on startup or Start.
fn main()->std::process::ExitCode {
    #[cfg(all(target_os="windows",target_arch="x86_64"))]
    let result=(||->std::io::Result<()> {
        let args:Vec<_>=std::env::args_os().skip(1).collect();
        if args.len()!=1 || args[0]!="--install" {return Err(std::io::Error::other("explicit --install argument required"));}
        let source=std::env::current_exe()?.parent().ok_or_else(||std::io::Error::other("installer directory"))?.join(skylive_virtual_camera_protocol::DLL_NAME);
        let installed=skylive_virtual_camera_protocol::installation::install_bundle(&source)?;
        println!("Registered Rust virtual camera source: {}",installed.display());Ok(())
    })();
    #[cfg(not(all(target_os="windows",target_arch="x86_64")))]
    let result:std::io::Result<()>=Err(std::io::Error::other("Windows11 x64 required"));
    match result {Ok(())=>std::process::ExitCode::SUCCESS,Err(error)=>{let code=if error.kind()==std::io::ErrorKind::PermissionDenied{"virtual_camera_admin_required"}else{"virtual_camera_install_failed"};eprintln!("{code}: {error}");std::process::ExitCode::FAILURE}}
}
