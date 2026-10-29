use super::*;
use crate::{
    audio_output::{AudioDelivery, StreamingAudioControl, StreamingAudioReceiver},
    native_aac,
};
use skylive_media::{AudioConvertLimits, AudioConverter, AudioFormat};
type PcmChunk = (u64, Box<[i16]>);
/// One bounded converter; no PCM is copied into an additional unbounded FIFO.
pub(crate) struct AudioPump {
    key: crate::audio_output::OutputKey,
    settings: AudioSettings,
    receiver: StreamingAudioReceiver,
    pub control: StreamingAudioControl,
    converter: AudioConverter,
    current: Option<AudioDelivery>,
    gap_left: u64,
    offset: usize,
    next_input: u64,
    next_output: u64,
    active: bool,
    sealed: bool,
    catchup:bool,
    seen_offline:u64,
    epoch_anchor:Option<crate::audio_output::OutputAnchor>,
    epoch_offset:Option<i64>,
    scratch: Box<[f32]>,
    zeros: Box<[f32]>,
}
impl AudioPump {
    pub(crate) fn new(
        key: StreamKey,
        settings: AudioSettings,
        receiver: StreamingAudioReceiver,
        control: StreamingAudioControl,
    ) -> Result<Self, Error> {
        let input = AudioFormat::new(
            settings.sample_rate(),
            settings.channels(),
            settings.channel_mask(),
        )
        .map_err(|e| Error::Audio(e.to_string()))?;
        let channels = if settings.channels() == 1 { 1 } else { 2 };
        let output = AudioFormat::new(48000, channels, if channels == 1 { 4 } else { 3 })
            .map_err(|e| Error::Audio(e.to_string()))?;
        let converter = AudioConverter::new(input, output, AudioConvertLimits{max_bytes:2*1024*1024,..Default::default()})
            .map_err(|e| Error::Audio(e.to_string()))?;
        Ok(Self {
            key: key.audio(),
            settings,
            receiver,
            control,
            converter,
            current: None,
            gap_left: 0,
            offset: 0,
            next_input: 0,
            next_output: 0,
            active: false,
            sealed: false,
            catchup:false,seen_offline:0,epoch_anchor:None,epoch_offset:None,
            scratch: vec![0.; 1024 * usize::from(channels)].into_boxed_slice(),
            zeros: vec![0.; 1024 * usize::from(settings.channels())].into_boxed_slice(),
        })
    }
    pub(crate) fn activate(&mut self, origin: MediaTime) -> Result<i64, Error> {
        let was_active=self.active;
        let anchor=if self.active {
            // The Program delivery anchor is immutable and remains on its
            // original absolute grid. Only this new codec epoch's converter
            // and relative clock reset; offline PCM is never relabeled.
            let input=AudioFormat::new(self.settings.sample_rate(),self.settings.channels(),self.settings.channel_mask()).map_err(|e|Error::Audio(e.to_string()))?;
            let channels=if self.settings.channels()==1{1}else{2};let output=AudioFormat::new(48000,channels,if channels==1{4}else{3}).map_err(|e|Error::Audio(e.to_string()))?;
            self.converter=AudioConverter::new(input,output,AudioConvertLimits{max_bytes:2*1024*1024,..Default::default()}).map_err(|e|Error::Audio(e.to_string()))?;
            self.current=None;self.gap_left=0;self.offset=0;self.next_output=0;self.sealed=false;self.catchup=true;
            crate::audio_output::OutputAnchor::new(origin,self.settings).map_err(|e|Error::Audio(e.to_string()))?
        }else{self.control.activate(self.key,origin).map_err(|e|Error::Audio(e.to_string()))?};
        let resuming=self.control.reconnecting();
        self.next_input = if resuming{anchor.first_sample_frame().max(self.control.observed_end_frame())}else{anchor.first_sample_frame()};
        if resuming{self.control.resume_after(self.next_input);}
        self.active = true;self.catchup=was_active||resuming;self.epoch_anchor=Some(anchor);
        let offset=anchor.sample_pts_100ns(self.next_input).map_err(|e|Error::Audio(e.to_string()))?;self.epoch_offset=if self.catchup{None}else{Some(offset)};Ok(offset)
    }
    /// Discard only while transport is offline or awaiting a fresh epoch
    /// origin. At most two deliveries per owner tick, no copied PCM queue.
    pub(crate) fn current_offset(&self)->Option<i64>{self.epoch_offset}
    pub(crate) fn begin_reconnect(&self){self.control.begin_reconnect();}
    pub(crate) fn account_offline(&mut self,status:&mut Status)->Result<(),Error>{let total=self.control.reconnect_discarded_frames();let delta=total.checked_sub(self.seen_offline).ok_or(Error::State("offline audio counter moved backwards"))?;add(&mut status.reconnect_audio_discarded_input,delta)?;self.seen_offline=total;Ok(())}
    pub(crate) fn discard_reconnecting(&mut self,status:&mut Status)->Result<(),Error>{
        self.account_offline(status)?;
        if let Some(fault)=self.control.stats().fault{return Err(Error::Audio(format!("Program streaming sink: {fault:?}")));}
        if let Some(current)=self.current.take(){add(&mut status.reconnect_audio_discarded_input,(current.frames()-self.offset) as u64)?;}self.offset=0;self.gap_left=0;
        for _ in 0..2{let Some(delivery)=self.receiver.poll()else{break;};if delivery.output_key()!=self.key||delivery.anchor().sample_rate()!=self.settings.sample_rate(){return Err(Error::Scope);}add(&mut status.reconnect_audio_discarded_input,delivery.frames() as u64)?;}
        if self.receiver.drained(){return Err(Error::Audio("Program producer ended during reconnect".into()));}Ok(())
    }
    pub(crate) fn stop_at(&self,cutoff:MediaTime)->Result<(),Error>{self.control.request_stop_at(cutoff).map_err(|e|Error::Audio(e.to_string()))}
    pub(crate) fn stop(&self) {
        self.control.request_stop();
    }
    pub(crate) fn retire_discard(&mut self) -> bool {
        self.current = None;
        while self.receiver.poll().is_some() {}
        self.receiver.drained()
    }
    pub(crate) fn done(&self) -> bool {
        self.sealed && self.converter.stats().drained
    }
    /// At most one input chunk and one1024-frame output per actor iteration.
    pub(crate) fn step(&mut self, status: &mut Status) -> Result<Option<PcmChunk>, Error> {
        self.account_offline(status)?;
        if let Some(fault) = self.control.stats().fault {
            return Err(Error::Audio(format!("Program streaming sink: {fault:?}")));
        }
        if !self.active {
            return Ok(None);
        }
        if let Some(output) = self.pull(status)? {
            return Ok(Some(output));
        }
        if self.current.is_none() && !self.sealed {
            if let Some(delivery) = self.receiver.poll() {
                if delivery.output_key() != self.key
                    || delivery.anchor().sample_rate() != self.settings.sample_rate()
                {
                    return Err(Error::Scope);
                }
                let first=delivery.absolute_first_frame();let end=first.checked_add(delivery.frames() as u64).ok_or(Error::Limit("audio reconnect delivery extent"))?;
                if delivery.frames()==0||delivery.samples().len()!=delivery.frames()*usize::from(self.settings.channels()){return Err(Error::Invalid("audio delivery order/span"));}
                if self.catchup {
                    if end<=self.next_input{add(&mut status.reconnect_audio_discarded_input,delivery.frames() as u64)?;return Ok(None);}
                    self.offset=usize::try_from(self.next_input.saturating_sub(first)).map_err(|_|Error::Limit("audio reconnect offset"))?;
                    // A producer may finish one deliberate offline discard
                    // concurrently with resume. Start at the first REAL sample
                    // delivered, retain its positive AV offset, never fill that
                    // missing offline interval with fabricated PCM silence.
                    self.next_input=self.next_input.max(first);self.gap_left=0;
                    let anchor=self.epoch_anchor.ok_or(Error::State("reconnect audio epoch anchor"))?;self.epoch_offset=Some(anchor.sample_pts_100ns(self.next_input).map_err(|e|Error::Audio(e.to_string()))?);
                    add(&mut status.reconnect_audio_discarded_input,self.offset as u64)?;self.catchup=false;
                }else{
                    let expected=self.next_input.checked_add(delivery.gap_before_frames()).ok_or(Error::Limit("audio source cursor"))?;
                    if expected!=first{return Err(Error::Invalid("audio delivery order/span"));}
                    if delivery.gap_before_frames()>u64::from(self.settings.sample_rate()/10){return Err(Error::Limit("audio delivery gap exceeds100ms"));}
                    self.gap_left=delivery.gap_before_frames();self.offset=0;
                }
                self.current = Some(delivery);
            } else if self.receiver.drained() {
                self.converter
                    .finish()
                    .map_err(|e| Error::Audio(e.to_string()))?;
                self.sealed = true;
            }
        }
        let capacity = self.converter.input_capacity_frames().min(1024);
        if capacity != 0 {
            let channels = usize::from(self.settings.channels());
            if self.gap_left != 0 {
                let frames = capacity.min(self.gap_left as usize);
                self.converter
                    .push(&self.zeros[..frames * channels])
                    .map_err(|e| Error::Audio(e.to_string()))?;
                self.next_input = self
                    .next_input
                    .checked_add(frames as u64)
                    .ok_or(Error::Limit("audio source cursor"))?;
                self.gap_left -= frames as u64;
                add(&mut status.audio_gap_frames, frames as u64)?;
            } else if let Some(delivery) = self.current.as_ref() {
                let frames = capacity.min(delivery.frames() - self.offset);
                self.converter
                    .push(
                        &delivery.samples()
                            [self.offset * channels..(self.offset + frames) * channels],
                    )
                    .map_err(|e| Error::Audio(e.to_string()))?;
                self.next_input = self
                    .next_input
                    .checked_add(frames as u64)
                    .ok_or(Error::Limit("audio source cursor"))?;
                self.offset += frames;
                add(&mut status.audio_input_frames, frames as u64)?;
                if self.offset == delivery.frames() {
                    self.current = None;
                }
            }
        }
        self.pull(status)
    }
    fn pull(&mut self, status: &mut Status) -> Result<Option<PcmChunk>, Error> {
        let frames = self
            .converter
            .pull(&mut self.scratch)
            .map_err(|e| Error::Audio(e.to_string()))?;
        if frames == 0 {
            return Ok(None);
        }
        let first = self.next_output;
        let channels = self.converter.output_format().channels();
        let samples = native_aac::pcm16_interleaved(
            &self.scratch[..frames * usize::from(channels)],
            channels,
        )
        .map_err(|e| Error::Aac(e.to_string()))?;
        self.next_output = self
            .next_output
            .checked_add(frames as u64)
            .ok_or(Error::Limit("AAC input cursor"))?;
        add(&mut status.audio_converted_frames, frames as u64)?;
        Ok(Some((first, samples)))
    }
}
