//! Local-media properties are staged until the owned media worker validates the
//! file. Metadata and playback state never become presentation-owned decoders.
use eframe::egui;
use skylive_core::{Command, Id, MediaSource};
pub const WAIT_MEDIA: &str = "Wait for the current media operation to finish before applying.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaEdit {
    Add { scene: Id },
    Rebind { source: Id },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPrepare {
    pub token: u64,
    pub session: u64,
    pub revision: u64,
    pub edit: MediaEdit,
    pub name: String,
    pub path: String,
    pub looping: bool,
    pub restart_on_activate: bool,
    pub close_when_inactive: bool,
    pub clear_on_end: bool,
    pub speed_percent: u16,
}
impl MediaPrepare {
    pub fn command(&self, media: MediaSource) -> Command {
        let media = media.with_playback(
            self.looping,
            self.restart_on_activate,
            self.close_when_inactive,
            self.clear_on_end,
        ).with_speed(self.speed_percent).expect("validated media speed");
        match self.edit {
            MediaEdit::Add { scene } => Command::AddMediaSource {
                scene,
                name: self.name.clone(),
                media,
            },
            MediaEdit::Rebind { source } => Command::RebindMediaSource { source, media },
        }
    }
}

pub struct MediaDraft {
    pub decoder_info:Option<skylive_host::mediafile::VideoDecoderInfo>,
    pub token: u64,
    pub session: u64,
    pub revision: u64,
    pub edit: MediaEdit,
    pub name: String,
    pub path: String,
    pub looping: bool,
    pub restart_on_activate: bool,
    pub close_when_inactive: bool,
    pub clear_on_end: bool,
    pub speed_percent: u16,
    pub loading: bool,
    pub worker_busy: bool,
    pub error: Option<String>,
}
impl MediaDraft {
    pub fn add(scene: Id, name: String) -> Self {
        Self {
            decoder_info:None,
            token: 0,
            session: 0,
            revision: 0,
            edit: MediaEdit::Add { scene },
            name,
            path: String::new(),
            looping: false,
            restart_on_activate: true,
            close_when_inactive: false,
            clear_on_end: true,
            speed_percent: 100,
            loading: false,
            worker_busy: false,
            error: None,
        }
    }
    pub fn rebind(source: Id, media: &MediaSource) -> Self {
        Self {
            decoder_info:None,
            token: 0,
            session: 0,
            revision: 0,
            edit: MediaEdit::Rebind { source },
            name: String::new(),
            path: media.path().into(),
            looping: media.looping(),
            restart_on_activate: media.restart_on_activate(),
            close_when_inactive: media.close_when_inactive(),
            clear_on_end: media.clear_on_end(),
            speed_percent: media.speed_percent(),
            loading: false,
            worker_busy: false,
            error: None,
        }
    }
    pub fn title(&self) -> &'static str {
        if matches!(self.edit, MediaEdit::Add { .. }) {
            "Add Media Source"
        } else {
            "Properties: Media Source"
        }
    }
    pub fn request(&self) -> Result<MediaPrepare, String> {
        if self.loading || self.worker_busy {
            return Err(WAIT_MEDIA.into());
        }
        if !(1..=200).contains(&self.speed_percent) {return Err("Playback speed must be 1 to 200 percent.".into());}
        if matches!(self.edit, MediaEdit::Add { .. })
            && (self.name.trim().is_empty()
                || self.name.len() > 256
                || self.name.chars().any(char::is_control))
        {
            return Err(
                "Source name requires 1..256 UTF-8 bytes without control characters.".into(),
            );
        }
        if self.path.trim().is_empty()
            || self.path.len() > 16_384
            || self.path.chars().any(char::is_control)
        {
            return Err("Choose a local media file path without control characters (up to 16384 UTF-8 bytes).".into());
        }
        Ok(MediaPrepare {
            token: self.token,
            session: self.session,
            revision: self.revision,
            edit: self.edit,
            name: self.name.clone(),
            path: self.path.clone(),
            looping: self.looping,
            restart_on_activate: self.restart_on_activate,
            close_when_inactive: self.close_when_inactive,
            clear_on_end: self.clear_on_end,
            speed_percent: self.speed_percent,
        })
    }
    pub fn matches(&self, request: &MediaPrepare) -> bool {
        self.token == request.token
            && self.session == request.session
            && self.revision == request.revision
            && self.edit == request.edit
            && self.name == request.name
            && self.path == request.path
            && self.looping == request.looping
            && self.restart_on_activate == request.restart_on_activate
            && self.close_when_inactive == request.close_when_inactive
            && self.clear_on_end == request.clear_on_end
            && self.speed_percent == request.speed_percent
    }
}

