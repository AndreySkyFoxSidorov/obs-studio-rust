//! SkyLive's own D3D11 hook DLL. No OBS binary or native detour library.
//! The exported start is called only after explicit target authorization.
#[cfg(all(target_os="windows",target_arch="x86_64"))]
mod windows;
#[unsafe(no_mangle)]
pub static SKYLIVE_GAME_HOOK_ABI:u64=skylive_game_capture_protocol::MAGIC^skylive_game_capture_protocol::ABI;
