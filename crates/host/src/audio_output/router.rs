use super::*;
pub enum AudioOutput {
    Recording(crate::recording::RecordingAudioSink),
    Streaming(StreamingAudioSink),
    ReplayBuffer(StreamingAudioSink),
    NdiProgram(StreamingAudioSink),
}
impl AudioOutput {
    pub fn key(&self) -> OutputKey {
        match self {
            Self::Recording(sink) => sink.output_key(),
            Self::Streaming(sink) | Self::ReplayBuffer(sink) | Self::NdiProgram(sink) => sink.output_key(),
        }
    }
    pub(crate) fn bind(&mut self, key: ProgramCaptureKey) -> Result<(), OutputError> {
        match self {
            Self::Recording(sink) => sink.bind(key).map_err(OutputError::Recording),
            Self::Streaming(sink) | Self::ReplayBuffer(sink) | Self::NdiProgram(sink) => sink.bind(key),
        }
    }
    fn submit(&mut self, key: ProgramCaptureKey, block: Arc<ProgramAudioBlock>) -> bool {
        match self {
            Self::Recording(sink) => sink.submit(key, block),
            Self::Streaming(sink) | Self::ReplayBuffer(sink) | Self::NdiProgram(sink) => sink.submit(key, block),
        }
    }
    fn selected_tracks(&self)->skylive_core::SourceAudioTrackMask{match self{
        Self::Recording(sink)=>sink.selected_tracks(),
        Self::Streaming(sink)|Self::ReplayBuffer(sink)|Self::NdiProgram(sink)=>skylive_core::SourceAudioTrackMask::NONE.with(sink.selected_track(),true),
    }}
    fn fail_scope(&self) {
        match self {
            Self::Recording(sink) => sink.fail_scope(),
            Self::Streaming(sink) | Self::ReplayBuffer(sink) | Self::NdiProgram(sink) => sink.fail_scope(),
        }
    }
    fn fail_backpressure(&self) {
        match self {
            Self::Recording(sink) => sink.fail_backpressure(),
            Self::Streaming(sink) | Self::ReplayBuffer(sink) | Self::NdiProgram(sink) => sink.fail_backpressure(),
        }
    }
    fn ready(&mut self) -> bool {
        match self {
            Self::Recording(sink) => !sink.finishing(),
            Self::Streaming(sink) | Self::ReplayBuffer(sink) | Self::NdiProgram(sink) => sink.poll_activation(),
        }
    }
}
#[derive(Default)]
pub struct ProgramOutputRouter {
    slots: [Option<AudioOutput>; 4],
}
impl ProgramOutputRouter {
    pub(crate) fn submit_live_except(&mut self,key:ProgramCaptureKey,block:Arc<ProgramAudioBlock>,recording_reader:bool,streaming_reader:bool){
        for (index,slot) in self.slots.iter_mut().enumerate(){
            if index==OutputKind::Recording.index() && recording_reader || index==OutputKind::Streaming.index() && streaming_reader {continue;}
            if slot.as_mut().is_some_and(|sink|!sink.submit(key,block.clone())){slot.take();}
        }
    }
    pub(crate) fn streaming_tracks(&self)->Option<skylive_core::SourceAudioTrackMask>{
        self.slots[OutputKind::Streaming.index()].as_ref().map(AudioOutput::selected_tracks)
    }
    pub(crate) fn submit_streaming(&mut self,key:ProgramCaptureKey,block:Arc<ProgramAudioBlock>)->bool{
        let slot=&mut self.slots[OutputKind::Streaming.index()];
        if slot.as_mut().is_some_and(|sink|!sink.submit(key,block)){slot.take();false}else{slot.is_some()}
    }
    pub(crate) fn fail_streaming_backpressure(&mut self){
        if let Some(sink)=self.slots[OutputKind::Streaming.index()].take(){sink.fail_backpressure();}
    }
    /// Publish a separately prepared recording block without ever replacing
    /// the block delivered to streaming, replay or NDI outputs.
    pub(crate) fn submit_recording(&mut self,key:ProgramCaptureKey,block:Arc<ProgramAudioBlock>)->bool {
        let slot=&mut self.slots[OutputKind::Recording.index()];
        if slot.as_mut().is_some_and(|sink|!sink.submit(key,block)) {slot.take();false} else {slot.is_some()}
    }
    /// Publish the ordinary Program mix only to non-recording sinks.
    pub(crate) fn submit_non_recording(&mut self,key:ProgramCaptureKey,block:Arc<ProgramAudioBlock>) {
        for slot in self.slots.iter_mut().skip(1) {
            if slot.as_mut().is_some_and(|sink|!sink.submit(key,block.clone())) {slot.take();}
        }
    }
    pub(crate) fn recording_tracks(&self)->Option<skylive_core::SourceAudioTrackMask> {
        self.slots[OutputKind::Recording.index()].as_ref().map(AudioOutput::selected_tracks)
    }
    pub(crate) fn recording_can_submit(&self,block:&ProgramAudioBlock)->bool {
        matches!(self.slots[OutputKind::Recording.index()].as_ref(),
            Some(AudioOutput::Recording(sink)) if sink.can_submit(block))
    }
    pub(crate) fn recording_may_submit(&self)->bool {
        matches!(self.slots[OutputKind::Recording.index()].as_ref(),
            Some(AudioOutput::Recording(sink)) if sink.may_submit())
    }
    pub(crate) fn recording_stop_requested(&self)->bool {
        matches!(self.slots[OutputKind::Recording.index()].as_ref(),
            Some(AudioOutput::Recording(sink)) if sink.stop_requested())
    }
    pub(crate) fn discard_fully_paused_recording(&self,start:u64,end:u64)->bool {
        match self.slots[OutputKind::Recording.index()].as_ref() {
            Some(AudioOutput::Recording(sink))=>sink.discard_fully_paused(start,end),_=>false,
        }
    }
    /// A bounded recording-only queue/history failure must not retire other
    /// outputs that share the Program owner.
    pub(crate) fn fail_recording_backpressure(&mut self) {
        if let Some(sink)=self.slots[OutputKind::Recording.index()].take(){sink.fail_backpressure();}
    }
    pub(crate) fn selected_tracks(&self)->skylive_core::SourceAudioTrackMask{
        self.slots.iter().flatten().fold(skylive_core::SourceAudioTrackMask::NONE,|mask,sink|mask.union(sink.selected_tracks()))
    }
    pub fn has(&self, kind: OutputKind) -> bool {
        self.slots[kind.index()].is_some()
    }
    pub(crate) fn attach(
        &mut self,
        key: ProgramCaptureKey,
        mut sink: AudioOutput,
    ) -> Result<(), OutputError> {
        let output = sink.key();
        output.validate()?;
        if self.has(output.kind) {
            return Err(OutputError::DuplicateKind(output.kind));
        }
        sink.bind(key)?;
        self.slots[output.kind.index()] = Some(sink);
        Ok(())
    }
    pub(crate) fn take(&mut self, kind: OutputKind) -> Option<AudioOutput> {
        self.slots[kind.index()].take()
    }
    pub fn detach(&mut self, key: OutputKey) -> bool {
        let slot = &mut self.slots[key.kind.index()];
        if slot.as_ref().is_some_and(|sink| sink.key() == key) {
            slot.take();
            true
        } else {
            false
        }
    }
    pub fn migrate_to(&mut self, candidate: &mut Self, key: ProgramCaptureKey) {
        for slot in &mut self.slots {
            if let Some(sink) = slot.take() {
                let _ = candidate.attach(key, sink);
            }
        }
    }
    pub(crate) fn prune(&mut self) {
        for slot in &mut self.slots {
            if slot.as_mut().is_some_and(|sink| !sink.ready()) {
                slot.take();
            }
        }
    }
    pub(crate) fn submit(&mut self, key: ProgramCaptureKey, block: Arc<ProgramAudioBlock>) {
        // No short-circuit across slots: one failure never suppresses the other.
        for slot in &mut self.slots {
            if slot
                .as_mut()
                .is_some_and(|sink| !sink.submit(key, block.clone()))
            {
                slot.take();
            }
        }
    }
    pub(crate) fn fail_scope(&mut self) {
        for slot in &mut self.slots {
            if let Some(sink) = slot.take() {
                sink.fail_scope();
            }
        }
    }
    pub(crate) fn fail_backpressure(&mut self) {
        for slot in &mut self.slots {
            if let Some(sink) = slot.take() {
                sink.fail_backpressure();
            }
        }
    }
}
