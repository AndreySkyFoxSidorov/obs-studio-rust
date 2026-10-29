#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(all(target_os="windows",target_arch="x86_64"))]mod cef;
#[cfg(all(target_os="windows",target_arch="x86_64"))]mod osr;
fn main(){
 #[cfg(all(target_os="windows",target_arch="x86_64"))] {if let Err(error)=osr::run(){eprintln!("SkyLive CEF helper: {error}");std::process::exit(1);}}
 #[cfg(not(all(target_os="windows",target_arch="x86_64")))] {eprintln!("Browser Source is implemented only for Windowsx64");std::process::exit(2);}
}