pub fn editor(ui: &mut egui::Ui, draft: &mut MediaDraft) {
    ui.add_enabled_ui(!draft.loading, |ui| {
        if matches!(draft.edit, MediaEdit::Add { .. }) {
            let label = ui.label(crate::i18n::tr(ui, "Name"));
            ui.add(
                egui::TextEdit::singleline(&mut draft.name)
                    .id_salt("media-source-name")
                    .char_limit(256)
                    .desired_width(ui.available_width()),
            )
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(
                ui,
                "Name this media source so you can find it in Sources.",
            ))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Name this media source so you can find it in Sources.",
            ));
        }
        let label = ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Media.FileOrHttp","File or HTTP(S) URL"));
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut draft.path)
                    .id_salt("media-source-path")
                    .char_limit(16_384)
                    .desired_width(ui.available_width()),
            )
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Media.FileOrHttpHelp","Choose a local MP4/MOV file or a finite HTTP(S) MP4 URL. Live streams are not supported."))
            .on_disabled_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Media.FileOrHttpHelp","Choose a local MP4/MOV file or a finite HTTP(S) MP4 URL. Live streams are not supported."));
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new("media-source-path-rect"), response.rect)
        });
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("media-source-path-enabled"),
                response.enabled(),
            )
        });
        let _ = response;
        let speed_label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Media.PlaybackSpeed","Playback speed (%)"));
        ui.add(egui::Slider::new(&mut draft.speed_percent,1..=200)).labelled_by(speed_label.id)
            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Media.PlaybackSpeedHelp","Speed changes video timing and audio pitch."));
        for (value, caption, help) in [
            (
                &mut draft.looping,
                "Loop",
                "Play the media again when it reaches the end.",
            ),
            (
                &mut draft.restart_on_activate,
                "Restart playback when source becomes active",
                "Start from the beginning when this source becomes active in Program.",
            ),
            (
                &mut draft.close_when_inactive,
                "Close file when inactive",
                "Release the media file when the source is no longer showing.",
            ),
            (
                &mut draft.clear_on_end,
                "Show nothing when playback ends",
                "Clear the last video frame after playback reaches the end.",
            ),
        ] {
            ui.checkbox(value, crate::i18n::tr(ui, caption))
                .on_hover_text(crate::i18n::tr(ui, help))
                .on_disabled_hover_text(crate::i18n::tr(ui, help));
        }
    });
    ui.label(crate::i18n::tr(ui, "Local MP4/MOV: progressive 8-bit H.264 video and AAC-LC mono/stereo audio. Unsupported files are rejected."));
    ui.label(crate::i18n::tr(ui,"Windows prefers the system software H.264 decoder for admitted Main/High and multi-slice files. Rust playback remains available for other supported formats. Hardware decoding is not enabled."));
    ui.label(crate::i18n::tr(ui,"Network media and arbitrary codec/container formats are not supported."));
    if let Some(info)=draft.decoder_info {
        ui.label(crate::i18n::tf(ui,"Active decoder: {decoder}",&[("{decoder}",crate::i18n::tr(ui,info.backend.label()))]));
        let profile=match info.profile {66=>"H.264 Baseline",77=>"H.264 Main",100=>"H.264 High",_=>"Other H.264 profile"};
        ui.label(crate::i18n::tf(ui,"Video profile: {profile}, level_idc: {level}",&[("{profile}",crate::i18n::tr(ui,profile)),("{level}",info.level.to_string())]));
    }
    if draft.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(crate::i18n::tr(ui, "Checking media file…"));
        });
    } else if draft.worker_busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(crate::i18n::tr(
                ui,
                "Waiting for the current media operation…",
            ));
        });
    }
    if let Some(error) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_draft_is_staged_and_prepared_metadata_keeps_requested_flags_for_one_edit() {
        let mut session = skylive_core::Session::new();
        let original = session.snapshot();
        let mut draft = MediaDraft::add(session.preview(), "Movie".into());
        assert!(
            !draft.looping
                && draft.restart_on_activate
                && !draft.close_when_inactive
                && draft.clear_on_end
        );
        assert!(draft.request().is_err());
        draft.path = "movie.mp4".into();
        draft.looping = true;
        draft.close_when_inactive = true;
        draft.token = 7;
        draft.session = session.identity();
        draft.revision = original.revision();
        let request = draft.request().unwrap();
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        session
            .apply(request.command(MediaSource::new("movie.mp4".into(), 64, 48, true).unwrap()))
            .unwrap();
        let model = session.snapshot();
        let source = model.sources()[0].id();
        let skylive_core::SourceKind::MediaSource { media } = model.source(source).unwrap().kind()
        else {
            panic!("media")
        };
        assert!(media.looping() && media.close_when_inactive());
        let mut replacement = MediaDraft::rebind(source, media);
        replacement.path = "audio.m4a".into();
        replacement.clear_on_end = false;
        session
            .apply(
                replacement
                    .request()
                    .unwrap()
                    .command(MediaSource::new("audio.m4a".into(), 0, 0, true).unwrap()),
            )
            .unwrap();
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            model.source(source).unwrap().kind()
        );
        replacement.path = "bad\0file".into();
        assert!(replacement.request().is_err());
    }
    #[test]
    fn prepared_reply_requires_same_modal_path_flags_and_revision() {
        let session = skylive_core::Session::new();
        let mut draft = MediaDraft::add(session.preview(), "Movie".into());
        draft.path = "movie.mp4".into();
        let request = draft.request().unwrap();
        assert!(draft.matches(&request));
        draft.token += 1;
        assert!(!draft.matches(&request));
        draft.token -= 1;
        draft.path = "different.mp4".into();
        assert!(!draft.matches(&request));
        draft.path = request.path.clone();
        draft.looping = !draft.looping;
        assert!(!draft.matches(&request));
        draft.looping = request.looping;
        draft.revision += 1;
        assert!(!draft.matches(&request));
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
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| super::super::dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
        );
        (output, intents)
    }
    fn caption(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| caption(shape, label))
            }
            _ => None,
        }
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
                caption(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .expect("media dialog control remains visible");
        let pointer = |pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(context, session, slot, pointer(true));
        frame(context, session, slot, pointer(false)).1
    }
    #[test]
    fn busy_apply_wait_tooltip_appears_on_fresh_context_without_prior_click() {
        use super::super::dialogs::Dialog;
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let session = skylive_core::Session::new();
        let mut draft = MediaDraft::add(session.preview(), "Movie".into());
        draft.path = "movie.mp4".into();
        draft.worker_busy = true;
        let mut slot = Some(Dialog::MediaSource(draft));
        frame(&context, &session, &mut slot, vec![]);
        let (output, _) = frame(&context, &session, &mut slot, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| caption(&shape.shape, "Apply"))
            .unwrap();
        let now = context.input(|input| input.time);
        let mut painted = egui::FullOutput::default();
        for (offset, events) in [
            (0.01, vec![egui::Event::PointerMoved(pos)]),
            (0.8, vec![]),
            (1.0, vec![]),
        ] {
            painted = context.run(
                egui::RawInput {
                    time: Some(now + offset),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    super::super::dialogs::show(
                        ctx,
                        &mut slot,
                        &mut vec![],
                        true,
                        &session.snapshot(),
                    )
                },
            );
        }
        assert!(
            painted
                .shapes
                .iter()
                .any(|shape| caption(&shape.shape, WAIT_MEDIA)
                    .is_some_and(|point| shape.clip_rect.contains(point))),
            "fresh-context disabled tooltip missing: {:?}",
            context.data_mut(
                |data| data.get_temp::<String>(egui::Id::new(("modal-action-hover", 0usize)))
            )
        );
    }
    #[test]
    fn busy_worker_disables_actual_apply_with_media_tooltip_but_keeps_fields_and_cancel_available()
    {
        use super::super::{Intent, dialogs::Dialog};
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let session = skylive_core::Session::new();
        let before = session.snapshot();
        let mut draft = MediaDraft::add(session.preview(), "Movie".into());
        draft.token = 19;
        draft.session = session.identity();
        draft.revision = before.revision();
        draft.path = "movie.mp4".into();
        let expected = draft.request().unwrap();
        draft.worker_busy = true;
        assert_eq!(draft.request().unwrap_err(), WAIT_MEDIA);
        let mut slot = Some(Dialog::MediaSource(draft));
        assert!(
            click(&context, &session, &mut slot, "Apply").is_empty(),
            "disabled Apply must not even emit a validation error or prepare request"
        );
        assert!(
            matches!(&slot,Some(Dialog::MediaSource(draft))if draft.worker_busy&&!draft.loading&&draft.error.is_none())
        );
        assert_eq!(
            context
                .data_mut(|data| data.get_temp::<bool>(egui::Id::new("media-source-path-enabled"))),
            Some(true),
            "ordinary scope work must not freeze path editing"
        );
        let (output, _) = frame(&context, &session, &mut slot, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                caption(&shape.shape, "Apply").filter(|pos| shape.clip_rect.contains(*pos))
            })
            .unwrap();
        // A disabled click above proves admission is blocked. Egui deliberately
        // suppresses tooltips when resting at the last click (response.rs:
        // clicked_more_recently_than_moved), so perform a new hover afterwards.
        let now = context.input(|input| input.time);
        let mut output = egui::FullOutput::default();
        for (offset, events) in [
            (
                0.20,
                vec![egui::Event::PointerMoved(pos - egui::vec2(0.0, 40.0))],
            ),
            (
                0.22,
                vec![egui::Event::PointerMoved(pos - egui::vec2(0.0, 20.0))],
            ),
            (0.24, vec![egui::Event::PointerMoved(pos)]),
            (0.8, vec![]),
            (1.0, vec![]),
        ] {
            output = context.run(
                egui::RawInput {
                    time: Some(now + offset),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    super::super::dialogs::show(
                        ctx,
                        &mut slot,
                        &mut vec![],
                        true,
                        &session.snapshot(),
                    )
                },
            );
        }
        let tooltip = output.shapes.iter().any(|shape| {
            caption(&shape.shape, WAIT_MEDIA).is_some_and(|point| shape.clip_rect.contains(point))
        });
        fn labels(shape: &egui::epaint::Shape) -> Vec<String> {
            match shape {
                egui::epaint::Shape::Text(text) => vec![text.galley.text().to_owned()],
                egui::epaint::Shape::Vec(shapes) => shapes.iter().flat_map(labels).collect(),
                _ => vec![],
            }
        }
        assert!(
            tooltip,
            "disabled Apply must show the media wait reason, not a device-enumeration or path-format error; footer: {:?}; painted labels: {:?}",
            context.data_mut(
                |data| data.get_temp::<String>(egui::Id::new(("modal-action-hover", 0usize)))
            ),
            output
                .shapes
                .iter()
                .flat_map(|shape| labels(&shape.shape))
                .collect::<Vec<_>>()
        );
        if let Some(Dialog::MediaSource(draft)) = &mut slot {
            draft.worker_busy = false;
        }
        let intents = click(&context, &session, &mut slot, "Apply");
        let [Intent::PrepareMedia(actual)] = intents.as_slice() else {
            panic!("ready Apply submits exactly one prepared-file request")
        };
        assert_eq!(actual, &expected);
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        assert!(click(&context, &session, &mut slot, "Cancel").is_empty());
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let mut busy = MediaDraft::add(session.preview(), "Movie".into());
        busy.path = "movie.mp4".into();
        busy.worker_busy = true;
        slot = Some(Dialog::MediaSource(busy));
        assert!(click(&context, &session, &mut slot, "Cancel").is_empty());
        assert!(
            slot.is_none(),
            "Cancel stays clickable while the media owner is busy"
        );
    }
    #[test]
    fn actual_media_dialog_checkbox_apply_and_cancel_stage_without_file_io_or_model_publication() {
        use super::super::{Intent, dialogs::Dialog};
        let context = egui::Context::default();
        let session = skylive_core::Session::new();
        let original = session.snapshot();
        let mut draft = MediaDraft::add(session.preview(), "Movie".into());
        draft.token = 9;
        draft.session = session.identity();
        draft.revision = original.revision();
        draft.path = "movie.mp4".into();
        let mut slot = Some(Dialog::MediaSource(draft));
        assert!(click(&context, &session, &mut slot, "Loop").is_empty());
        let intents = click(&context, &session, &mut slot, "Apply");
        let [Intent::PrepareMedia(request)] = intents.as_slice() else {
            panic!("Apply requests worker validation, not a direct canonical command")
        };
        assert!(
            request.looping
                && request.restart_on_activate
                && !request.close_when_inactive
                && request.clear_on_end
        );
        assert_eq!(request.token, 9);
        assert!(slot.is_some());
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        assert!(click(&context, &session, &mut slot, "Cancel").is_empty());
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
        let draft = MediaDraft::add(session.preview(), "Movie".into());
        slot = Some(Dialog::MediaSource(draft));
        assert!(matches!(
            click(&context, &session, &mut slot, "Apply").as_slice(),
            [Intent::DialogError(_)]
        ));
        assert!(slot.is_some());
        assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
    }
}
