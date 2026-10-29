//! Typed per-source transport admission. Copy to host/src/media_source/control.rs.
use super::*;
use crate::audio_program::ProgramAudioError;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaControl {
    Pause,
    Resume,
    Restart,
    /// Retire the file reader after the audio cut; reopen only when eligible.
    Reload,
    Stop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MediaTransportStatus {
    pub state: MediaPlaybackState,
    pub stream_generation: u64,
    pub ready: bool,
    pub last_control: Option<MediaAppliedTime>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MediaAppliedTime {
    pub requested_ns: u64,
    pub effective_ns: u64,
    pub audio_frame: Option<u64>,
    pub audio_sample_rate: Option<u32>,
    pub late_frames: u64,
}
#[derive(Clone, Debug)]
pub struct MediaControlTarget {
    pub(super) instance: u64,
    pub(super) session: u64,
    pub(super) epoch: u64,
    pub(super) revision: u64,
    pub(super) source: Id,
    pub(super) media: Arc<MediaSource>,
    pub(super) status: MediaTransportStatus,
}
impl MediaControlTarget {
    pub fn matches(&self, session: &Session) -> bool {
        !session.is_closed()
            && self.session == session.identity()
            && self.revision == session.snapshot().revision()
            && matches!(session.snapshot().source(self.source).map(|s|s.kind()),Some(skylive_core::SourceKind::MediaSource{media})if media==self.media.as_ref())
    }
    pub fn source(&self) -> Id {
        self.source
    }
    pub fn status(&self) -> MediaTransportStatus {
        self.status
    }
}
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub showing:bool,
    pub media: Arc<MediaSource>,
    pub status: MediaTransportStatus,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaBarrierReceipt {
    pub effective_frame: u64,
    pub effective_ns: u64,
    pub sample_rate: u32,
    pub late_frames: u64,
}
#[derive(Debug)]
pub(crate) struct MediaBarrier {
    pub source: Id,
    pub media: Arc<MediaSource>,
    pub generation: u64,
    pub next_generation: u64,
    pub last_sequence: u64,
    pub effective_ns: u64,
    pub suspend: bool,
    pub reply: mpsc::SyncSender<Result<MediaBarrierReceipt, ProgramAudioError>>,
}
impl MediaSnapshot {
    /// Stock Media Source hotkey semantics, sharing actual target receipts with
    /// toolbar/API controls while retaining the source-showing hotkey gate.
    pub fn source_hotkey_control_for(&self,session:u64,revision:u64,source:Id,media:&MediaSource,operation:skylive_core::SourcePlaybackAction)->Result<Option<(MediaControlTarget,MediaControl)>,MediaSourceError>{
        use skylive_core::SourcePlaybackAction as Hotkey;
        if matches!(operation,Hotkey::Previous|Hotkey::Next){return Err(MediaSourceError::Invalid("Playback shortcuts require a matching Media Source or Slideshow control."));}
        let target=self.control_target_for(session,revision,source,media)?;
        if !self.source_is_showing(session,source,media){return Ok(None);}
        let state=target.status().state;
        let control=match operation{
            Hotkey::PlayPause=>Some(match state{MediaPlaybackState::Playing=>MediaControl::Pause,MediaPlaybackState::Paused=>MediaControl::Resume,_=>MediaControl::Restart}),
            Hotkey::Play=>match state{MediaPlaybackState::Playing=>None,MediaPlaybackState::Paused=>Some(MediaControl::Resume),_=>Some(MediaControl::Restart)},
            Hotkey::Pause=>(state==MediaPlaybackState::Playing).then_some(MediaControl::Pause),
            Hotkey::Restart=>Some(MediaControl::Restart),Hotkey::Stop=>(state!=MediaPlaybackState::Stopped).then_some(MediaControl::Stop),
            Hotkey::Previous|Hotkey::Next=>None,
        };Ok(control.map(|control|(target,control)))
    }
    /// Native source-hotkey showing membership, distinct from sidechain-only
    /// audio acquisition. The immutable binding must still match this snapshot.
    pub fn source_is_showing(&self,session:u64,source:Id,media:&MediaSource)->bool{
        self.session==session&&self.controls.get(&source).is_some_and(|entry|entry.showing&&entry.media.as_ref()==media)
    }

    pub fn control_target_for(
        &self,
        session: u64,
        revision: u64,
        source: Id,
        media: &MediaSource,
    ) -> Result<MediaControlTarget, MediaSourceError> {
        if self.session != session {
            return Err(MediaSourceError::Invalid(
                "media control belongs to a different scope",
            ));
        }
        let entry = self
            .controls
            .get(&source)
            .filter(|e| e.media.as_ref() == media)
            .ok_or(MediaSourceError::Invalid(
                "media source is not admitted with the current binding",
            ))?;
        if !entry.status.ready {
            return Err(MediaSourceError::Busy);
        }
        Ok(MediaControlTarget {
            instance: self.instance,
            session,
            epoch: self.epoch,
            revision,
            source,
            media: entry.media.clone(),
            status: entry.status,
        })
    }
    pub fn transport_status(
        &self,
        session: u64,
        source: Id,
        media: &MediaSource,
    ) -> Option<MediaTransportStatus> {
        if self.session != session {
            return None;
        }
        let entry = self.controls.get(&source)?;
        (entry.media.as_ref() == media).then_some(entry.status)
    }
}
impl MediaOwner {
    pub fn target_is_current(&self, target: &MediaControlTarget) -> bool {
        !self.closed
            && target.instance == self.instance
            && target.session == self.session
            && target.epoch == self.epoch
    }
    pub fn control_target(
        &self,
        session: &Session,
        source: Id,
    ) -> Result<MediaControlTarget, MediaSourceError> {
        if self.closed || session.is_closed() {
            return Err(MediaSourceError::Closed);
        }
        if self.session != session.identity()
            || self.snapshot.session != session.identity()
            || self.snapshot.epoch != self.epoch
        {
            return Err(MediaSourceError::Invalid(
                "media control belongs to a different scope",
            ));
        }
        let model = session.snapshot();
        let Some(skylive_core::SourceKind::MediaSource { media }) =
            model.source(source).map(|s| s.kind())
        else {
            return Err(MediaSourceError::Invalid(
                "selected source is not a local Media Source",
            ));
        };
        let entry = self
            .snapshot
            .controls
            .get(&source)
            .filter(|entry| entry.media.as_ref() == media)
            .ok_or(MediaSourceError::Invalid(
                "media source is not admitted with the current binding",
            ))?;
        Ok(MediaControlTarget {
            instance: self.instance,
            session: session.identity(),
            epoch: self.epoch,
            revision: model.revision(),
            source,
            media: entry.media.clone(),
            status: entry.status,
        })
    }
    pub fn control(
        &mut self,
        session: &Session,
        target: MediaControlTarget,
        operation: MediaControl,
        now: MediaTime,
    ) -> Result<MediaRequestKey, MediaSourceError> {
        if operation==MediaControl::Reload{return Err(MediaSourceError::Invalid("Reload requires an exact MediaReloadTarget proof"));}
        self.submit_control(session,target,super::ControlRequest::Direct(operation),now)
    }
    /// Native hotkeys are decided by the media actor, never a displayed state.
    /// Callback acceptance is independent of the later Controlled codec result.
    pub fn source_hotkey_control(&mut self,session:&Session,target:MediaControlTarget,operation:skylive_core::SourcePlaybackAction,callback:crate::global_hotkeys::Callback,now:MediaTime)->Result<MediaRequestKey,MediaSourceError>{
        if matches!(operation,skylive_core::SourcePlaybackAction::Previous|skylive_core::SourcePlaybackAction::Next){callback.reject();return Err(MediaSourceError::Invalid("Playback shortcuts require a matching Media Source or Slideshow control."));}
        let result=self.submit_control(session,target,super::ControlRequest::SourceHotkey{operation,callback:callback.clone()},now);
        if result.is_err(){callback.reject();}result
    }
    fn submit_control(&mut self,session:&Session,target:MediaControlTarget,request:super::ControlRequest,now:MediaTime)->Result<MediaRequestKey,MediaSourceError>{
        now.0
            .checked_add(1_000_000_000)
            .ok_or(MediaSourceError::TimeOverflow)?;
        let key = self.key(session)?;
        let model = session.snapshot();
        if target.instance != self.instance
            || target.session != session.identity()
            || target.epoch != self.epoch
            || target.revision != model.revision()
            || !matches!(model.source(target.source).map(|s|s.kind()),Some(skylive_core::SourceKind::MediaSource{media})if media==target.media.as_ref())
            || self
                .snapshot
                .controls
                .get(&target.source)
                .is_none_or(|entry| {
                    entry.media != target.media
                        || entry.status.stream_generation != target.status.stream_generation
                })
        {
            return Err(MediaSourceError::Invalid("stale media control target"));
        }
        // Play/PlayPause may resolve to Restart only inside the media owner.
        // Give each hotkey a cancellable admission gate before that decision;
        // its callback receipt remains independent of the later codec result.
        let gate=matches!(&request,super::ControlRequest::Direct(MediaControl::Restart)|super::ControlRequest::SourceHotkey{..}).then(||Arc::new(super::ControlGate::new()));
        self.send(
            key,
            Job::Control {
                key,
                target,
                request,
                now,
                sent: Instant::now(),
                gate: gate.clone(),
            },
        )?;
        self.pending_gate=gate.map(|gate|(key,gate));
        Ok(key)
    }
}
