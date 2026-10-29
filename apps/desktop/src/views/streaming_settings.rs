//! Staged selected-profile RTMP service/server and Custom editor. No IO, resolution, SDK or transport.
use eframe::egui;
use skylive_core::{
    Command, ProfileCommand, ProfileId, Session, StreamingSettings, StudioSnapshot,
};
pub struct StreamingDraft {
    presets:super::stream_service_presets::Selection,
    pub encoder:super::video_encoder::Draft,
    session: u64,
    revision: u64,
    pub profile: ProfileId,
    profile_name: String,
    server_url: String,
    stream_key: String,
    video_bitrate: String,
    keyframe_seconds: String,
    audio_enabled: bool,
    audio_track:skylive_core::AudioTrackId,
    audio_bitrate: u32,
    show_key: bool,
    reconnect:super::stream_reconnect::Draft,
}
pub struct StreamingApply {
    pub session: u64,
    pub revision: u64,
    pub profile: ProfileId,
    pub settings: StreamingSettings,
}
impl StreamingApply {
    pub fn command_for(self, session: &Session) -> Result<Command, String> {
        if session.is_closed()
            || self.session != session.identity()
            || self.revision != session.snapshot().revision()
            || self.profile != session.snapshot().profiles().selected_id()
        {
            return Err("The profile changed. Reopen Stream settings before applying.".into());
        }
        Ok(Command::Profile {
            command: ProfileCommand::SetStreaming {
                id: self.profile,
                settings: self.settings,
            },
        })
    }
}
impl StreamingDraft {
    /// Navigation can construct a view-only draft from an immutable model.
    /// The frontend MUST bind its current Session before displaying it.
    pub fn from_model(model: &StudioSnapshot) -> Self {
        let p = model.profiles().selected();
        let s = p.streaming();
        Self {
            presets:super::stream_service_presets::Selection::from_url(s.server_url()),
            encoder:super::video_encoder::Draft::new(s.encoder()),
            session: 0,
            revision: model.revision(),
            profile: p.id(),
            profile_name: p.name().into(),
            server_url: s.server_url().into(),
            stream_key: s.stream_key().into(),
            video_bitrate: s.video_bitrate_kbps().to_string(),
            keyframe_seconds: s.keyframe_interval_seconds().to_string(),
            audio_enabled: s.audio_enabled(),
            audio_track:s.audio_track(),
            audio_bitrate: s.audio_bitrate_kbps(),
            show_key: false,
            reconnect:super::stream_reconnect::Draft::new(s.reconnect()),
        }
    }
    #[cfg(test)]
    pub fn capture(session: &Session) -> Result<Self, String> {
        let mut value = Self::from_model(&session.snapshot());
        value.bind(session)?;
        Ok(value)
    }
    pub fn bind(&mut self, session: &Session) -> Result<(), String> {
        if session.is_closed()
            || self.revision != session.snapshot().revision()
            || self.profile != session.snapshot().profiles().selected_id()
        {
            return Err("The profile changed. Reopen Stream settings before applying.".into());
        }
        self.session = session.identity();
        Ok(())
    }
    pub fn settings(&self) -> Result<StreamingSettings, String> {
        let video = self
            .video_bitrate
            .parse::<u32>()
            .map_err(|_| "Enter a whole streaming video bitrate in kbit/s.".to_owned())?;
        let gop = self
            .keyframe_seconds
            .parse::<u32>()
            .map_err(|_| "Enter a whole keyframe interval in seconds.".to_owned())?;
        let reconnect=self.reconnect.settings()?;
        StreamingSettings::new(
            self.server_url.clone(),
            self.stream_key.clone(),
            video,
            gop,
            self.audio_enabled,
            self.audio_bitrate,
        )
        .and_then(|v|v.with_encoder(self.encoder.selection))
        .and_then(|v|v.with_reconnect(reconnect))
        .map(|v|v.with_audio_track(self.audio_track))
        .map_err(|e| e.to_string())
    }
    pub fn request(&self) -> Result<StreamingApply, String> {
        if self.session == 0 {
            return Err("Stream settings have no active document binding.".into());
        }
        Ok(StreamingApply {
            session: self.session,
            revision: self.revision,
            profile: self.profile,
            settings: self.settings()?,
        })
    }
    pub fn unchanged(&self, model: &StudioSnapshot) -> bool {
        let settings = model.profiles().selected().streaming();
        self.reconnect.unchanged(settings.reconnect())&&self.encoder.selection==settings.encoder()
            && self.profile == model.profiles().selected_id()
            && self.server_url == settings.server_url()
            && self.stream_key == settings.stream_key()
            && self.video_bitrate.parse::<u32>().ok() == Some(settings.video_bitrate_kbps())
            && self.keyframe_seconds.parse::<u32>().ok()
                == Some(settings.keyframe_interval_seconds())
            && self.audio_track==settings.audio_track()
            && self.audio_enabled == settings.audio_enabled()
            && self.audio_bitrate == settings.audio_bitrate_kbps()
    }
    fn reset(&mut self) {
        let s = StreamingSettings::default();
        self.reconnect=super::stream_reconnect::Draft::new(s.reconnect());
        self.encoder=super::video_encoder::Draft::new(s.encoder());
        self.presets=super::stream_service_presets::Selection::from_url(s.server_url());
        self.server_url = s.server_url().into();
        self.stream_key = s.stream_key().into();
        self.video_bitrate = s.video_bitrate_kbps().to_string();
        self.keyframe_seconds = s.keyframe_interval_seconds().to_string();
        self.audio_enabled = s.audio_enabled();
        self.audio_track=s.audio_track();
        self.audio_bitrate = s.audio_bitrate_kbps();
    }
}
pub fn editor(ui:&mut egui::Ui,draft:&mut StreamingDraft){editor_inner(ui,draft,true);}
pub fn editor_setup(ui:&mut egui::Ui,draft:&mut StreamingDraft){editor_inner(ui,draft,false);}
fn editor_inner(ui: &mut egui::Ui, draft: &mut StreamingDraft,probe:bool) {
    ui.label(crate::i18n::tf(
        ui,
        "Profile: {name}",
        &[("{name}", draft.profile_name.clone())],
    ));
    ui.strong(crate::i18n::tr(ui, "RTMP / RTMPS · H.264 · AAC"));
    ui.label(crate::i18n::tr(ui,"Configure an RTMP or RTMPS server. On Windows, RTMPS uses TLS 1.2 and validates the server certificate and hostname. Applying settings never starts a connection."));
    super::stream_service_presets::show(ui,&mut draft.presets,&mut draft.server_url);
    let label = ui.label(crate::i18n::tr(ui, "Server URL"));
    let server=ui.add(egui::TextEdit::singleline(&mut draft.server_url).char_limit(4096).desired_width(ui.available_width()).id_salt("stream-server-url")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Enter rtmp://host[:port]/application or rtmps://host[:port]/application. Default ports are 1935 and 443. IPv6 uses brackets; editing does not connect."));
    if server.changed(){draft.presets.custom();}
    let label = ui.label(crate::i18n::tr(ui, "Stream Key"));
    let response=ui.add(egui::TextEdit::singleline(&mut draft.stream_key).password(!draft.show_key).char_limit(4096).desired_width(ui.available_width()).id_salt("stream-key")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Enter the exact stream name or key. Spaces and query text are preserved; the server URL is separate."));
    #[cfg(test)]
    ui.ctx()
        .data_mut(|data| data.insert_temp(egui::Id::new("stream-key-rect"), response.rect));
    let _ = response;
    ui.checkbox(&mut draft.show_key, crate::i18n::tr(ui, "Show Stream Key"))
        .on_hover_text(crate::i18n::tr(
            ui,
            "Show or hide the key in this editor without changing its stored value.",
        ));
    egui::Grid::new("streaming-encoder-fields").num_columns(2).show(ui,|ui|{
        let label=ui.label(crate::i18n::tr(ui,"Video Bitrate (kbit/s)"));ui.add(egui::TextEdit::singleline(&mut draft.video_bitrate).char_limit(10).desired_width(120.0).id_salt("stream-video-bitrate")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Request an average H.264 bitrate from 64 to 100000 kbit/s. Constant bitrate is not guaranteed."));ui.end_row();
        let label=ui.label(crate::i18n::tr(ui,"Keyframe Interval (seconds)"));ui.add(egui::TextEdit::singleline(&mut draft.keyframe_seconds).char_limit(2).desired_width(120.0).id_salt("stream-keyframe-interval")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose 1 to 10 seconds. Native GOP frames use the OBS integer frame-rate calculation."));ui.end_row();
    });
    if probe {super::video_encoder::editor(ui,&mut draft.encoder);}
    ui.checkbox(&mut draft.audio_enabled,crate::i18n::tr(ui,"Include Program Audio")).on_hover_text(crate::i18n::tr(ui,"Publish final Program audio as AAC. Preview-only sources are excluded; the format is checked at Start."));
    ui.add_enabled_ui(draft.audio_enabled,|ui|{ui.label(crate::i18n::tr(ui,"Audio Bitrate (kbit/s)"));egui::ComboBox::from_id_salt("stream-audio-bitrate").selected_text(draft.audio_bitrate.to_string()).show_ui(ui,|ui|{for rate in [96,128,160,192]{ui.selectable_value(&mut draft.audio_bitrate,rate,rate.to_string()).on_hover_text(crate::i18n::tr(ui,"Choose a supported AAC bitrate; the choice is retained while audio is disabled."));}}).response.on_hover_text(crate::i18n::tr(ui,"Choose a supported AAC bitrate; the choice is retained while audio is disabled.")).on_disabled_hover_text(crate::i18n::tr(ui,"Choose a supported AAC bitrate; the choice is retained while audio is disabled."));});
    super::output_audio_tracks::selector(ui,"stream-audio-track","Streaming Audio Track",&mut draft.audio_track,draft.audio_enabled);
    super::stream_reconnect::show(ui,&mut draft.reconnect);
    ui.horizontal_wrapped(|ui|{
        if ui.button(crate::i18n::tr(ui,"Clear Server Configuration")).on_hover_text(crate::i18n::tr(ui,"Clear the server and key in this draft while keeping encoder choices. Apply is still required.")).clicked(){draft.server_url.clear();draft.stream_key.clear();draft.presets=super::stream_service_presets::Selection::from_url("");}
        if ui.button(crate::i18n::tr(ui,"Restore Stream Defaults")).on_hover_text(crate::i18n::tr(ui,"Restore an unconfigured server and default encoder choices in this draft. Apply is still required.")).clicked(){draft.reset();}
    });
    ui.label(crate::i18n::tr(ui,"Streaming choices belong to the selected profile and stay outside scene Undo. Apply saves the draft; Cancel leaves the profile unchanged."));
    if let Err(error) = draft.settings() {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
    }
}
#[cfg(test)]
#[path = "streaming_settings_tests.rs"]
mod tests;
