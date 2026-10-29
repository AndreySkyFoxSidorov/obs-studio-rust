use super::*;
use std::mem::size_of;

impl AudioPlan {
    /// Single-source pre-gain processing on the input grid. Transactional state
    /// publication is independent from any later output consumer/read window.
    pub(crate) fn process_converted_source(&self,request:AudioMixRequest,start:u64,
        original:Option<skylive_media::AudioFormat>,samples:&mut[f32],state:&mut AudioDspState)->Result<(),AudioError>
    {
        if self.sources.len()!=1||request.frames==0||request.frames>1024||request.sample_rate!=self.audio_settings.sample_rate()
            ||request.channels!=self.audio_settings.channels()||samples.len()!=request.frames*usize::from(request.channels){
            return Err(AudioError::InvalidRequest("source processing scope or block shape"));
        }
        let source=&self.sources[0];
        if let Some(index)=samples.iter().position(|sample|!sample.is_finite()) {return Err(AudioError::NonFiniteSample {source:source.source,index});}
        let transform=skylive_media::SourceChannelTransform::new(skylive_media::SourceChannelSettings {
            force_mono:source.properties.force_mono(),balance:source.properties.balance()},original,request.channels).map_err(AudioError::Media)?;
        let mut candidate=state.clone();
        candidate.prepare_source_input(self,request,start)?;
        for frame in samples.chunks_exact_mut(usize::from(request.channels)){transform.process_frame(frame).map_err(AudioError::Media)?;}
        if !source.filters.is_empty(){let mut envelope=vec![0.0;request.frames];candidate.process_source_block(source.source,samples,&mut envelope)?;}
        if let Some(index)=samples.iter().position(|sample|!sample.is_finite()) {return Err(AudioError::NonFiniteSample {source:source.source,index});}
        candidate.finish(start,request.frames)?;
        *state=candidate;
        Ok(())
    }

