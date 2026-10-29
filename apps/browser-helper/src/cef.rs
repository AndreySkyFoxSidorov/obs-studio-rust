/*
// Copyright (c) 2008-2020 Marshall A. Greenblatt. Portions Copyright (c)
// 2006-2009 Google Inc. All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are
// met:
//
//    * Redistributions of source code must retain the above copyright
// notice, this list of conditions and the following disclaimer.
//    * Redistributions in binary form must reproduce the above
// copyright notice, this list of conditions and the following disclaimer
// in the documentation and/or other materials provided with the
// distribution.
//    * Neither the name of Google Inc. nor the name Chromium Embedded
// Framework nor the names of its contributors may be used to endorse
// or promote products derived from this software without specific prior
// written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
// OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
// THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

*/
//! C ABI layouts from CEF2b7d20b8, Windowsx64 API hash2e66390b5f0fa21ab4667b503e76164037cd674e.
//! Derived from BSD-licensed CEF headers retained under staging/browser-source/references/cef127.
#![allow(dead_code)]
use std::{ffi::c_void,mem::size_of};
#[repr(C)] pub struct Base {pub size:usize,pub add_ref:Option<unsafe extern "system" fn(*mut Base)>,pub release:Option<unsafe extern "system" fn(*mut Base)->i32>,pub has_one_ref:Option<unsafe extern "system" fn(*mut Base)->i32>,pub has_at_least_one_ref:Option<unsafe extern "system" fn(*mut Base)->i32>}
#[repr(C)] #[derive(Clone,Copy,Default)] pub struct CefString {pub str_:*mut u16,pub length:usize,pub dtor:Option<unsafe extern "system" fn(*mut u16)>}
#[repr(C)] #[derive(Clone,Copy,Default)] pub struct Rect {pub x:i32,pub y:i32,pub width:i32,pub height:i32}
#[repr(C)] #[derive(Clone,Copy,Default)] pub struct MouseEvent {pub x:i32,pub y:i32,pub modifiers:u32}
#[repr(C)] #[derive(Clone,Copy,Default)] pub struct KeyEvent {pub kind:i32,pub modifiers:u32,pub windows_key_code:i32,pub native_key_code:i32,pub is_system_key:i32,pub character:u16,pub unmodified_character:u16,pub focus_on_editable_field:i32}
#[repr(C)] #[derive(Clone,Copy,Default)] pub struct ScreenInfo {pub device_scale_factor:f32,pub depth:i32,pub depth_per_component:i32,pub is_monochrome:i32,pub rect:Rect,pub available_rect:Rect}
#[repr(C)] pub struct MainArgs {pub instance:*mut c_void}
#[repr(C)] pub struct WindowInfo {pub ex_style:u32,pub window_name:CefString,pub style:u32,pub bounds:Rect,pub parent_window:*mut c_void,pub menu:*mut c_void,pub windowless_rendering_enabled:i32,pub shared_texture_enabled:i32,pub external_begin_frame_enabled:i32,pub window:*mut c_void,pub runtime_style:i32}
pub trait Capi:Sized {fn base(&mut self)->&mut Base;}
// These two callback tables retain every field in the pinned CEF127 headers.
#[repr(C)] pub struct AppApi {pub base:Base,pub on_before_command_line_processing:usize,pub on_register_custom_schemes:usize,pub get_resource_bundle_handler:usize,pub get_browser_process_handler:Option<unsafe extern "system" fn(*mut AppApi)->*mut BrowserProcessHandler>,pub get_render_process_handler:usize}
#[repr(C)] pub struct BrowserProcessHandler {pub base:Base,pub on_register_custom_preferences:usize,pub on_context_initialized:Option<unsafe extern "system" fn(*mut BrowserProcessHandler)>,pub on_before_child_process_launch:usize,pub on_already_running_app_relaunch:Option<unsafe extern "system" fn(*mut BrowserProcessHandler,*mut c_void,*const CefString)->i32>,pub on_schedule_message_pump_work:usize,pub get_default_client:usize,pub get_default_request_context_handler:usize}
#[repr(C)] pub struct Browser {pub base:Base,
 pub is_valid: usize,
 pub get_host: Option<unsafe extern "system" fn(*mut Browser)->*mut BrowserHost>,
 pub can_go_back: usize,
 pub go_back: usize,
 pub can_go_forward: usize,
 pub go_forward: usize,
 pub is_loading: usize,
 pub reload: usize,
 pub reload_ignore_cache: Option<unsafe extern "system" fn(*mut Browser)>,
 pub stop_load: usize,
 pub get_identifier: usize,
 pub is_same: usize,
 pub is_popup: usize,
 pub has_document: usize,
 pub get_main_frame: Option<unsafe extern "system" fn(*mut Browser)->*mut Frame>,
 pub get_focused_frame: usize,
 pub get_frame_by_identifier: usize,
 pub get_frame_by_name: usize,
 pub get_frame_count: usize,
 pub get_frame_identifiers: usize,
 pub get_frame_names: usize,
}
impl Capi for Browser {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct BrowserHost {pub base:Base,
 pub get_browser: usize,
 pub close_browser: Option<unsafe extern "system" fn(*mut BrowserHost,i32)>,
 pub try_close_browser: usize,
 pub set_focus: Option<unsafe extern "system" fn(*mut BrowserHost,i32)>,
 pub get_window_handle: usize,
 pub get_opener_window_handle: usize,
 pub has_view: usize,
 pub get_client: usize,
 pub get_request_context: usize,
 pub can_zoom: usize,
 pub zoom: usize,
 pub get_default_zoom_level: usize,
 pub get_zoom_level: usize,
 pub set_zoom_level: usize,
 pub run_file_dialog: usize,
 pub start_download: usize,
 pub download_image: usize,
 pub print: usize,
 pub print_to_pdf: usize,
 pub find: usize,
 pub stop_finding: usize,
 pub show_dev_tools: usize,
 pub close_dev_tools: usize,
 pub has_dev_tools: usize,
 pub send_dev_tools_message: usize,
 pub execute_dev_tools_method: usize,
 pub add_dev_tools_message_observer: usize,
 pub get_navigation_entries: usize,
 pub replace_misspelling: usize,
 pub add_word_to_dictionary: usize,
 pub is_window_rendering_disabled: usize,
 pub was_resized: usize,
 pub was_hidden: Option<unsafe extern "system" fn(*mut BrowserHost,i32)>,
 pub notify_screen_info_changed: usize,
 pub invalidate: Option<unsafe extern "system" fn(*mut BrowserHost,i32)>,
 pub send_external_begin_frame: usize,
 pub send_key_event: Option<unsafe extern "system" fn(*mut BrowserHost,*const KeyEvent)>,
 pub send_mouse_click_event: Option<unsafe extern "system" fn(*mut BrowserHost,*const MouseEvent,i32,i32,i32)>,
 pub send_mouse_move_event: Option<unsafe extern "system" fn(*mut BrowserHost,*const MouseEvent,i32)>,
 pub send_mouse_wheel_event: Option<unsafe extern "system" fn(*mut BrowserHost,*const MouseEvent,i32,i32)>,
 pub send_touch_event: usize,
 pub send_capture_lost_event: usize,
 pub notify_move_or_resize_started: usize,
 pub get_windowless_frame_rate: usize,
 pub set_windowless_frame_rate: Option<unsafe extern "system" fn(*mut BrowserHost,i32)>,
 pub ime_set_composition: usize,
 pub ime_commit_text: usize,
 pub ime_finish_composing_text: usize,
 pub ime_cancel_composition: usize,
 pub drag_target_drag_enter: usize,
 pub drag_target_drag_over: usize,
 pub drag_target_drag_leave: usize,
 pub drag_target_drop: usize,
 pub drag_source_ended_at: usize,
 pub drag_source_system_drag_ended: usize,
 pub get_visible_navigation_entry: usize,
 pub set_accessibility_state: usize,
 pub set_auto_resize_enabled: usize,
 pub get_extension: usize,
 pub is_background_host: usize,
 pub set_audio_muted: Option<unsafe extern "system" fn(*mut BrowserHost,i32)>,
 pub is_audio_muted: Option<unsafe extern "system" fn(*mut BrowserHost)->i32>,
 pub is_fullscreen: usize,
 pub exit_fullscreen: usize,
 pub can_execute_chrome_command: usize,
 pub execute_chrome_command: usize,
 pub is_render_process_unresponsive: usize,
 pub get_runtime_style: usize,
}
impl Capi for BrowserHost {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct Client {pub base:Base,
 pub get_audio_handler: Option<unsafe extern "system" fn(*mut Client)->*mut AudioHandler>,
 pub get_command_handler: usize,
 pub get_context_menu_handler: Option<unsafe extern "system" fn(*mut Client)->*mut ContextMenuHandler>,
 pub get_dialog_handler: Option<unsafe extern "system" fn(*mut Client)->*mut DialogHandler>,
 pub get_display_handler: usize,
 pub get_download_handler: Option<unsafe extern "system" fn(*mut Client)->*mut DownloadHandler>,
 pub get_drag_handler: usize,
 pub get_find_handler: usize,
 pub get_focus_handler: usize,
 pub get_frame_handler: usize,
 pub get_permission_handler: Option<unsafe extern "system" fn(*mut Client)->*mut PermissionHandler>,
 pub get_jsdialog_handler: Option<unsafe extern "system" fn(*mut Client)->*mut JsdialogHandler>,
 pub get_keyboard_handler: usize,
 pub get_life_span_handler: Option<unsafe extern "system" fn(*mut Client)->*mut LifeSpanHandler>,
 pub get_load_handler: Option<unsafe extern "system" fn(*mut Client)->*mut LoadHandler>,
 pub get_print_handler: usize,
 pub get_render_handler: Option<unsafe extern "system" fn(*mut Client)->*mut RenderHandler>,
 pub get_request_handler: Option<unsafe extern "system" fn(*mut Client)->*mut RequestHandler>,
 pub on_process_message_received: usize,
}
impl Capi for Client {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct RenderHandler {pub base:Base,
 pub get_accessibility_handler: usize,
 pub get_root_screen_rect: usize,
 pub get_view_rect: Option<unsafe extern "system" fn(*mut RenderHandler,*mut Browser,*mut Rect)>,
 pub get_screen_point: usize,
 pub get_screen_info: Option<unsafe extern "system" fn(*mut RenderHandler,*mut Browser,*mut ScreenInfo)->i32>,
 pub on_popup_show: Option<unsafe extern "system" fn(*mut RenderHandler,*mut Browser,i32)>,
 pub on_popup_size: Option<unsafe extern "system" fn(*mut RenderHandler,*mut Browser,*const Rect)>,
 pub on_paint: Option<unsafe extern "system" fn(*mut RenderHandler,*mut Browser,i32,usize,*const Rect,*const c_void,i32,i32)>,
 pub on_accelerated_paint: usize,
 pub get_touch_handle_size: usize,
 pub on_touch_handle_state_changed: usize,
 pub start_dragging: usize,
 pub update_drag_cursor: usize,
 pub on_scroll_offset_changed: usize,
 pub on_ime_composition_range_changed: usize,
 pub on_text_selection_changed: usize,
 pub on_virtual_keyboard_requested: usize,
}
impl Capi for RenderHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct LifeSpanHandler {pub base:Base,
 pub on_before_popup: Option<unsafe extern "system" fn(*mut LifeSpanHandler,*mut Browser,*mut Frame,*const CefString,*const CefString,i32,i32,*const c_void,*mut WindowInfo,*mut *mut Client,*mut BrowserSettings,*mut *mut c_void,*mut i32)->i32>,
 pub on_before_dev_tools_popup: usize,
 pub on_after_created: usize,
 pub do_close: usize,
 pub on_before_close: Option<unsafe extern "system" fn(*mut LifeSpanHandler,*mut Browser)>,
}
impl Capi for LifeSpanHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct LoadHandler {pub base:Base,
 pub on_loading_state_change: usize,
 pub on_load_start: usize,
 pub on_load_end: Option<unsafe extern "system" fn(*mut LoadHandler,*mut Browser,*mut Frame,i32)>,
 pub on_load_error: Option<unsafe extern "system" fn(*mut LoadHandler,*mut Browser,*mut Frame,i32,*const CefString,*const CefString)>,
}
impl Capi for LoadHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct Frame {pub base:Base,
 pub is_valid: usize,
 pub undo: usize,
 pub redo: usize,
 pub cut: usize,
 pub copy: usize,
 pub paste: usize,
 pub del: usize,
 pub select_all: usize,
 pub view_source: usize,
 pub get_source: usize,
 pub get_text: usize,
 pub load_request: usize,
 pub load_url: Option<unsafe extern "system" fn(*mut Frame,*const CefString)>,
 pub execute_java_script: Option<unsafe extern "system" fn(*mut Frame,*const CefString,*const CefString,i32)>,
 pub is_main: Option<unsafe extern "system" fn(*mut Frame)->i32>,
 pub is_focused: usize,
 pub get_name: usize,
 pub get_identifier: usize,
 pub get_parent: usize,
 pub get_url: Option<unsafe extern "system" fn(*mut Frame)->*mut CefString>,
 pub get_browser: usize,
 pub get_v8context: usize,
 pub visit_dom: usize,
 pub create_urlrequest: usize,
 pub send_process_message: usize,
}
impl Capi for Frame {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct JsdialogHandler {pub base:Base,
 pub on_jsdialog: Option<unsafe extern "system" fn(*mut JsdialogHandler,*mut Browser,*const CefString,i32,*const CefString,*const CefString,*mut JsdialogCallback,*mut i32)->i32>,
 pub on_before_unload_dialog: Option<unsafe extern "system" fn(*mut JsdialogHandler,*mut Browser,*const CefString,i32,*mut JsdialogCallback)->i32>,
 pub on_reset_dialog_state: usize,
 pub on_dialog_closed: usize,
}
impl Capi for JsdialogHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct JsdialogCallback {pub base:Base,
 pub cont: Option<unsafe extern "system" fn(*mut JsdialogCallback,i32,*const CefString)>,
}
impl Capi for JsdialogCallback {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct DialogHandler {pub base:Base,
 pub on_file_dialog: Option<unsafe extern "system" fn(*mut DialogHandler,*mut Browser,i32,*const CefString,*const CefString,*mut c_void,*mut c_void,*mut c_void,*mut FileDialogCallback)->i32>,
}
impl Capi for DialogHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct FileDialogCallback {pub base:Base,
 pub cont: usize,
 pub cancel: Option<unsafe extern "system" fn(*mut FileDialogCallback)>,
}
impl Capi for FileDialogCallback {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct ContextMenuHandler {pub base:Base,
 pub on_before_context_menu: Option<unsafe extern "system" fn(*mut ContextMenuHandler,*mut Browser,*mut Frame,*mut c_void,*mut MenuModel)>,
 pub run_context_menu: Option<unsafe extern "system" fn(*mut ContextMenuHandler,*mut Browser,*mut Frame,*mut c_void,*mut MenuModel,*mut RunContextMenuCallback)->i32>,
 pub on_context_menu_command: usize,
 pub on_context_menu_dismissed: usize,
 pub run_quick_menu: usize,
 pub on_quick_menu_command: usize,
 pub on_quick_menu_dismissed: usize,
}
impl Capi for ContextMenuHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct RunContextMenuCallback {pub base:Base,
 pub cont: usize,
 pub cancel: Option<unsafe extern "system" fn(*mut RunContextMenuCallback)>,
}
impl Capi for RunContextMenuCallback {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct DownloadHandler {pub base:Base,
 pub can_download: Option<unsafe extern "system" fn(*mut DownloadHandler,*mut Browser,*const CefString,*const CefString)->i32>,
 pub on_before_download: Option<unsafe extern "system" fn(*mut DownloadHandler,*mut Browser,*mut c_void,*const CefString,*mut c_void)->i32>,
 pub on_download_updated: Option<unsafe extern "system" fn(*mut DownloadHandler,*mut Browser,*mut c_void,*mut DownloadItemCallback)>,
}
impl Capi for DownloadHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct DownloadItemCallback {pub base:Base,
 pub cancel: Option<unsafe extern "system" fn(*mut DownloadItemCallback)>,
 pub pause: usize,
 pub resume: usize,
}
impl Capi for DownloadItemCallback {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct RequestHandler {pub base:Base,
 pub on_before_browse: usize,
 pub on_open_urlfrom_tab: Option<unsafe extern "system" fn(*mut RequestHandler,*mut Browser,*mut Frame,*const CefString,i32,i32)->i32>,
 pub get_resource_request_handler: Option<unsafe extern "system" fn(*mut RequestHandler,*mut Browser,*mut Frame,*mut c_void,i32,i32,*const CefString,*mut i32)->*mut ResourceRequestHandler>,
 pub get_auth_credentials: usize,
 pub on_certificate_error: usize,
 pub on_select_client_certificate: usize,
 pub on_render_view_ready: usize,
 pub on_render_process_unresponsive: usize,
 pub on_render_process_responsive: usize,
 pub on_render_process_terminated: usize,
 pub on_document_available_in_main_frame: usize,
}
impl Capi for RequestHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct ResourceRequestHandler {pub base:Base,
 pub get_cookie_access_filter: usize,
 pub on_before_resource_load: usize,
 pub get_resource_handler: usize,
 pub on_resource_redirect: usize,
 pub on_resource_response: usize,
 pub get_resource_response_filter: usize,
 pub on_resource_load_complete: usize,
 pub on_protocol_execution: Option<unsafe extern "system" fn(*mut ResourceRequestHandler,*mut Browser,*mut Frame,*mut c_void,*mut i32)>,
}
impl Capi for ResourceRequestHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct PermissionHandler {pub base:Base,
 pub on_request_media_access_permission: Option<unsafe extern "system" fn(*mut PermissionHandler,*mut Browser,*mut Frame,*const CefString,u32,*mut MediaAccessCallback)->i32>,
 pub on_show_permission_prompt: Option<unsafe extern "system" fn(*mut PermissionHandler,*mut Browser,u64,*const CefString,u32,*mut PermissionPromptCallback)->i32>,
 pub on_dismiss_permission_prompt: usize,
}
impl Capi for PermissionHandler {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct MediaAccessCallback {pub base:Base,
 pub cont: usize,
 pub cancel: Option<unsafe extern "system" fn(*mut MediaAccessCallback)>,
}
impl Capi for MediaAccessCallback {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct PermissionPromptCallback {pub base:Base,
 pub cont: Option<unsafe extern "system" fn(*mut PermissionPromptCallback,i32)>,
}
impl Capi for PermissionPromptCallback {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)] pub struct MenuModel {pub base:Base,
 pub is_sub_menu: usize,
 pub clear: Option<unsafe extern "system" fn(*mut MenuModel)->i32>,
 pub get_count: usize,
 pub add_separator: usize,
 pub add_item: usize,
 pub add_check_item: usize,
 pub add_radio_item: usize,
 pub add_sub_menu: usize,
 pub insert_separator_at: usize,
 pub insert_item_at: usize,
 pub insert_check_item_at: usize,
 pub insert_radio_item_at: usize,
 pub insert_sub_menu_at: usize,
 pub remove: usize,
 pub remove_at: usize,
 pub get_index_of: usize,
 pub get_command_id_at: usize,
 pub set_command_id_at: usize,
 pub get_label: usize,
 pub get_label_at: usize,
 pub set_label: usize,
 pub set_label_at: usize,
 pub get_type: usize,
 pub get_type_at: usize,
 pub get_group_id: usize,
 pub get_group_id_at: usize,
 pub set_group_id: usize,
 pub set_group_id_at: usize,
 pub get_sub_menu: usize,
 pub get_sub_menu_at: usize,
 pub is_visible: usize,
 pub is_visible_at: usize,
 pub set_visible: usize,
 pub set_visible_at: usize,
 pub is_enabled: usize,
 pub is_enabled_at: usize,
 pub set_enabled: usize,
 pub set_enabled_at: usize,
 pub is_checked: usize,
 pub is_checked_at: usize,
 pub set_checked: usize,
 pub set_checked_at: usize,
 pub has_accelerator: usize,
 pub has_accelerator_at: usize,
 pub set_accelerator: usize,
 pub set_accelerator_at: usize,
 pub remove_accelerator: usize,
 pub remove_accelerator_at: usize,
 pub get_accelerator: usize,
 pub get_accelerator_at: usize,
 pub set_color: usize,
 pub set_color_at: usize,
 pub get_color: usize,
 pub get_color_at: usize,
 pub set_font_list: usize,
 pub set_font_list_at: usize,
}
impl Capi for MenuModel {fn base(&mut self)->&mut Base {&mut self.base}}
#[repr(C)]#[derive(Clone,Copy)]pub struct AudioParameters{pub channel_layout:i32,pub sample_rate:i32,pub frames_per_buffer:i32}
#[repr(C)]pub struct AudioHandler{pub base:Base,pub get_audio_parameters:Option<unsafe extern "system" fn(*mut AudioHandler,*mut Browser,*mut AudioParameters)->i32>,pub on_audio_stream_started:Option<unsafe extern "system" fn(*mut AudioHandler,*mut Browser,*const AudioParameters,i32)>,pub on_audio_stream_packet:Option<unsafe extern "system" fn(*mut AudioHandler,*mut Browser,*const *const f32,i32,i64)>,pub on_audio_stream_stopped:Option<unsafe extern "system" fn(*mut AudioHandler,*mut Browser)>,pub on_audio_stream_error:Option<unsafe extern "system" fn(*mut AudioHandler,*mut Browser,*const CefString)>}
impl Capi for AudioHandler{fn base(&mut self)->&mut Base{&mut self.base}}
#[repr(C)] pub struct Settings {
 pub size:usize,
 pub no_sandbox:i32,
 pub browser_subprocess_path:CefString,
 pub framework_dir_path:CefString,
 pub main_bundle_path:CefString,
 pub chrome_runtime:i32,
 pub multi_threaded_message_loop:i32,
 pub external_message_pump:i32,
 pub windowless_rendering_enabled:i32,
 pub command_line_args_disabled:i32,
 pub cache_path:CefString,
 pub root_cache_path:CefString,
 pub persist_session_cookies:i32,
 pub persist_user_preferences:i32,
 pub user_agent:CefString,
 pub user_agent_product:CefString,
 pub locale:CefString,
 pub log_file:CefString,
 pub log_severity:i32,
 pub log_items:i32,
 pub javascript_flags:CefString,
 pub resources_dir_path:CefString,
 pub locales_dir_path:CefString,
 pub pack_loading_disabled:i32,
 pub remote_debugging_port:i32,
 pub uncaught_exception_stack_size:i32,
 pub background_color:u32,
 pub accept_language_list:CefString,
 pub cookieable_schemes_list:CefString,
 pub cookieable_schemes_exclude_defaults:i32,
 pub chrome_policy_id:CefString,
 pub chrome_app_icon_id:i32,
}
#[repr(C)] pub struct BrowserSettings {
 pub size:usize,
 pub windowless_frame_rate:i32,
 pub standard_font_family:CefString,
 pub fixed_font_family:CefString,
 pub serif_font_family:CefString,
 pub sans_serif_font_family:CefString,
 pub cursive_font_family:CefString,
 pub fantasy_font_family:CefString,
 pub default_font_size:i32,
 pub default_fixed_font_size:i32,
 pub minimum_font_size:i32,
 pub minimum_logical_font_size:i32,
 pub default_encoding:CefString,
 pub remote_fonts:i32,
 pub javascript:i32,
 pub javascript_close_windows:i32,
 pub javascript_access_clipboard:i32,
 pub javascript_dom_paste:i32,
 pub image_loading:i32,
 pub image_shrink_standalone_to_fit:i32,
 pub text_area_resize:i32,
 pub tab_to_links:i32,
 pub local_storage:i32,
 pub databases:i32,
 pub webgl:i32,
 pub background_color:u32,
 pub chrome_status_bubble:i32,
 pub chrome_zoom_bubble:i32,
}
pub fn zero<T>()->T {unsafe {std::mem::zeroed()}}
pub struct Wide {data:Vec<u16>}
impl Wide {pub fn new(s:&str)->Self{Self {data:s.encode_utf16().collect()}}pub fn value(&mut self)->CefString{CefString {str_:self.data.as_mut_ptr(),length:self.data.len(),dtor:None}}}
pub unsafe fn release<T>(p:*mut T){if !p.is_null(){let b=p.cast::<Base>();if let Some(release)=unsafe {(*b).release}{unsafe {release(b)};}}}
use windows::{Win32::{Foundation::{FreeLibrary,HMODULE},System::LibraryLoader::{LoadLibraryExW,GetProcAddress,LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,LOAD_LIBRARY_SEARCH_SYSTEM32}},core::{PCWSTR,PCSTR}};
pub struct Api {module:HMODULE,pub string_free:unsafe extern "C" fn(*mut CefString),pub execute:unsafe extern "C" fn(*const MainArgs,*mut c_void,*mut c_void)->i32,pub initialize:unsafe extern "C" fn(*const MainArgs,*const Settings,*mut c_void,*mut c_void)->i32,pub pump:unsafe extern "C" fn(),pub shutdown:unsafe extern "C" fn(),pub create:unsafe extern "C" fn(*const WindowInfo,*mut Client,*const CefString,*const BrowserSettings,*mut c_void,*mut c_void)->*mut Browser}
impl Api {
 pub fn load(root:&std::path::Path)->Result<Self,String>{
  let path=root.join("libcef.dll");let wide:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
  let module=unsafe {LoadLibraryExW(PCWSTR(wide.as_ptr()),None,LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR|LOAD_LIBRARY_SEARCH_SYSTEM32)}.map_err(|e|format!("CEF runtime load {}: {e}",path.display()))?;
  unsafe fn symbol<T:Copy>(module:HMODULE,name:&[u8])->Result<T,String>{if size_of::<T>()!=size_of::<usize>(){return Err("CEF function pointer size".into());}let p=unsafe {GetProcAddress(module,PCSTR(name.as_ptr()))}.ok_or_else(||format!("CEF symbol missing: {}",String::from_utf8_lossy(name)))?;Ok(unsafe {std::mem::transmute_copy(&p)})}
  let result=(||{let hash:unsafe extern "C" fn(i32)->*const i8=unsafe {symbol(module,b"cef_api_hash\0")}?;let version:unsafe extern "C" fn(i32)->i32=unsafe {symbol(module,b"cef_version_info\0")}?;
   for (entry,expected) in [(0,skylive_browser_protocol::CEF_HASH),(2,skylive_browser_protocol::CEF_COMMIT)] {let p=unsafe {hash(entry)};if p.is_null(){return Err("CEF ABI hash is null".into());}let actual=unsafe {std::ffi::CStr::from_ptr(p)}.to_str().map_err(|_|"CEF hash UTF8")?;if actual!=expected{return Err(format!("CEF ABI mismatch: expected {expected}, got {actual}"));}}
   if unsafe {version(0)}!=127||unsafe {version(1)}!=145||unsafe {version(2)}!=7{return Err("CEF127.145.7 runtime is required".into());}
   Ok(Self {module,string_free:unsafe {symbol(module,b"cef_string_userfree_utf16_free\0") }?,execute:unsafe {symbol(module,b"cef_execute_process\0") }?,initialize:unsafe {symbol(module,b"cef_initialize\0") }?,pump:unsafe {symbol(module,b"cef_do_message_loop_work\0") }?,shutdown:unsafe {symbol(module,b"cef_shutdown\0") }?,create:unsafe {symbol(module,b"cef_browser_host_create_browser_sync\0") }?})})();
  if result.is_err(){let _=unsafe {FreeLibrary(module)};}result
 }
}
impl Drop for Api {fn drop(&mut self){let _=unsafe {FreeLibrary(self.module)};}}
use std::os::windows::ffi::OsStrExt;
