//! Profile recording choices are staged here; no writer, path I/O or transport.
use eframe::egui;
use skylive_core::{
    Command, Profile, ProfileCommand, ProfileId, RecordingSettings, StudioSnapshot,
};

pub struct RecordingDraft {
    pub container:skylive_core::RecordingContainer,
    pub audio_tracks:skylive_core::RecordingTracks,
    track_names:[String;6],
    pub replay_audio_track:skylive_core::AudioTrackId,
    pub encoder:super::video_encoder::Draft,
    pub keyframe_seconds:u32,
    pub profile: ProfileId,
    pub profile_name: String,
    pub directory: String,
    pub video_bitrate: String,
    pub audio_bitrate: u32,
    pub audio_enabled: bool,
    pub prefer_hardware: bool,
    pub replay_duration:u32,
    pub replay_memory:u32,
}
impl RecordingDraft {
    pub fn from_profile(profile: &Profile) -> Self {
        let settings = profile.recording();
        Self {
            container:settings.container(),
            audio_tracks:settings.audio_tracks().clone(),
            track_names:super::output_audio_tracks::names(settings.audio_tracks()),
            replay_audio_track:profile.replay_buffer().audio_track(),
            encoder:super::video_encoder::Draft::new(settings.encoder()),
            keyframe_seconds:settings.keyframe_interval_seconds(),
            profile: profile.id(),
            profile_name: profile.name().into(),
            directory: settings.directory().into(),
            video_bitrate: settings.video_bitrate_kbps().to_string(),
            audio_bitrate: settings.audio_bitrate_kbps(),
            audio_enabled: settings.audio_enabled(),
            prefer_hardware: settings.prefer_hardware(),
            replay_duration:profile.replay_buffer().duration_seconds(),replay_memory:profile.replay_buffer().memory_mib(),
        }
    }
    pub fn settings(&self) -> Result<RecordingSettings, String> {
        let video = self
            .video_bitrate
            .parse::<u32>()
            .map_err(|_| "Enter a whole video bitrate in kbit/s.".to_owned())?;
        let tracks=super::output_audio_tracks::settings(&self.audio_tracks,&self.track_names)?;
        RecordingSettings::new(
            self.directory.clone(),
            video,
            self.audio_bitrate,
            self.audio_enabled,
            self.prefer_hardware,
        )
        .and_then(|v|v.with_audio_tracks(tracks))
        .and_then(|v|v.with_container(self.container))
        .and_then(|v|v.with_encoder(self.encoder.selection))
        .and_then(|v|v.with_keyframe_interval(if self.encoder.selection.is_default(){2}else{self.keyframe_seconds}))
        .map_err(|error| error.to_string())
    }
    pub fn command(&self) -> Result<Command, String> {
        Ok(Command::Profile {
            command: ProfileCommand::SetRecordingAndReplay {
                id: self.profile,
                recording: self.settings()?,
                replay:skylive_core::ReplayBufferSettings::new(self.replay_duration,self.replay_memory).map_err(|e|e.to_string())?.with_audio_track(self.replay_audio_track),
            },
        })
    }
    pub fn unchanged(&self, model: &StudioSnapshot) -> bool {
        model
            .profiles()
            .entries()
            .iter()
            .find(|profile| profile.id() == self.profile)
            .is_some_and(|profile| {
                let settings = profile.recording();
                super::output_audio_tracks::settings(&self.audio_tracks,&self.track_names).is_ok_and(|v|v==*settings.audio_tracks())&&self.replay_audio_track==profile.replay_buffer().audio_track()&&self.container==settings.container()&&self.encoder.selection==settings.encoder()&&(if self.encoder.selection.is_default(){2}else{self.keyframe_seconds})==settings.keyframe_interval_seconds()&&self.replay_duration==profile.replay_buffer().duration_seconds()&&self.replay_memory==profile.replay_buffer().memory_mib()
                    && self.directory == settings.directory()
                    && self.video_bitrate.parse::<u32>().ok() == Some(settings.video_bitrate_kbps())
                    && self.audio_bitrate == settings.audio_bitrate_kbps()
                    && self.audio_enabled == settings.audio_enabled()
                    && self.prefer_hardware == settings.prefer_hardware()
            })
    }
}
pub fn editor(ui:&mut egui::Ui,draft:&mut RecordingDraft,intents:&mut Vec<super::Intent>){editor_with_replay(ui,draft,true,Some(intents));}
pub fn editor_recording_only(ui:&mut egui::Ui,draft:&mut RecordingDraft){editor_with_replay(ui,draft,false,None);}
fn editor_with_replay(ui:&mut egui::Ui,draft:&mut RecordingDraft,show_replay:bool,mut intents:Option<&mut Vec<super::Intent>>){
    if show_replay {egui::CollapsingHeader::new(crate::i18n::tr(ui,"Replay Buffer")).default_open(true).show(ui,|ui|{
        egui::Grid::new("replay-profile-settings").num_columns(2).show(ui,|ui|{
            let label=ui.label(crate::i18n::tr(ui,"Replay duration (seconds)"));ui.add(egui::DragValue::new(&mut draft.replay_duration).range(1..=600)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Keep a recent encoded clip. Whole keyframes can add one GOP; memory pressure can shorten the clip."));ui.end_row();
            let label=ui.label(crate::i18n::tr(ui,"Encoded buffer memory (MiB)"));ui.add(egui::DragValue::new(&mut draft.replay_memory).range(16..=2048)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Bound encoded packets including clips pinned by a save. Renderer and codec working memory are separate."));ui.end_row();
        });
        super::output_audio_tracks::selector(ui,"replay-audio-track","Replay Audio Track",&mut draft.replay_audio_track,draft.audio_enabled);
        ui.small(crate::i18n::tr(ui,"Replay Buffer saves one selected audio mix using the global recording audio bitrate, recording video encoder and folder. Changes apply on the next start."));
    });}
    ui.label(crate::i18n::tf(
        ui,
        "Profile: {name}",
        &[("{name}", draft.profile_name.clone())],
    ));
    ui.strong(crate::i18n::tr(ui,"H.264 video · AAC audio"));
    let label=ui.label(crate::i18n::tr(ui,"Recording Format"));
    egui::ComboBox::from_id_salt("recording-container").selected_text(crate::i18n::tr(ui,match draft.container{skylive_core::RecordingContainer::Mp4=>"MPEG-4 (.mp4)",skylive_core::RecordingContainer::Matroska=>"Matroska (.mkv)"})).show_ui(ui,|ui|{
        for (container,text) in [(skylive_core::RecordingContainer::Mp4,"MPEG-4 (.mp4)"),(skylive_core::RecordingContainer::Matroska,"Matroska (.mkv)")]{
            ui.add_enabled_ui(skylive_host::recording::container_available(container),|ui|{ui.selectable_value(&mut draft.container,container,crate::i18n::tr(ui,text)).on_hover_text(crate::i18n::tr(ui,"Choose the actual recording container. New files use its matching extension; Replay Buffer saves MP4."));}).response.on_disabled_hover_text(crate::i18n::tr(ui,"Matroska recording is available on Windows. The saved selection is retained on other platforms."));
        }
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the actual recording container. New files use its matching extension; Replay Buffer saves MP4."));
    ui.label(crate::i18n::tr(ui,"Matroska writes complete video and audio blocks during recording. Interrupted private files may contain an incomplete tail; automatic repair and playback are not guaranteed."));
    if draft.container==skylive_core::RecordingContainer::Matroska{ui.small(crate::i18n::tr(ui,"Matroska supports the selected recording audio tracks. MKV Media Source input and arbitrary multitrack remux are outside this recording feature."));}
    if !skylive_host::recording::available() {
        ui.label(crate::i18n::tr(
            ui,
            "Recording is unavailable on this platform",
        ));
    }
    let label = ui.label(crate::i18n::tr(ui, "Recording Path"));
    ui.horizontal(|ui| {
        let browse_width=if intents.is_some() && cfg!(target_os="windows") {85.0+ui.spacing().item_spacing.x} else {0.0};
        ui.add(egui::TextEdit::singleline(&mut draft.directory)
            .hint_text(skylive_host::recording::default_directory().to_string_lossy())
            .char_limit(4096).desired_width((ui.available_width()-browse_width).max(50.0))
            .id_salt("recording-directory"))
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(ui,"Set the folder for new recordings. Its availability is checked when recording starts."));
        if let Some(intents)=intents.as_deref_mut(){
            if let Some(intent)=crate::native_browse::button(ui,"Browse",crate::native_browse::Target::RecordingFolder,true){intents.push(intent);}
        }
    });
    egui::Grid::new("recording-profile-rates").num_columns(2).show(ui,|ui|{
        let label=ui.label(crate::i18n::tr(ui,"Video Bitrate (kbit/s)"));
        let response=ui.add(egui::TextEdit::singleline(&mut draft.video_bitrate).char_limit(10).desired_width(100.0).id_salt("recording-video-bitrate"))
            .labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Request an average H.264 bitrate from 100 to 200000 kbit/s. Constant bitrate is not guaranteed."));
        #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new("recording-video-bitrate-rect"),response.rect));
        let _=response;ui.end_row();
        let label=ui.label(crate::i18n::tr(ui,"Default recording audio bitrate (kbit/s)"));
        ui.add_enabled_ui(draft.audio_enabled,|ui|{
            egui::ComboBox::from_id_salt("recording-audio-bitrate").selected_text(draft.audio_bitrate.to_string()).show_ui(ui,|ui|{
                for bitrate in [96,128,160,192]{ui.selectable_value(&mut draft.audio_bitrate,bitrate,bitrate.to_string()).on_hover_text(crate::i18n::tr(ui,"Choose a supported AAC audio bitrate."));}
            }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose a supported AAC audio bitrate.")).on_disabled_hover_text(crate::i18n::tr(ui,"Enable Record Audio to change its bitrate. The saved bitrate is retained while audio is off."));
        });ui.end_row();
    });
    ui.checkbox(
        &mut draft.audio_enabled,
        crate::i18n::tr(ui, "Record Audio"),
    )
    .on_hover_text(crate::i18n::tr(
        ui,
        "Include the selected audio mixes as independent mono or stereo AAC tracks.",
    ));
    super::output_audio_tracks::recording(ui,&mut draft.audio_tracks,&mut draft.track_names,draft.audio_bitrate,draft.audio_enabled);
    if show_replay {super::video_encoder::editor(ui,&mut draft.encoder);}
    ui.add_enabled_ui(show_replay&&!draft.encoder.selection.is_default(),|ui|{let label=ui.label(crate::i18n::tr(ui,"NVENC Keyframe Interval (seconds)"));ui.add(egui::DragValue::new(&mut draft.keyframe_seconds).range(1..=10)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Configure the native NVENC GOP for recording and Replay Buffer. Applies on the next start.")).on_disabled_hover_text(crate::i18n::tr(ui,"This GOP control is available for the explicit NVENC encoder."));});
    ui.label(crate::i18n::tr(ui,"These settings apply to new recordings. Apply saves the selected profile without starting recording or creating folders. Cancel keeps its current settings."));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_recording_draft_validates_without_scene_history_or_source_mutation() {
        let mut session = skylive_core::Session::new();
        let original = session.snapshot();
        let mut draft = RecordingDraft::from_profile(original.profiles().selected());
        assert!(draft.unchanged(&original));
        assert_eq!(
            draft.settings().unwrap(),
            *original.profiles().selected().recording()
        );
        draft.directory = "C:/Videos/Живий ефір".into();
        draft.video_bitrate = "12000".into();
        draft.audio_enabled = false;
        draft.audio_bitrate = 192;
        draft.prefer_hardware = false;
        let expected = draft.settings().unwrap();
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        assert!(!draft.unchanged(&original));
        session.apply(draft.command().unwrap()).unwrap();
        assert_eq!(
            session.snapshot().profiles().selected().recording(),
            &expected
        );
        assert!(
            !session.can_undo(),
            "profile settings do not create scene Undo entries"
        );
        assert_eq!(session.snapshot().sources(), original.sources());
        assert!(draft.unchanged(&session.snapshot()));
        let before = session.snapshot();
        session.apply(draft.command().unwrap()).unwrap();
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        for invalid in ["", "not a bitrate", "99", "200001", "4294967296"] {
            draft.video_bitrate = invalid.into();
            assert!(draft.command().is_err());
            assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
    fn frame(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<super::super::dialogs::Dialog>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<super::super::Intent>) {
        let mut intents = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| super::super::dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
        );
        (output, intents)
    }
    fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| point(shape, label)),
            _ => None,
        }
    }
    fn click_at(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<super::super::dialogs::Dialog>,
        pos: egui::Pos2,
    ) -> Vec<super::super::Intent> {
        frame(
            context,
            session,
            slot,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            context,
            session,
            slot,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .1
    }
    fn click(
        context: &egui::Context,
        session: &skylive_core::Session,
        slot: &mut Option<super::super::dialogs::Dialog>,
        label: &str,
    ) -> Vec<super::super::Intent> {
        frame(context, session, slot, vec![]);
        let (output, _) = frame(context, session, slot, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                point(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .expect("recording settings control is visible");
        click_at(context, session, slot, pos)
    }
    #[test]
    fn actual_output_page_widgets_stage_cancel_reject_invalid_text_and_commit_selected_profile() {
        use super::super::{Intent, dialogs::Dialog};
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Voice".into(),
                binding: skylive_core::AudioCaptureBinding::new(
                    skylive_core::AudioFlow::Input,
                    skylive_core::AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap();
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let baseline = session.snapshot();
        let mut slot = Some(Dialog::general(&baseline));
        let next = click(&context, &session, &mut slot, "Output")
            .into_iter()
            .find_map(|intent| {
                if let Intent::OpenDialog(dialog) = intent {
                    Some(dialog)
                } else {
                    None
                }
            })
            .expect("the real Output page navigation opens the recording draft");
        slot = Some(next);
        assert!(matches!(&slot, Some(Dialog::Recording(_))));
        click(&context, &session, &mut slot, "Record Audio");
        assert!(matches!(&slot,Some(Dialog::Recording(draft)) if !draft.audio_enabled));
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        click(&context, &session, &mut slot, "Cancel");
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        slot = Some(Dialog::recording(&baseline));
        frame(&context, &session, &mut slot, vec![]);
        frame(&context, &session, &mut slot, vec![]);
        let pos = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("recording-video-bitrate-rect"))
            })
            .unwrap()
            .center();
        click_at(&context, &session, &mut slot, pos);
        frame(
            &context,
            &session,
            &mut slot,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Text("oops".into()),
            ],
        );
        assert!(matches!(&slot,Some(Dialog::Recording(draft)) if draft.video_bitrate=="oops"));
        let invalid = click(&context, &session, &mut slot, "Apply");
        assert!(
            invalid
                .iter()
                .any(|intent| matches!(intent, Intent::DialogError(_)))
        );
        assert!(matches!(&slot,Some(Dialog::Recording(draft)) if draft.video_bitrate=="oops"));
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        if let Some(Dialog::Recording(draft)) = &mut slot {
            draft.video_bitrate = "15000".into();
            draft.directory = "C:/Videos/Зйомка".into();
            draft.audio_bitrate = 192;
        }
        let command = click(&context, &session, &mut slot, "Apply")
            .into_iter()
            .find_map(|intent| match intent {
                Intent::Command(command) => Some(command),
                Intent::SubmitSettings { .. } => {
                    super::super::settings::prepare(slot.as_ref().unwrap(), &session).ok()
                }
                _ => None,
            })
            .expect("Apply uses the shared profile command");
        session.apply(command).unwrap();
        assert_eq!(
            session
                .snapshot()
                .profiles()
                .selected()
                .recording()
                .video_bitrate_kbps(),
            15000
        );
        assert_eq!(session.snapshot().sources(), baseline.sources());
        assert_eq!(session.snapshot().global_audio(), baseline.global_audio());
        session.apply(Command::Undo).unwrap();
        assert!(
            session.snapshot().sources().is_empty(),
            "recording settings do not insert a scene Undo step"
        );
        assert_eq!(
            session
                .snapshot()
                .profiles()
                .selected()
                .recording()
                .video_bitrate_kbps(),
            15000
        );
    }
}
