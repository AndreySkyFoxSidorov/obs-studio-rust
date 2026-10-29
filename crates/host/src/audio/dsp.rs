//! Worker-owned source filter history. One source is evaluated once after route
//! deduplication; candidate state publishes only after a complete successful mix.
use super::*;
#[path = "craft_lofi.rs"] mod craft_lofi;
use craft_lofi::LoFi;
#[path = "craft_rectifier.rs"] mod craft_rectifier;
use craft_rectifier::Rectifier;
#[path = "craft_dither.rs"] mod craft_dither;
use craft_dither::Dither;
#[path = "craft_dc_removal.rs"] mod craft_dc_removal;
use craft_dc_removal::DcRemoval;
#[path = "craft_saturator.rs"] mod craft_saturator;
use craft_saturator::Saturator;
use skylive_core::CompressorDetector;
use skylive_media::craft_audio::{
    Compressor, CompressorParameters, Detector, Gate, GateParameters, SimpleEq, SimpleEqParameters,
};
use skylive_media::obs_audio::{ObsGain, invert_polarity_interleaved};
use skylive_media::obs_dynamics::{
    DynamicsLimits, ObsCompressor, ObsCompressorParameters, ObsDetector, ObsExpander,
    ObsExpanderParameters, ObsLimiter, ObsLimiterParameters, ObsNoiseGate, ObsNoiseGateParameters,
    ObsUpwardCompressor, ObsUpwardParameters,
};
use std::mem::size_of;
use skylive_media::craft_time::{ModDelay,ModDelayParameters,FdnReverb,FdnReverbParameters,CraftTimeLimits};
use skylive_media::obs_three_band_eq::{ObsThreeBandEq,ObsThreeBandEqParameters,ObsThreeBandEqLimits};
pub const MAX_DSP_STATE_BYTES: usize = 512 * 1024;
#[derive(Clone)]
enum Kernel {
    ObsNoiseSuppression { enabled: bool },
    CraftDither{processor:Dither,enabled:bool},
    CraftRectifier{processor:Rectifier,enabled:bool},
    CraftDcRemoval{processor:DcRemoval,enabled:bool},
    CraftLoFi{processor:LoFi,enabled:bool},
    CraftSaturator{processor:Saturator,enabled:bool},
    CraftModDelay{processor:Option<Box<ModDelay>>,enabled:bool},
    CraftFdnReverb{processor:Option<Box<FdnReverb>>,enabled:bool},
    ObsThreeBandEq {processor:Box<ObsThreeBandEq>,enabled:bool},
    ObsUpwardCompressor {processor:Box<ObsUpwardCompressor>,enabled:bool},
    ObsNoiseGate {processor:ObsNoiseGate,enabled:bool},
    ObsLimiter {
        processor: ObsLimiter,
        enabled: bool,
    },
    ObsCompressor {
        processor: ObsCompressor,
        sidechain:Option<Box<sidechain::DetectorQueue>>,
        enabled: bool,
    },
    ObsExpander {
        processor: Box<ObsExpander>,
        enabled: bool,
    },
    ObsGain {
        processor: ObsGain,
        enabled: bool,
    },
    ObsPolarityInvert {
        enabled: bool,
    },
    Gate {
        processor: Gate,
        enabled: bool,
    },
    SimpleEq {
        processor: Box<SimpleEq>,
        enabled: bool,
    },
    Pan(StereoPan),
    Compressor {
        processor: Compressor,
        enabled: bool,
    },
}
impl Kernel {
    fn new(filter: AudioFilter, rate: u32, channels: u16) -> Result<Self, AudioError> {
        Ok(match filter {
            AudioFilter::ObsNoiseSuppression {enabled} => Self::ObsNoiseSuppression {enabled},
            AudioFilter::CraftDither{enabled,..}=>Self::CraftDither{processor:Dither::new(channels,filter)?,enabled},
            AudioFilter::CraftRectifier{enabled,..}=>Self::CraftRectifier{processor:Rectifier::new(rate,channels,filter)?,enabled},
            AudioFilter::CraftDcRemoval{enabled,..}=>Self::CraftDcRemoval{processor:DcRemoval::new(rate,channels,filter)?,enabled},
            AudioFilter::CraftLoFi{enabled,..}=>Self::CraftLoFi{processor:LoFi::new(rate,channels,filter)?,enabled},
            AudioFilter::CraftSaturator{enabled,..}=>Self::CraftSaturator{processor:Saturator::new(rate,channels,filter)?,enabled},
            AudioFilter::CraftModDelay{enabled,..}=>Self::CraftModDelay{processor:if enabled{Some(Box::new(ModDelay::new(rate,channels,delay_parameters(filter)?,CraftTimeLimits::default()).map_err(AudioError::CraftTime)?))}else{None},enabled},
            AudioFilter::CraftFdnReverb{enabled,..}=>Self::CraftFdnReverb{processor:if enabled{Some(Box::new(FdnReverb::new(rate,channels,reverb_parameters(filter)?,CraftTimeLimits::default()).map_err(AudioError::CraftTime)?))}else{None},enabled},
            AudioFilter::ObsThreeBandEq {enabled,..}=>Self::ObsThreeBandEq {processor:Box::new(ObsThreeBandEq::new(rate,channels,obs_three_band_eq_parameters(filter)?).map_err(AudioError::ObsThreeBandEq)?),enabled},
            AudioFilter::ObsUpwardCompressor {enabled,..}=>Self::ObsUpwardCompressor {
                processor:Box::new(ObsUpwardCompressor::new(rate,channels,obs_upward_parameters(filter)?).map_err(AudioError::ObsDynamics)?),enabled,
            },
            AudioFilter::ObsNoiseGate {enabled,..}=>Self::ObsNoiseGate {
                processor:ObsNoiseGate::new(rate,channels,obs_noise_gate_parameters(filter)?).map_err(AudioError::ObsDynamics)?,enabled,
            },
            AudioFilter::ObsLimiter { enabled, .. } => Self::ObsLimiter {
                processor: ObsLimiter::new(rate, channels, obs_limiter_parameters(filter)?)
                    .map_err(AudioError::ObsDynamics)?,
                enabled,
            },
            AudioFilter::ObsCompressor { enabled, sidechain:key, .. } => Self::ObsCompressor {
                sidechain:key.map(|key|sidechain::DetectorQueue::new(key,channels)).transpose()?,
                processor: ObsCompressor::new(rate, channels, obs_compressor_parameters(filter)?)
                    .map_err(AudioError::ObsDynamics)?,
                enabled,
            },
            AudioFilter::ObsExpander { enabled, .. } => Self::ObsExpander {
                processor: Box::new(
                    ObsExpander::new(rate, channels, obs_expander_parameters(filter)?)
                        .map_err(AudioError::ObsDynamics)?,
                ),
                enabled,
            },
            AudioFilter::ObsGain { db, enabled } => Self::ObsGain {
                processor: ObsGain::new(db).map_err(AudioError::Media)?,
                enabled,
            },
            AudioFilter::ObsPolarityInvert { enabled } => Self::ObsPolarityInvert { enabled },
            AudioFilter::Gate { enabled, .. } => Self::Gate {
                processor: Gate::new(rate, channels, gate_parameters(filter)?)
                    .map_err(AudioError::Gate)?,
                enabled,
            },
            AudioFilter::SimpleEq { enabled, .. } => Self::SimpleEq {
                processor: Box::new(
                    SimpleEq::new(rate, channels, simple_eq_parameters(filter)?)
                        .map_err(AudioError::Eq)?,
                ),
                enabled,
            },
            AudioFilter::Pan { .. } => Self::Pan(StereoPan::new(pan_settings(filter)?)),
            AudioFilter::Compressor { enabled, .. } => Self::Compressor {
                processor: Compressor::new(rate, channels, compressor_parameters(filter)?)
                    .map_err(AudioError::Compressor)?,
                enabled,
            },
        })
    }
    fn update(&mut self, filter: AudioFilter,rate:u32,channels:u16) -> Result<(), AudioError> {
        match (self, filter) {
            (Self::ObsNoiseSuppression{enabled},AudioFilter::ObsNoiseSuppression{enabled:next})=>{*enabled=next;}
            (Self::CraftDither{processor,enabled},AudioFilter::CraftDither{enabled:next,..})=>{if *enabled!=next{processor.reset();}processor.update(filter)?;*enabled=next;}
            (Self::CraftRectifier{processor,enabled},AudioFilter::CraftRectifier{enabled:next,..})=>{if *enabled!=next{processor.reset();}processor.update(filter)?;*enabled=next;}
            (Self::CraftDcRemoval{processor,enabled},AudioFilter::CraftDcRemoval{enabled:next,..})=>{if *enabled!=next{processor.reset();}processor.update(filter)?;*enabled=next;}
            (Self::CraftLoFi{processor,enabled},AudioFilter::CraftLoFi{enabled:next,..})=>{processor.update(filter)?;*enabled=next;}
            (Self::CraftSaturator{processor,enabled},AudioFilter::CraftSaturator{enabled:next,..})=>{processor.update(filter)?;*enabled=next;}
            (Self::CraftModDelay{processor,enabled},AudioFilter::CraftModDelay{enabled:next,..})=>{
                if !next{*processor=None;}else if let Some(processor)=processor{processor.set_parameters(delay_parameters(filter)?).map_err(AudioError::CraftTime)?;}
                else{*processor=Some(Box::new(ModDelay::new(rate,channels,delay_parameters(filter)?,CraftTimeLimits::default()).map_err(AudioError::CraftTime)?));}
                *enabled=next;
            },
            (Self::CraftFdnReverb{processor,enabled},AudioFilter::CraftFdnReverb{enabled:next,..})=>{
                if !next{*processor=None;}else if let Some(processor)=processor{processor.set_parameters(reverb_parameters(filter)?).map_err(AudioError::CraftTime)?;}
                else{*processor=Some(Box::new(FdnReverb::new(rate,channels,reverb_parameters(filter)?,CraftTimeLimits::default()).map_err(AudioError::CraftTime)?));}
                *enabled=next;
            },
            (Self::ObsThreeBandEq {processor,enabled},AudioFilter::ObsThreeBandEq {enabled:next,..})=>{processor.set_parameters(obs_three_band_eq_parameters(filter)?);*enabled=next;}
            (Self::ObsUpwardCompressor {processor,enabled},AudioFilter::ObsUpwardCompressor {enabled:next,..})=>{
                processor.set_parameters(obs_upward_parameters(filter)?);*enabled=next;
            }
            (Self::ObsNoiseGate {processor,enabled},AudioFilter::ObsNoiseGate {enabled:next,..})=>{
                processor.set_parameters(obs_noise_gate_parameters(filter)?);
                *enabled=next;
            }
            (
                Self::ObsLimiter { processor, enabled },
                AudioFilter::ObsLimiter { enabled: next, .. },
            ) => {
                processor.set_parameters(obs_limiter_parameters(filter)?);
                *enabled = next;
            }
            (
                Self::ObsCompressor { processor, sidechain, enabled },
                AudioFilter::ObsCompressor { enabled: next, sidechain:key, .. },
            ) => {
                if sidechain.as_ref().map(|q|q.key())!=key{
                    *sidechain=key.map(|key|sidechain::DetectorQueue::new(key,processor.channels())).transpose()?;
                }
                processor.set_parameters(obs_compressor_parameters(filter)?);
                *enabled = next;
            }
            (
                Self::ObsExpander { processor, enabled },
                AudioFilter::ObsExpander { enabled: next, .. },
            ) => {
                processor.set_parameters(obs_expander_parameters(filter)?);
                *enabled = next;
            }
            (Self::ObsGain { processor, enabled }, AudioFilter::ObsGain { db, enabled: next }) => {
                processor.update(db).map_err(AudioError::Media)?;
                *enabled = next;
            }
            (
                Self::ObsPolarityInvert { enabled },
                AudioFilter::ObsPolarityInvert { enabled: next },
            ) => {
                *enabled = next;
            }
            (Self::Gate { processor, enabled }, AudioFilter::Gate { enabled: next, .. }) => {
                processor.set_parameters(gate_parameters(filter)?);
                if *enabled != next {
                    processor.reset();
                }
                *enabled = next;
            }
            (
                Self::SimpleEq { processor, enabled },
                AudioFilter::SimpleEq { enabled: next, .. },
            ) => {
                processor.set_parameters(simple_eq_parameters(filter)?);
                if *enabled != next {
                    processor.reset();
                }
                *enabled = next;
            }
            (Self::Pan(processor), AudioFilter::Pan { .. }) => {
                *processor = StereoPan::new(pan_settings(filter)?)
            }
            (
                Self::Compressor { processor, enabled },
                AudioFilter::Compressor { enabled: next, .. },
            ) => {
                processor.set_parameters(compressor_parameters(filter)?);
                if *enabled != next {
                    processor.reset();
                }
                *enabled = next;
            }
            _ => {
                return Err(AudioError::InvalidRequest(
                    "filter runtime kind changed without preparation",
                ));
            }
        }
        Ok(())
    }
    fn process_callback(
        &mut self,
        samples: &mut [f32],
        channels: usize,
        envelope: &mut [f32],
    ) -> Result<(), AudioError> {
        match self {
            Self::ObsNoiseSuppression{enabled:false}=>Ok(()),
            Self::ObsNoiseSuppression{enabled:true}=>Err(AudioError::InvalidRequest("RNNoise requires the delayed source-history owner")),
            Self::CraftDither{processor,enabled:true}=>processor.process(samples,channels),
            Self::CraftDither{enabled:false,..}=>Ok(()),
            Self::CraftRectifier{processor,enabled:true}=>processor.process(samples,channels),
            Self::CraftRectifier{enabled:false,..}=>Ok(()),
            Self::CraftDcRemoval{processor,enabled:true}=>processor.process(samples,channels),
            Self::CraftDcRemoval{enabled:false,..}=>Ok(()),
            Self::CraftLoFi{processor,enabled:true}=>processor.process(samples,channels),
            Self::CraftLoFi{enabled:false,..}=>Ok(()),
            Self::CraftSaturator{processor,enabled:true}=>processor.process(samples,channels),
            Self::CraftSaturator{enabled:false,..}=>Ok(()),
            Self::CraftModDelay{processor:Some(processor),enabled:true}=>processor.process(samples,CraftTimeLimits{max_frames:MAX_AUDIO_FRAMES,..Default::default()}).map_err(AudioError::CraftTime),
            Self::CraftFdnReverb{processor:Some(processor),enabled:true}=>processor.process(samples,CraftTimeLimits{max_frames:MAX_AUDIO_FRAMES,..Default::default()}).map_err(AudioError::CraftTime),
            Self::CraftModDelay{enabled:false,..}|Self::CraftFdnReverb{enabled:false,..}=>Ok(()),
            Self::CraftModDelay{..}|Self::CraftFdnReverb{..}=>Err(AudioError::InvalidRequest("Craft kernel is not prepared")),
            Self::ObsThreeBandEq {processor,enabled:true}=>processor.process_interleaved(samples,ObsThreeBandEqLimits::default()).map_err(AudioError::ObsThreeBandEq),
            Self::ObsThreeBandEq {enabled:false,..}=>Ok(()),
            Self::ObsUpwardCompressor {processor,enabled:true}=>processor.process_interleaved(samples,DynamicsLimits::default()).map_err(AudioError::ObsDynamics),
            Self::ObsUpwardCompressor {enabled:false,..}=>Ok(()),
            Self::ObsNoiseGate {processor,enabled:true}=>processor.process_interleaved(samples,DynamicsLimits::default()).map_err(AudioError::ObsDynamics),
            Self::ObsNoiseGate {enabled:false,..}=>Ok(()),
            Self::ObsLimiter {
                processor,
                enabled: true,
            } => processor
                .process_callback(samples, envelope, DynamicsLimits::default())
                .map_err(AudioError::ObsDynamics),
            Self::ObsCompressor {processor,sidechain,enabled:true}=>{
                if let Some(queue)=sidechain{
                    let frames=samples.len()/channels;
                    processor.process_sidechain_callback(samples,queue.detector(frames),envelope,DynamicsLimits::default())
                        .map_err(AudioError::ObsDynamics)
                }else{processor.process_callback(samples,envelope,DynamicsLimits::default()).map_err(AudioError::ObsDynamics)}
            },
            Self::ObsExpander {
                processor,
                enabled: true,
            } => processor
                .process_interleaved(samples, DynamicsLimits::default())
                .map_err(AudioError::ObsDynamics),
            Self::ObsLimiter { enabled: false, .. }
            | Self::ObsCompressor { enabled: false, .. }
            | Self::ObsExpander { enabled: false, .. } => Ok(()),
            _ => {
                for frame in samples.chunks_exact_mut(channels) {
                    self.process(frame)?;
                }
                Ok(())
            }
        }
    }
    fn process(&mut self, frame: &mut [f32]) -> Result<(), AudioError> {
        match self {
            Self::ObsNoiseSuppression{enabled:false}=>Ok(()),
            Self::ObsNoiseSuppression{enabled:true}=>Err(AudioError::InvalidRequest("RNNoise cannot be evaluated as an in-place frame")),
            Self::ObsThreeBandEq {processor,enabled:true}=>processor.process_interleaved(frame,ObsThreeBandEqLimits::default()).map_err(AudioError::ObsThreeBandEq),
            Self::ObsThreeBandEq {enabled:false,..}=>Ok(()),
            Self::ObsUpwardCompressor {processor,enabled:true}=>processor.process_interleaved(frame,DynamicsLimits::default()).map_err(AudioError::ObsDynamics),
            Self::ObsUpwardCompressor {enabled:false,..}=>Ok(()),
            Self::ObsNoiseGate {processor,enabled:true}=>processor.process_interleaved(frame,DynamicsLimits::default()).map_err(AudioError::ObsDynamics),
            Self::ObsNoiseGate {enabled:false,..}=>Ok(()),
            Self::CraftDither{..}|Self::CraftRectifier{..}|Self::CraftDcRemoval{..}|Self::CraftLoFi{..}|Self::CraftSaturator{..}|Self::CraftModDelay{..}|Self::CraftFdnReverb{..}|Self::ObsLimiter { .. } | Self::ObsCompressor { .. } | Self::ObsExpander { .. } => Err(
                AudioError::InvalidRequest("OBS dynamics requires an admitted source callback"),
            ),
            Self::ObsGain {
                processor,
                enabled: true,
            } => processor.process_frame(frame).map_err(AudioError::Media),
            Self::ObsPolarityInvert { enabled: true } => {
                let channels = u16::try_from(frame.len())
                    .map_err(|_| AudioError::InvalidRequest("filter channel count"))?;
                invert_polarity_interleaved(
                    frame,
                    channels,
                    skylive_media::PluginAudioLimits::default(),
                )
                .map_err(AudioError::Media)
            }
            Self::ObsGain { enabled: false, .. } | Self::ObsPolarityInvert { enabled: false } => {
                Ok(())
            }
            Self::Gate {
                processor,
                enabled: true,
            } => processor
                .process_interleaved(frame)
                .map_err(AudioError::Gate),
            Self::SimpleEq {
                processor,
                enabled: true,
            } => processor.process_interleaved(frame).map_err(AudioError::Eq),
            Self::Gate { enabled: false, .. } | Self::SimpleEq { enabled: false, .. } => Ok(()),
            Self::Pan(processor) => processor.process_frame(frame).map_err(AudioError::Media),
            Self::Compressor {
                processor,
                enabled: true,
            } => processor
                .process_interleaved(frame)
                .map_err(AudioError::Compressor),
            Self::Compressor { enabled: false, .. } => Ok(()),
        }
    }
}
#[derive(Clone)]
struct SourceState {
    source: Id,
    binding: AudioInputKey,
    filters: Vec<AudioFilter>,
    kernels: Vec<Kernel>,
}
#[derive(Clone, Default)]
pub struct AudioDspState {
    session: Option<u64>,
    rate: u32,
    channels: u16,
    next_frame: Option<u64>,
    sources: Vec<SourceState>,
}
impl AudioDspState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn reset_source(&mut self, source: Id) {
        self.sources.retain(|state| state.source != source);
    }
    pub fn retain_sources(&mut self, ids: &[Id]) {
        self.sources.retain(|state| ids.contains(&state.source));
    }
    pub fn gain_reduction_db(&self, source: Id, authored_index: usize) -> Option<f32> {
        let state = self.sources.iter().find(|state| state.source == source)?;
        let index = authored_index;
        match state.kernels.get(index)? {
            Kernel::ObsNoiseGate {processor,enabled:true}=>Some(processor.gain_reduction_db()),
            Kernel::Gate {
                processor,
                enabled: true,
            } => Some(processor.gain_reduction_db()),
            Kernel::Compressor {
                processor,
                enabled: true,
            } => Some(processor.gain_reduction_db()),
            _ => None,
        }
    }
    pub(crate) fn prepare(
        &mut self,
        plan: &AudioPlan,
        request: AudioMixRequest,
        start: u64,
    ) -> Result<(), AudioError> {
        // Admit the complete replacement before constructing any kernel/Box.
        // mix_on_grid_with_dsp owns the surrounding transactional candidate.
        if plan.craft_state_bytes()?>MAX_CRAFT_RETAINED{return Err(AudioError::Limit("Craft time-effect retained state ceiling"));}
        if planned_state_bytes(plan)? > MAX_DSP_STATE_BYTES {
            return Err(AudioError::Limit(
                "source filter retained state byte ceiling",
            ));
        }
        if self.session != Some(plan.session)
            || self.rate != request.sample_rate
            || self.channels != request.channels
            || self.next_frame.is_some_and(|next| next != start)
        {
            self.reset();
        }
        self.session = Some(plan.session);
        self.rate = request.sample_rate;
        self.channels = request.channels;
        self.sources.retain(|state| {
            plan.sources
                .iter()
                .any(|source| source.source == state.source && !source.filters.is_empty())
        });
        let count = plan
            .sources
            .iter()
            .filter(|source| !source.filters.is_empty())
            .count();
        self.sources
            .try_reserve_exact(count.saturating_sub(self.sources.len()))
            .map_err(|_| AudioError::Limit("filter state allocation"))?;
        for source in plan
            .sources
            .iter()
            .filter(|source| !source.filters.is_empty())
        {
            let same = self
                .sources
                .iter()
                .position(|state| state.source == source.source);
            let rebuild = same.is_none_or(|index| {
                let state = &self.sources[index];
                state.binding != source.binding
                    || state.filters.len() != source.filters.len()
                    || reordered(&state.filters, &source.filters)
                    || state
                        .filters
                        .iter()
                        .zip(&source.filters)
                        .any(|(a, b)| std::mem::discriminant(a) != std::mem::discriminant(b))
            });
            if rebuild {
                let mut kernels = Vec::new();
                kernels
                    .try_reserve_exact(source.filters.len())
                    .map_err(|_| AudioError::Limit("filter chain allocation"))?;
                for &filter in source.filters.iter() {
                    kernels.push(Kernel::new(filter, request.sample_rate, request.channels)?);
                }
                let state = SourceState {
                    source: source.source,
                    binding: source.binding.clone(),
                    filters: source.filters.clone().into_boxed_slice().into_vec(),
                    kernels: kernels.into_boxed_slice().into_vec(),
                };
                if let Some(index) = same {
                    self.sources[index] = state;
                } else {
                    self.sources.push(state);
                }
            } else if let Some(index) = same {
                let state = &mut self.sources[index];
                for (kernel, &filter) in state.kernels.iter_mut().zip(source.filters.iter()) {
                    kernel.update(filter,request.sample_rate,request.channels)?;
                }
                state.filters.clone_from(&source.filters);
            }
        }
        if self.sources.capacity() != self.sources.len() {
            self.sources = std::mem::take(&mut self.sources)
                .into_boxed_slice()
                .into_vec();
        }
        Ok(())
    }
    /// One source's original-input clock. Missing capture intervals are not
    /// synthesized as filter input; native discontinuities reset at the owner.
    pub(crate) fn prepare_source_input(&mut self,plan:&AudioPlan,request:AudioMixRequest,start:u64)->Result<(),AudioError>{
        if self.next_frame.is_some_and(|next|start<next){return Err(AudioError::InvalidRequest("source DSP input clock rewound"));}
        self.next_frame=Some(start);
        self.prepare(plan,request,start)
    }
    pub(crate) fn craft_heap_bytes(&self)->usize{
        self.sources.iter().flat_map(|state|&state.kernels).map(|kernel|match kernel{
            Kernel::CraftModDelay{processor:Some(processor),..}=>processor.state_bytes(),
            Kernel::CraftFdnReverb{processor:Some(processor),..}=>processor.state_bytes(),Kernel::CraftSaturator{processor,..}=>processor.state_bytes(),Kernel::CraftLoFi{processor,..}=>processor.state_bytes(),Kernel::CraftDcRemoval{processor,..}=>processor.state_bytes(),Kernel::CraftRectifier{processor,..}=>processor.state_bytes(),Kernel::CraftDither{processor,..}=>processor.state_bytes(),_=>0,
        }).sum()
    }
    pub(crate) fn prepare_detector_input(&mut self,plan:&AudioPlan,start:u64)->Result<(),AudioError>{
        self.prepare_source_input(plan,AudioMixRequest{sample_rate:plan.audio_settings.sample_rate(),channels:plan.audio_settings.channels(),frames:1,advance_ns:0},start)
    }
    pub(crate) fn check_signal(&self,stamp:sidechain::SignalStamp,frames:usize)->Result<(),AudioError>{
        for state in &self.sources{for kernel in &state.kernels{if let Kernel::ObsCompressor{sidechain:Some(q),..}=kernel{
            if Some(q.key())==stamp.source{q.check(stamp,frames)?;}
        }}}Ok(())
    }
    pub(crate) fn publish_signal(&mut self,stamp:sidechain::SignalStamp,samples:&[f32],muted:bool){
        for state in &mut self.sources{for kernel in &mut state.kernels{if let Kernel::ObsCompressor{sidechain:Some(q),..}=kernel{
            if Some(q.key())==stamp.source{q.publish(stamp,samples,muted);}
        }}}
    }
    pub(crate) fn sidechain_sources(&mut self,keys:&[(Id,AudioInputKey,u64,u64)]){
        for state in &mut self.sources{for kernel in &mut state.kernels{if let Kernel::ObsCompressor{sidechain:Some(q),..}=kernel{q.available(keys);}}}
    }
    pub(crate) fn sidechain_cut(&mut self,source:Id,cut:u64){
        for state in &mut self.sources{for kernel in &mut state.kernels{if let Kernel::ObsCompressor{sidechain:Some(q),..}=kernel{q.cut(source,cut);}}}
    }
    /// Real point segment of an ordered source packet chain. A delayed marker
    /// inside the requested range is an error, never a fake passthrough.
    pub(crate) fn process_source_range(&mut self,source:Id,begin:usize,end:usize,
        samples:&mut[f32],envelope:&mut[f32])->Result<(),AudioError>{
        let channels=usize::from(self.channels);
        if channels==0||samples.len()%channels!=0||envelope.len()<samples.len()/channels{
            return Err(AudioError::InvalidRequest("source point segment shape"));
        }
        let state=self.sources.iter_mut().find(|state|state.source==source).ok_or(AudioError::MissingInput(source))?;
        let kernels=state.kernels.get_mut(begin..end).ok_or(AudioError::InvalidRequest("source point segment range"))?;
        for kernel in kernels{kernel.process_callback(samples,channels,envelope)?;}
        Ok(())
    }
    pub(crate) fn process_source_block(
        &mut self,
        source: Id,
        samples: &mut [f32],
        envelope: &mut [f32],
    ) -> Result<(), AudioError> {
        let channels = usize::from(self.channels);
        if channels == 0 || !samples.len().is_multiple_of(channels) {
            return Err(AudioError::InvalidRequest("source callback shape"));
        }
        let state = self
            .sources
            .iter_mut()
            .find(|state| state.source == source)
            .ok_or(AudioError::MissingInput(source))?;
        for kernel in &mut state.kernels {
            kernel.process_callback(samples, channels, envelope)?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn process_frame(
        &mut self,
        source: Id,
        frame: &mut [f32],
    ) -> Result<(), AudioError> {
        self.process_source_block(source, frame, &mut [0.0; 1])
    }
    pub(crate) fn finish(&mut self, start: u64, frames: usize) -> Result<(), AudioError> {
        self.next_frame = Some(
            start
                .checked_add(frames as u64)
                .ok_or(AudioError::TimeOverflow)?,
        );
        Ok(())
    }
}
pub fn compressor_parameters(filter: AudioFilter) -> Result<CompressorParameters, AudioError> {
    let AudioFilter::Compressor {
        threshold_db,
        ratio,
        knee_db,
        attack_ms,
        release_ms,
        makeup_db,
        detector,
        ..
    } = filter
    else {
        return Err(AudioError::InvalidRequest("filter is not a compressor"));
    };
    CompressorParameters::new(
        threshold_db,
        ratio,
        knee_db,
        attack_ms,
        release_ms,
        makeup_db,
        match detector {
            CompressorDetector::Peak => Detector::Peak,
            CompressorDetector::Rms => Detector::Rms,
        },
    )
    .map_err(AudioError::Compressor)
}

pub fn gate_parameters(filter: AudioFilter) -> Result<GateParameters, AudioError> {
    let AudioFilter::Gate {
        threshold_db,
        ratio,
        range_db,
        attack_ms,
        hold_ms,
        release_ms,
        ..
    } = filter
    else {
        return Err(AudioError::InvalidRequest("filter is not a gate"));
    };
    GateParameters::new(
        threshold_db,
        ratio,
        range_db,
        attack_ms,
        hold_ms,
        release_ms,
    )
    .map_err(AudioError::Gate)
}
pub fn simple_eq_parameters(filter: AudioFilter) -> Result<SimpleEqParameters, AudioError> {
    let AudioFilter::SimpleEq {
        low_db,
        low_freq_hz,
        mid_db,
        mid_freq_hz,
        mid_q,
        high_db,
        high_freq_hz,
        output_db,
        ..
    } = filter
    else {
        return Err(AudioError::InvalidRequest("filter is not a simple EQ"));
    };
    SimpleEqParameters::new(
        low_db,
        low_freq_hz,
        mid_db,
        mid_freq_hz,
        mid_q,
        high_db,
        high_freq_hz,
        output_db,
    )
    .map_err(AudioError::Eq)
}

fn binding_string_bytes(binding: &AudioInputKey) -> usize {
    match binding {
        AudioInputKey::Native(binding) | AudioInputKey::Window {binding,..} => match binding.target() {
            skylive_core::AudioTarget::WindowsEndpoint { id } => id.len(),
            skylive_core::AudioTarget::MacDevice { uid } => uid.len(),
            skylive_core::AudioTarget::WindowsProcess { identity } => identity.executable().len(),
            skylive_core::AudioTarget::WindowsWindow { selector } => selector.title().len()+selector.class().len()+selector.executable().len(),
            _ => 0,
        },
        AudioInputKey::EmbeddedCamera {binding,..}=>match binding.target(){skylive_core::VideoCaptureTarget::Windows{symbolic_link}=>symbolic_link.len(),skylive_core::VideoCaptureTarget::Macos{unique_id}=>unique_id.len()},
        AudioInputKey::Media { media, .. } => media.path().len(),
        AudioInputKey::Ndi {ndi,..}=>ndi.connection().sender.name.len()+ndi.connection().sender.url.as_ref().map_or(0,String::len),
        AudioInputKey::Browser(key)=>key.browser.css().len()+match key.browser.location(){skylive_core::BrowserLocation::Url{url}=>url.len(),skylive_core::BrowserLocation::LocalFile{path}=>path.len()},
    }
}
/// Source metadata and inline/boxed kernels, not PCM or allocator bookkeeping.
/// Binding constructors normalize String capacity; private vectors normalize
/// capacity on publication, so their retained payload agrees with this charge.
pub(crate) fn validate_plan_state(plan:&AudioPlan)->Result<usize,AudioError>{
    let bytes=planned_state_bytes(plan)?;
    if bytes>MAX_DSP_STATE_BYTES{return Err(AudioError::Limit("source filter retained state byte ceiling"));}
    Ok(bytes)
}
fn planned_state_bytes(plan: &AudioPlan) -> Result<usize, AudioError> {
    let mut bytes = size_of::<AudioDspState>();
    for source in plan
        .processing_sources()
        .filter(|source| !source.filters.is_empty())
    {
        let slots = source
            .filters
            .len()
            .checked_mul(size_of::<AudioFilter>() + size_of::<Kernel>())
            .ok_or(AudioError::Limit("filter retained byte overflow"))?;
        let boxed = source.filters.iter().try_fold(0usize, |bytes, filter| {
            let size = match filter {
                AudioFilter::ObsThreeBandEq {..}=>size_of::<ObsThreeBandEq>(),
                AudioFilter::ObsUpwardCompressor {..}=>size_of::<ObsUpwardCompressor>(),
                AudioFilter::SimpleEq { .. } => size_of::<SimpleEq>(),
                AudioFilter::ObsExpander { .. } => size_of::<ObsExpander>(),
                _ => 0,
            };
            bytes
                .checked_add(size)
                .ok_or(AudioError::Limit("boxed filter retained byte overflow"))
        })?;
        bytes = bytes
            .checked_add(size_of::<SourceState>())
            .and_then(|value| value.checked_add(binding_string_bytes(&source.binding)))
            .and_then(|value| value.checked_add(slots))
            .and_then(|value| value.checked_add(boxed))
            .ok_or(AudioError::Limit("filter retained byte overflow"))?;
    }
    Ok(bytes)
}

/// Anonymous chain positions cannot retain instance history across an observed
/// permutation. Parameter edits preserve history; exact multiset reorders reset.
pub(crate) fn reordered(before: &[AudioFilter], after: &[AudioFilter]) -> bool {
    before != after
        && before.len() == after.len()
        && before.iter().all(|filter| {
            before.iter().filter(|entry| *entry == filter).count()
                == after.iter().filter(|entry| *entry == filter).count()
        })
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn boxed_eq_retained_boundary_is_admitted_before_mutating_existing_history() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(skylive_core::Command::AddAudioCaptureSource {
                scene,
                name: "Budget".into(),
                binding: skylive_core::AudioCaptureBinding::new(
                    skylive_core::AudioFlow::Input,
                    skylive_core::AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .find(|entry| entry.id() == item)
            .unwrap()
            .source();
        session
            .apply(skylive_core::Command::SetSourceAudioFilters {
                source,
                filters: vec![AudioFilter::gate()],
            })
            .unwrap();
        let mut plan =
            derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(scene))).unwrap();
        let request = AudioMixRequest {
            sample_rate: 48000,
            channels: 2,
            frames: 1,
            advance_ns: 0,
        };
        let mut state = AudioDspState::default();
        state.prepare(&plan, request, 0).unwrap();
        state.process_frame(source, &mut [0.001, 0.001]).unwrap();
        let before = state.gain_reduction_db(source, 0);
        let mut template = plan.sources[0].clone();
        template.filters = vec![AudioFilter::simple_eq(); MAX_SOURCE_AUDIO_FILTERS];
        plan.sources.clear();
        for index in 0..MAX_AUDIO_INPUTS {
            let mut next = template.clone();
            next.source = Id::new(1000 + index as u64);
            plan.sources.push(next);
        }
        assert!(planned_state_bytes(&plan).unwrap() > MAX_DSP_STATE_BYTES);
        assert!(matches!(
            state.prepare(&plan, request, 0),
            Err(AudioError::Limit(_))
        ));
        assert_eq!(state.gain_reduction_db(source, 0), before);
        assert_eq!(state.sources.len(), 1);
        let pcm = [0.01, 0.02];
        let inputs: Vec<_> = plan
            .sources
            .iter()
            .map(|source| PcmInput::Samples {
                source: source.source,
                sample_rate: 48000,
                channels: 2,
                samples: &pcm,
            })
            .collect();
        assert!(matches!(
            plan.mix_on_grid_with_dsp(request, &inputs, 0, &mut state),
            Err(AudioError::Limit(_))
        ));
        assert_eq!(pcm, [0.01, 0.02]);
        assert_eq!(state.gain_reduction_db(source, 0), before);
        while planned_state_bytes(&plan).unwrap() > MAX_DSP_STATE_BYTES {
            plan.sources.pop();
        }
        assert!(!plan.sources.is_empty());
        let admitted = planned_state_bytes(&plan).unwrap();
        let mut extra = plan.clone();
        let mut next = template.clone();
        next.source = Id::new(9999);
        extra.sources.push(next);
        assert!(planned_state_bytes(&extra).unwrap() > MAX_DSP_STATE_BYTES);
        state.prepare(&plan, request, 0).unwrap();
        assert!(admitted <= MAX_DSP_STATE_BYTES);
        assert_eq!(state.sources.capacity(), state.sources.len());
        for source in &state.sources {
            assert_eq!(source.filters.capacity(), source.filters.len());
            assert_eq!(source.kernels.capacity(), source.kernels.len());
        }
        assert!(
            size_of::<Kernel>() < size_of::<SimpleEq>() / 2,
            "legacy slots must not each reserve an EQ body"
        );
        let mut string_plan = plan.clone();
        let length = 16_384;
        let key = AudioInputKey::Media {
            source: Id::new(123),
            media: skylive_core::MediaSource::new("a".repeat(length), 0, 0, true).unwrap(),
        };
        let old = binding_string_bytes(&string_plan.sources[0].binding);
        string_plan.sources[0].binding = key;
        assert_eq!(
            planned_state_bytes(&string_plan).unwrap(),
            admitted + length - old
        );
    }
}

pub fn obs_upward_parameters(filter:AudioFilter)->Result<ObsUpwardParameters,AudioError>{
    let AudioFilter::ObsUpwardCompressor {threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,detector,..}=filter else {
        return Err(AudioError::InvalidRequest("filter is not an OBS upward compressor"));
    };
    ObsUpwardParameters::new(threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,match detector{
        skylive_core::ObsExpanderDetector::Rms=>ObsDetector::Rms,skylive_core::ObsExpanderDetector::Peak=>ObsDetector::Peak,
    }).map_err(AudioError::ObsDynamics)
}
pub fn obs_noise_gate_parameters(filter:AudioFilter)->Result<ObsNoiseGateParameters,AudioError>{
    let AudioFilter::ObsNoiseGate {open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms,..}=filter else {
        return Err(AudioError::InvalidRequest("filter is not an OBS noise gate"));
    };
    ObsNoiseGateParameters::new(open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms).map_err(AudioError::ObsDynamics)
}
pub fn obs_limiter_parameters(filter: AudioFilter) -> Result<ObsLimiterParameters, AudioError> {
    let AudioFilter::ObsLimiter {
        threshold_db,
        release_ms,
        ..
    } = filter
    else {
        return Err(AudioError::InvalidRequest("filter is not OBS Limiter"));
    };
    ObsLimiterParameters::new(threshold_db, release_ms).map_err(AudioError::ObsDynamics)
}
pub fn obs_compressor_parameters(
    filter: AudioFilter,
) -> Result<ObsCompressorParameters, AudioError> {
    let AudioFilter::ObsCompressor {
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        output_db,
        ..
    } = filter
    else {
        return Err(AudioError::InvalidRequest("filter is not OBS Compressor"));
    };
    ObsCompressorParameters::new(threshold_db, ratio, attack_ms, release_ms, output_db)
        .map_err(AudioError::ObsDynamics)
}
pub fn obs_expander_parameters(filter: AudioFilter) -> Result<ObsExpanderParameters, AudioError> {
    let AudioFilter::ObsExpander {
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        output_db,
        detector,
        ..
    } = filter
    else {
        return Err(AudioError::InvalidRequest("filter is not OBS Expander"));
    };
    ObsExpanderParameters::new(
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        output_db,
        match detector {
            skylive_core::ObsExpanderDetector::Rms => ObsDetector::Rms,
            skylive_core::ObsExpanderDetector::Peak => ObsDetector::Peak,
        },
    )
    .map_err(AudioError::ObsDynamics)
}

#[cfg(test)]
mod obs_dynamics_budget_tests {
    include!("obs_dynamics_budget_tests.rs");
}

/// Fixed-band OBS parameters; the Craft equalizer has a different kernel/schema.
pub fn obs_three_band_eq_parameters(filter:AudioFilter)->Result<ObsThreeBandEqParameters,AudioError>{
    let AudioFilter::ObsThreeBandEq {low_db,mid_db,high_db,..}=filter else{return Err(AudioError::InvalidRequest("filter is not OBS 3-Band Equalizer"));};
    ObsThreeBandEqParameters::new(low_db,mid_db,high_db).map_err(AudioError::ObsThreeBandEq)
}

pub(crate) const MAX_CRAFT_RETAINED:usize=32*1024*1024;
pub(crate) fn craft_filter_bytes(filters:&[AudioFilter],rate:u32,channels:u16)->Result<usize,AudioError>{
    filters.iter().try_fold(0usize,|bytes,filter|{
        let next=match filter{
            AudioFilter::CraftModDelay{enabled:true,..}=>ModDelay::required_bytes(rate,channels).map_err(AudioError::CraftTime)?,
            AudioFilter::CraftFdnReverb{enabled:true,..}=>FdnReverb::required_bytes(rate,channels).map_err(AudioError::CraftTime)?,
            AudioFilter::CraftDither{..}=>usize::from(channels)*size_of::<f32>(),AudioFilter::CraftRectifier{..}=>usize::from(channels)*2*size_of::<f32>(),AudioFilter::CraftDcRemoval{..}=>usize::from(channels)*2*size_of::<f32>(),AudioFilter::CraftSaturator{..}=>usize::from(channels)*3*size_of::<f32>(),AudioFilter::CraftLoFi{..}=>usize::from(channels)*size_of::<f32>(),_=>0,
        };
        bytes.checked_add(next).ok_or(AudioError::Limit("Craft time-effect state byte overflow"))
    })
}
fn delay_parameters(filter:AudioFilter)->Result<ModDelayParameters,AudioError>{
    if let AudioFilter::CraftModDelay{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz,..}=filter{
        let parameters=ModDelayParameters{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz};parameters.validate().map_err(AudioError::CraftTime)?;Ok(parameters)
    }else{Err(AudioError::InvalidRequest("not a Craft Mod Delay"))}
}
fn reverb_parameters(filter:AudioFilter)->Result<FdnReverbParameters,AudioError>{
    if let AudioFilter::CraftFdnReverb{predelay_ms,decay_seconds,damping_pct,size_pct,mix_pct,..}=filter{
        let parameters=FdnReverbParameters{predelay_ms,decay_seconds,damping_pct,size_pct,mix_pct};parameters.validate().map_err(AudioError::CraftTime)?;Ok(parameters)
    }else{Err(AudioError::InvalidRequest("not a Craft FDN Reverb"))}
}