    pub fn mix(
        &self,
        request: AudioMixRequest,
        inputs: &[PcmInput<'_>],
    ) -> Result<MixedAudio, AudioError> {
        self.mix_with_limits(request, inputs, AudioMixLimits::default())
    }

    /// Smaller byte budgets may reject a supported block before output
    /// allocation. This CPU evaluator is not suitable for real-time callbacks.
    pub fn mix_with_limits(
        &self,
        request: AudioMixRequest,
        inputs: &[PcmInput<'_>],
        limits: AudioMixLimits,
    ) -> Result<MixedAudio, AudioError> {
        self.mix_grid_with_limits(request, inputs, limits, 0)
    }

    /// Evaluate a retained route span on the absolute output sample grid.
    /// Unlike adding independently rounded local offsets, this preserves the
    /// sub-nanosecond phase of nonzero block starts at rates such as 44.1 kHz.
    pub fn mix_on_grid(
        &self,
        request: AudioMixRequest,
        inputs: &[PcmInput<'_>],
        start_frame: u64,
    ) -> Result<MixedAudio, AudioError> {
        self.mix_grid_with_limits(request, inputs, AudioMixLimits::default(), start_frame)
    }

    fn mix_grid_with_limits(
        &self,
        request: AudioMixRequest,
        inputs: &[PcmInput<'_>],
        limits: AudioMixLimits,
        start_frame: u64,
    ) -> Result<MixedAudio, AudioError> {
        // Standalone snapshot evaluation starts fresh DSP history explicitly.
        let mut state = AudioDspState::default();
        self.mix_grid_state(request, inputs, limits, start_frame, &mut state, None)
    }
    /// Persistent Program evaluation. A rejected block cannot advance envelope
    /// history: publish the bounded candidate only after the complete mix.
    pub fn mix_on_grid_with_dsp(
        &self,
        request: AudioMixRequest,
        inputs: &[PcmInput<'_>],
        start_frame: u64,
        state: &mut AudioDspState,
    ) -> Result<MixedAudio, AudioError> {
        let mut candidate = state.clone();
        let mixed = self.mix_grid_state(
            request,
            inputs,
            AudioMixLimits::default(),
            start_frame,
            &mut candidate,
            None,
        )?;
        *state = candidate;
        Ok(mixed)
    }
    pub(crate) fn mix_processed_on_grid(&self,request:AudioMixRequest,inputs:&[PcmInput<'_>],start:u64,
        actions:&dyn Fn(Id,u64)->Option<(f32,bool)>)->Result<MixedAudio,AudioError>{
        let plan=self.processed_output_plan();let mut state=AudioDspState::default();
        plan.mix_grid_state(request,inputs,AudioMixLimits::default(),start,&mut state,Some(actions))
    }
    pub(crate) fn mix_processed_on_grid_track(&self,request:AudioMixRequest,inputs:&[PcmInput<'_>],start:u64,
        actions:&dyn Fn(Id,u64)->Option<(f32,bool)>,track:skylive_core::AudioTrackId)->Result<MixedAudio,AudioError>{
        let plan=self.processed_output_plan();let mut state=AudioDspState::default();
        plan.mix_grid_state_track(request,inputs,AudioMixLimits::default(),start,&mut state,Some(actions),track,false,None)
    }
    /// Optional post-DSP/post-gain source-sample tap for a caller-owned meter.
    /// The callback is invoked only during evaluation and is not a commit or
    /// presentation acknowledgement. Callers must stage its state and publish
    /// it only if the complete mix succeeds. No extra PCM buffer is allocated.
    pub(crate) fn mix_processed_on_grid_metered(&self,request:AudioMixRequest,inputs:&[PcmInput<'_>],start:u64,
        actions:&dyn Fn(Id,u64)->Option<(f32,bool)>,tap:&mut dyn FnMut(Id,u64,usize,f64))->Result<MixedAudio,AudioError>{
        let plan=self.processed_output_plan();let mut state=AudioDspState::default();
        plan.mix_grid_state_track(request,inputs,AudioMixLimits::default(),start,&mut state,Some(actions),skylive_core::AudioTrackId::ONE,true,Some(tap))
    }
    fn mix_grid_state(
        &self,
        request: AudioMixRequest,
        inputs: &[PcmInput<'_>],
        limits: AudioMixLimits,
        start_frame: u64,
        state: &mut AudioDspState,
        actions: Option<&dyn Fn(Id,u64)->Option<(f32,bool)>>,
    ) -> Result<MixedAudio, AudioError> {
        self.mix_grid_state_track(request,inputs,limits,start_frame,state,actions,skylive_core::AudioTrackId::ONE,true,None)
    }
    fn mix_grid_state_track(&self,request:AudioMixRequest,inputs:&[PcmInput<'_>],limits:AudioMixLimits,
        start_frame:u64,state:&mut AudioDspState,actions:Option<&dyn Fn(Id,u64)->Option<(f32,bool)>>,
        track:skylive_core::AudioTrackId,include_monitor:bool,
        mut meter_tap:Option<&mut dyn FnMut(Id,u64,usize,f64)>)->Result<MixedAudio,AudioError>{
        if !(8000..=192000).contains(&request.sample_rate) {
            return Err(AudioError::InvalidRequest(
                "sample rate must be 8000..192000 Hz",
            ));
        }
        if self.sources.iter().flat_map(|source|&source.filters).any(|filter|filter.sidechain_source().is_some()){
            return Err(AudioError::InvalidRequest("external compressor keys require timestamped AudioProgram post-DSP signals"));
        }
        if self.sources.iter().any(|source|source.properties.sync_offset_ms()!=0){
            return Err(AudioError::InvalidRequest("source sync offsets require timestamped AudioProgram post-DSP history; a direct snapshot mix has no past/future samples"));
        }
        let channels = usize::from(request.channels);
        if channels == 0 || channels > MAX_AUDIO_CHANNELS {
            return Err(AudioError::InvalidRequest("channels must be 1..8"));
        }
        if request.frames == 0 || request.frames > MAX_AUDIO_FRAMES {
            return Err(AudioError::InvalidRequest("frames must be 1..4096"));
        }
        if inputs.len() > MAX_AUDIO_INPUTS || self.sources.len() > MAX_AUDIO_INPUTS {
            return Err(AudioError::Limit("PCM input count exceeds 64"));
        }
        if limits.max_total_bytes == 0 || limits.max_total_bytes > MAX_AUDIO_MIX_BYTES {
            return Err(AudioError::InvalidRequest(
                "mix byte ceiling must be 1..16 MiB",
            ));
        }
        let sample_count = request
            .frames
            .checked_mul(channels)
            .ok_or(AudioError::Limit("PCM sample count overflow"))?;
        let last_offset = sample_offset(request.frames - 1, request.sample_rate, start_frame)?;
        let final_advance = request
            .advance_ns
            .checked_add(last_offset)
            .ok_or(AudioError::TimeOverflow)?;
        self.curve.factors(final_advance)?;

        // Fixed admission metadata is stack-owned; no source-count dependent
        // buffer exists until every shape and byte/time budget has passed.
        let mut positions = [None; MAX_AUDIO_INPUTS];
        let mut input_bytes = 0usize;
        for (position, input) in inputs.iter().enumerate() {
            let source = input.source();
            let index = self
                .sources
                .iter()
                .position(|planned| planned.source == source)
                .ok_or(AudioError::UnexpectedInput(source))?;
            if positions[index].replace(position).is_some() {
                return Err(AudioError::DuplicateInput(source));
            }
            if let Some((sample_rate, actual_channels, samples)) = input.buffer() {
                if sample_rate != request.sample_rate || actual_channels != request.channels {
                    return Err(AudioError::FormatMismatch(source));
                }
                if samples.len() != sample_count {
                    return Err(AudioError::InvalidBuffer(source));
                }
                input_bytes = input_bytes
                    .checked_add(
                        samples
                            .len()
                            .checked_mul(size_of::<f32>())
                            .ok_or(AudioError::Limit("PCM input byte overflow"))?,
                    )
                    .ok_or(AudioError::Limit("PCM input byte overflow"))?;
            }
        }
        let mut transforms = [None; MAX_AUDIO_INPUTS];
        for (index, source) in self.sources.iter().enumerate() {
            if positions[index].is_none() {
                return Err(AudioError::MissingInput(source.source));
            }
            let origin = inputs[positions[index].ok_or(AudioError::MissingInput(source.source))?]
                .original()
                .map_err(AudioError::Media)?;
            transforms[index] = Some(
                skylive_media::SourceChannelTransform::new(
                    skylive_media::SourceChannelSettings {
                        force_mono: source.properties.force_mono(),
                        balance: source.properties.balance(),
                    },
                    origin,
                    request.channels,
                )
                .map_err(AudioError::Media)?,
            );
            if source.filters.len() > MAX_SOURCE_AUDIO_FILTERS {
                return Err(AudioError::Limit("source audio filter count exceeds 8"));
            }
            for filter in &source.filters {
                filter
                    .validate()
                    .map_err(|_| AudioError::InvalidRequest("invalid source audio filter"))?;
            }
        }
        let monitor=include_monitor&&self.has_monitoring();
        let work_bytes = accounted_bytes(
            input_bytes,
            sample_count,
            request.frames,
            self.sources.len(),
            channels,
        )?;
        let work_bytes=work_bytes.checked_add(if monitor {sample_count*12}else{0}).ok_or(AudioError::Limit("monitor workspace overflow"))?;
        let current=state.craft_heap_bytes();let required=self.craft_state_bytes()?;
        let craft_peak=current.checked_add(current.max(required).checked_mul(2).ok_or(AudioError::Limit("Craft snapshot byte overflow"))?)
            .ok_or(AudioError::Limit("Craft snapshot byte overflow"))?;
        let work_bytes=work_bytes.checked_add(craft_peak).ok_or(AudioError::Limit("Craft mixing workspace overflow"))?;
        if work_bytes > limits.max_total_bytes {
            return Err(AudioError::Limit(
                "PCM input and mixing workspace byte ceiling",
            ));
        }
        // Validate the complete boundary even for muted, shared, endpoint-zero
        // weight or eventually cancelling samples. Silence is always explicit.
        for input in inputs {
            if let Some((_, _, samples)) = input.buffer() {
                if let Some(index) = samples.iter().position(|value| !value.is_finite()) {
                    return Err(AudioError::NonFiniteSample {
                        source: input.source(),
                        index,
                    });
                }
            }
        }

        state.prepare(self, request, start_frame)?;

        let mut weights = allocate_vec(request.frames, (0.0f32, 0.0f32))?;
        for (frame, factors) in weights.iter_mut().enumerate() {
            let offset = request
                .advance_ns
                .checked_add(sample_offset(frame, request.sample_rate, start_frame)?)
                .ok_or(AudioError::TimeOverflow)?;
            *factors = self.curve.factors(offset)?;
        }
        let mut accumulation = allocate_vec(sample_count, 0.0f64)?;
        let mut monitor_accumulation=if monitor {allocate_vec(sample_count,0.0f64)?}else{Vec::new()};
        // One source at a time. A Mix/control-constant span is the explicit
        // OBS callback boundary; linked dynamics never runs frame-by-frame.
        let mut source_work = allocate_vec(sample_count, 0.0f32)?;
        let mut envelope_work = allocate_vec(request.frames, 0.0f32)?;
        let mut source_peaks = [[0.0f64; MAX_AUDIO_CHANNELS]; MAX_AUDIO_INPUTS];
        let mut source_squares = [[0.0f64; MAX_AUDIO_CHANNELS]; MAX_AUDIO_INPUTS];
        for (index, source) in self.sources.iter().enumerate() {
            let input = &inputs[positions[index].ok_or(AudioError::MissingInput(source.source))?];
            let samples = input.buffer().map(|(_, _, samples)| samples);
            let transform = transforms[index].ok_or(AudioError::InvalidRequest(
                "source channel transform not admitted",
            ))?;
            let process = !source.filters.is_empty() || !transform.is_bypassed();
            if samples.is_none() && source.filters.is_empty() {
                continue;
            }
            let gain = if source.muted {
                0.0
            } else {
                f64::from(source.gain)
            };
            if process {
                match samples {
                    Some(samples) => source_work.copy_from_slice(samples),
                    None => source_work.fill(0.),
                }
                for frame in source_work.chunks_exact_mut(channels) {
                    transform.process_frame(frame).map_err(AudioError::Media)?;
                }
                if !source.filters.is_empty() {
                    state.process_source_block(
                        source.source,
                        &mut source_work,
                        &mut envelope_work,
                    )?;
                }
            }
            for (frame, factors) in weights.iter().enumerate() {
                let gain=if let Some((gain,muted))=actions.and_then(|actions|actions(source.source,start_frame+frame as u64)) {
                    if !gain.is_finite()||gain<0.0{return Err(AudioError::InvalidRequest("invalid timestamped source volume"));}
                    if muted{0.0}else{f64::from(gain)}
                }else{gain};
                let weight = if source.monitoring==skylive_core::AudioMonitoringMode::MonitorOnly {0.0}else{source.routes.weight(*factors)};
                for channel in 0..channels {
                    let position = frame * channels + channel;
                    let input = if process {
                        source_work[position]
                    } else {
                        samples.map_or(0., |s| s[position])
                    };
                    let value = f64::from(input) * gain;
                    if let Some(tap)=meter_tap.as_mut(){tap(source.source,start_frame+frame as u64,channel,value);}
                    source_peaks[index][channel] = source_peaks[index][channel].max(value.abs());
                    source_squares[index][channel] += value * value;
                    let track_weight=if source.properties.audio_tracks().contains(track){weight}else{0.0};
                    accumulation[position] += value * track_weight;
                    if include_monitor&&!source.monitoring.is_off() {monitor_accumulation[position]+=value;}
                }
            }
        }
        let mut output = allocate_vec(sample_count, 0.0f32)?;
        let mut peaks = [0.0f64; MAX_AUDIO_CHANNELS];
        let mut squares = [0.0f64; MAX_AUDIO_CHANNELS];
        let mut clipped = [0usize; MAX_AUDIO_CHANNELS];
        for (index, (sum, target)) in accumulation.iter().zip(&mut output).enumerate() {
            let channel = index % channels;
            if *sum < -1.0 || *sum > 1.0 {
                clipped[channel] += 1;
            }
            *target = sum.clamp(-1.0, 1.0) as f32;
            let value = f64::from(*target);
            peaks[channel] = peaks[channel].max(value.abs());
            squares[channel] += value * value;
        }
        let mut monitor_samples=if monitor {allocate_vec(sample_count,0.0f32)?}else{Vec::new()};
        for (output,value) in monitor_samples.iter_mut().zip(&monitor_accumulation){*output=value.clamp(-1.0,1.0)as f32;}
        let mut source_meters = Vec::new();
        source_meters
            .try_reserve_exact(self.sources.len())
            .map_err(|_| AudioError::Limit("source meter allocation"))?;
        for (index, source) in self.sources.iter().enumerate() {
            source_meters.push(SourceMeter {
                source: source.source,
                channels: meters(
                    &source_peaks[index],
                    &source_squares[index],
                    channels,
                    request.frames,
                )?,
            });
        }
        let mut clipped_samples = Vec::new();
        clipped_samples
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::Limit("clipping meter allocation"))?;
        clipped_samples.extend_from_slice(&clipped[..channels]);
        state.finish(start_frame, request.frames)?;
        Ok(MixedAudio {
            session: self.session,
            revision: self.revision,
            output_key: self.output_key,
            request,
            samples: output,
            monitor_samples,
            source_meters,
            program_meters: meters(&peaks, &squares, channels, request.frames)?,
            clipped_samples,
        })
    }
}
fn allocate_vec<T: Clone>(length: usize, value: T) -> Result<Vec<T>, AudioError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| AudioError::Limit("audio workspace allocation"))?;
    output.resize(length, value);
    Ok(output)
}
fn meters(
    peaks: &[f64; MAX_AUDIO_CHANNELS],
    squares: &[f64; MAX_AUDIO_CHANNELS],
    channels: usize,
    frames: usize,
) -> Result<Vec<ChannelMeter>, AudioError> {
    let mut meters = Vec::new();
    meters
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::Limit("channel meter allocation"))?;
    for channel in 0..channels {
        meters.push(ChannelMeter {
            peak: peaks[channel],
            rms: (squares[channel] / frames as f64).sqrt(),
        });
    }
    Ok(meters)
}
fn sample_offset(index: usize, rate: u32, start_frame: u64) -> Result<u64, AudioError> {
    let frame = start_frame
        .checked_add(index as u64)
        .ok_or(AudioError::TimeOverflow)?;
    let start_ns = u64::try_from(u128::from(start_frame) * 1_000_000_000 / u128::from(rate))
        .map_err(|_| AudioError::TimeOverflow)?;
    let current_ns = u64::try_from(u128::from(frame) * 1_000_000_000 / u128::from(rate))
        .map_err(|_| AudioError::TimeOverflow)?;
    current_ns
        .checked_sub(start_ns)
        .ok_or(AudioError::TimeOverflow)
}
fn accounted_bytes(
    input: usize,
    samples: usize,
    frames: usize,
    sources: usize,
    channels: usize,
) -> Result<usize, AudioError> {
    let mut total = input;
    total = total
        .checked_add(3 * MAX_DSP_STATE_BYTES)
        .ok_or(AudioError::Limit("filter candidate state byte overflow"))?;
    // Read-only input buffers, f64 sums, f32 output, shared per-frame factors,
    // source/program result meter buffers and fixed stack working arrays.
    for (count, bytes) in [
        (samples, size_of::<f64>() + 2 * size_of::<f32>()),
        (frames, size_of::<f32>()),
        (frames, size_of::<(f32, f32)>()),
        (sources, size_of::<SourceMeter>()),
        (sources * channels + channels, size_of::<ChannelMeter>()),
        (channels, size_of::<usize>()),
        (MAX_AUDIO_INPUTS * MAX_AUDIO_CHANNELS * 2, size_of::<f64>()),
        (MAX_AUDIO_INPUTS, size_of::<Option<usize>>()),
        (MAX_AUDIO_CHANNELS * 2, size_of::<f64>()),
        (MAX_AUDIO_CHANNELS, size_of::<usize>()),
        (MAX_SOURCE_AUDIO_FILTERS, size_of::<Option<StereoPan>>()),
        (MAX_AUDIO_CHANNELS, size_of::<f32>()),
    ] {
        total = total
            .checked_add(
                count
                    .checked_mul(bytes)
                    .ok_or(AudioError::Limit("audio workspace byte overflow"))?,
            )
            .ok_or(AudioError::Limit("audio workspace byte overflow"))?;
    }
    Ok(total)
}
