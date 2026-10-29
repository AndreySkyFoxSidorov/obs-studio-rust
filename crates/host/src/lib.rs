//! Native adapters. Domain and media never import this crate.
pub mod help_links;
pub mod document_repair;
pub mod missing_files;
pub mod screenshot;
pub mod remux;
mod private_output;
pub mod assets;
pub mod lut_assets;
pub mod native_path_dialog;
pub mod image_slideshow;
pub mod audio;
pub mod audio_capture;
pub mod audio_output;
pub mod audio_program;
pub mod audio_monitor;
pub mod camera_capture;
pub mod camera_activation;
pub mod capture;
pub mod source_output_resize;
pub mod localization;
pub mod media_source;
pub mod game_capture;
pub mod global_hotkeys;
pub mod ndi;
pub mod browser;
pub mod mediafile;
pub mod native_aac;
pub mod native_h264;
pub mod obs;
pub mod playback;
pub mod mask_animation_clock;
mod mask_live_versions;
pub mod projector;
pub mod scroll_animation;
pub mod preview;
pub mod render_gpu;
mod freeze_timing_state;
pub mod freeze_timing_book;
pub mod freeze_timing_collect;
mod freeze_refresh_plan;
pub mod recording;
pub mod replay_buffer;
pub mod resource_issue;
pub mod rtmp;
pub mod storage;
pub mod collection_library;
pub mod transition;
pub mod user_preferences;
pub mod last_native_document;
pub mod profile_io;
pub mod process_statistics;
pub mod process_priority;

pub mod preview_interaction;
pub mod window_capture;

pub mod text;
pub mod font_assets;
pub mod system_fonts;
mod obs_text_fonts;

mod video_filters;

#[cfg(all(test, target_os = "windows"))]
mod native_packet_evidence;

pub mod streaming;
pub mod virtual_camera;

#[cfg(feature = "gpu-filters")]
pub mod gpu_color_correction;
