//! Real donor time-effect controls. Draft only; canonical publication is shared.
use eframe::egui;use skylive_core::AudioFilter;
fn slider(ui:&mut egui::Ui,label:&str,value:&mut f32,min:f32,max:f32,log:bool,suffix:&str,hint:&str){
 let name=ui.label(crate::i18n::tr(ui,label));
 ui.add(egui::Slider::new(value,min..=max).logarithmic(log).suffix(suffix)).labelled_by(name.id)
  .on_hover_text(crate::i18n::tr(ui,hint)).on_disabled_hover_text(crate::i18n::tr(ui,hint));ui.end_row();
}
fn sat_slider(ui:&mut egui::Ui,key:&str,label:&str,value:&mut f32,min:f32,max:f32,log:bool,suffix:&str){
 let name=ui.label(crate::i18n::tr_key_or(ui,key,label));
 ui.add(egui::Slider::new(value,min..=max).logarithmic(log).suffix(suffix)).labelled_by(name.id).on_hover_text(crate::i18n::tr_key_or(ui,key,label));ui.end_row();
}
fn lofi_slider(ui:&mut egui::Ui,key:&str,label:&str,hint_key:&str,hint:&str,value:&mut f32,min:f32,max:f32,log:bool,suffix:&str){
 let name=ui.label(crate::i18n::tr_key_or(ui,key,label));
 ui.add(egui::Slider::new(value,min..=max).logarithmic(log).suffix(suffix)).labelled_by(name.id)
  .on_hover_text(crate::i18n::tr_key_or(ui,hint_key,hint));ui.end_row();
}
pub(super) fn editor(ui:&mut egui::Ui,filter:&mut AudioFilter){
 match &mut *filter{
  AudioFilter::CraftSignalGenerator{wave,frequency_hz,level_db,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Generator.Description","Replaces source audio with a test signal. Lower the level before enabling."));
   egui::Grid::new("craft-generator-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Generator.Wave","Signal"));
    let names=[("SkyLive.Craft.Generator.Sine","Sine"),("SkyLive.Craft.Generator.Square","Square"),("SkyLive.Craft.Generator.Saw","Saw"),("SkyLive.Craft.Generator.Triangle","Triangle"),("SkyLive.Craft.Generator.White","White noise"),("SkyLive.Craft.Generator.Pink","Pink noise")];
    let selected=names[usize::from((*wave).min(5))];
    egui::ComboBox::from_id_salt("craft-generator-wave").selected_text(crate::i18n::tr_key_or(ui,selected.0,selected.1)).show_ui(ui,|ui|{
     for (index,(key,name)) in names.iter().enumerate(){let label=crate::i18n::tr_key_or(ui,key,name);ui.selectable_value(wave,index as u8,label);}
    });ui.end_row();
    lofi_slider(ui,"SkyLive.Craft.Generator.Frequency","Frequency","SkyLive.Craft.Generator.FrequencyHint","Tone frequency; noise signals do not use this setting.",frequency_hz,20.,20000.,true," Hz");
    lofi_slider(ui,"SkyLive.Craft.Generator.Level","Level","SkyLive.Craft.Generator.LevelHint","Output level in dBFS; -96 dB silences the generator.",level_db,-96.,0.,false," dBFS");
   });
  },
  AudioFilter::CraftTimeShift{delay_ms,delay_samples,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TimeShift.Description","Delay audio with sample precision; changing the delay crossfades between taps."));
   egui::Grid::new("craft-time-shift-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.TimeShift.Delay","Delay","SkyLive.Craft.TimeShift.DelayHint","Set the delay in milliseconds.",delay_ms,0.,1000.,false," ms");
    let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TimeShift.Samples","Additional samples"));
    ui.add(egui::DragValue::new(delay_samples).range(0..=10000)).labelled_by(label.id)
     .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.TimeShift.SamplesHint","Add an exact number of samples to the delay."));ui.end_row();
   });
  },
  AudioFilter::CraftPitch{semitones,cents,mix_pct,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Pitch.Description","Shift pitch with SoundCraft's two-head delay. Adds about 25 ms latency."));
   egui::Grid::new("craft-pitch-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.Pitch.Semitones","Semitones","SkyLive.Craft.Pitch.SemitonesHint","Set the pitch shift in semitones.",semitones,-24.,24.,false," st");
    lofi_slider(ui,"SkyLive.Craft.Pitch.Cents","Fine","SkyLive.Craft.Pitch.CentsHint","Fine tune the pitch shift in cents.",cents,-100.,100.,false," ct");
    lofi_slider(ui,"SkyLive.Craft.Pitch.Mix","Mix","SkyLive.Craft.Pitch.MixHint","Blend shifted and delayed original audio.",mix_pct,0.,100.,false," %");
   });
  },
  AudioFilter::CraftRingMod{frequency_hz,mix_pct,mode,resonance_pct,mod_source,mod_rate_hz,mod_depth_pct,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.horizontal(|ui|{
    let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Ring.Mode","Effect"));
    let modes=[("SkyLive.Craft.Ring.ModeRing","Ring"),("SkyLive.Craft.Ring.ModeFm","Frequency Mod"),("SkyLive.Craft.Ring.ModeResPlus","Resonator +"),("SkyLive.Craft.Ring.ModeResMinus","Resonator -")];
    let (key,name)=modes[usize::from(*mode).min(3)];
    egui::ComboBox::from_id_salt("craft-ring-mode").selected_text(crate::i18n::tr_key_or(ui,key,name)).show_ui(ui,|ui|{
     for (i,(key,name)) in modes.iter().enumerate(){ui.selectable_value(mode,i as u8,crate::i18n::tr_key_or(ui,key,name));}
    }).response.labelled_by(label.id);
   });
   egui::Grid::new("craft-ring-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.Ring.Frequency","Frequency","SkyLive.Craft.Ring.FrequencyHint","Set the sine carrier frequency.",frequency_hz,20.,5000.,true," Hz");
    if *mode>=2 {lofi_slider(ui,"SkyLive.Craft.Ring.Resonance","Resonance","SkyLive.Craft.Ring.ResonanceHint","Set feedback of the resonator.",resonance_pct,0.,95.,false," %");}
    let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Ring.ModSource","Modulation source"));
    let sources=[("SkyLive.Craft.Ring.SourceLfo","LFO"),("SkyLive.Craft.Ring.SourceEnvelope","Envelope"),("SkyLive.Craft.Ring.SourceHold","Sample & Hold")];
    let (key,name)=sources[usize::from(*mod_source).min(2)];
    egui::ComboBox::from_id_salt("craft-ring-source").selected_text(crate::i18n::tr_key_or(ui,key,name)).show_ui(ui,|ui|{
     for (i,(key,name)) in sources.iter().enumerate(){ui.selectable_value(mod_source,i as u8,crate::i18n::tr_key_or(ui,key,name));}
    }).response.labelled_by(label.id);ui.end_row();
    lofi_slider(ui,"SkyLive.Craft.Ring.ModRate","Modulation rate","SkyLive.Craft.Ring.ModRateHint","Set the rate of the LFO or sample and hold.",mod_rate_hz,0.05,20.,true," Hz");
    lofi_slider(ui,"SkyLive.Craft.Ring.ModDepth","Modulation depth","SkyLive.Craft.Ring.ModDepthHint","Move the frequency by up to two octaves.",mod_depth_pct,0.,100.,false," %");
    lofi_slider(ui,"SkyLive.Craft.Ring.Mix","Mix","SkyLive.Craft.Ring.MixHint","Blend the ring-modulated signal with the original.",mix_pct,0.,100.,false," %");
   });
  },
  AudioFilter::CraftDither{bits,noise_shaping,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.Hint","TPDF dither and requantization with optional first-order noise shaping."));
   ui.horizontal(|ui|{
    let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.Depth","Bit depth"));
    egui::ComboBox::from_id_salt("craft-dither-bits").selected_text(format!("{} bit",bits)).show_ui(ui,|ui|{
     for depth in [16u8,20,24] {ui.selectable_value(bits,depth,format!("{} bit",depth));}
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.DepthHint","Select the target PCM bit depth."));
   });
   ui.checkbox(noise_shaping,crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.Shaping","Noise shaping"))
    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.ShapingHint","Enable first-order error feedback to shape quantization noise."));
  },
  AudioFilter::CraftEq1{kind,frequency_hz,gain_db,q,output_db,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.horizontal(|ui|{
    let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Eq1.Type","Type"));
    let names=[("Peak","Peak"),("LowShelf","Low Shelf"),("HighShelf","High Shelf"),("HighPass","High Pass"),("LowPass","Low Pass"),("Notch","Notch"),("BandPass","Band Pass")];
    let (key,name)=names[usize::from(*kind).min(6)];
    egui::ComboBox::from_id_salt("craft-eq1-kind").selected_text(crate::i18n::tr_key_or(ui,&format!("SkyLive.Craft.Eq1.{key}"),name)).show_ui(ui,|ui|{
     for (i,(key,name)) in names.iter().enumerate(){ui.selectable_value(kind,i as u8,crate::i18n::tr_key_or(ui,&format!("SkyLive.Craft.Eq1.{key}"),name));}
    }).response.labelled_by(label.id);
   });
   egui::Grid::new("craft-eq1-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.Eq1.Frequency","Frequency","SkyLive.Craft.Eq1.FrequencyHint","Set the filter center or cutoff frequency.",frequency_hz,20.,20000.,true," Hz");
    if *kind<=2 {lofi_slider(ui,"SkyLive.Craft.Eq1.Gain","Gain","SkyLive.Craft.Eq1.GainHint","Set the peak or shelf gain.",gain_db,-24.,24.,false," dB");}
    lofi_slider(ui,"SkyLive.Craft.Eq1.Q","Q","SkyLive.Craft.Eq1.QHint","Set the filter bandwidth or resonance.",q,0.1,10.,true,"");
    lofi_slider(ui,"SkyLive.Craft.Eq1.Output","Output","SkyLive.Craft.Eq1.OutputHint","Adjust output gain.",output_db,-24.,24.,false," dB");
   });
  },
  AudioFilter::CraftRectifier{full_wave,mix_pct,output_db,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Rectifier.Hint","SoundCraft half or full-wave rectification with DC removal."));
   ui.checkbox(full_wave,crate::i18n::tr_key_or(ui,"SkyLive.Craft.Rectifier.Full","Full wave"))
    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Rectifier.FullHint","Rectify both halves of the waveform; disable for half-wave."));
   egui::Grid::new("craft-rectifier-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.Rectifier.Mix","Mix","SkyLive.Craft.Rectifier.MixHint","Blend processed audio with the original.",mix_pct,0.,100.,false," %");
    lofi_slider(ui,"SkyLive.Craft.Rectifier.Output","Output","SkyLive.Craft.Rectifier.OutputHint","Adjust output gain after mixing.",output_db,-24.,12.,false," dB");
   });
  },
  AudioFilter::CraftMaximizer{threshold_db,ceiling_db,release_ms,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Maximizer.Description","Increase loudness with linked lookahead limiting and a configurable ceiling."));
   egui::Grid::new("craft-maximizer-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.Maximizer.Threshold","Threshold","SkyLive.Craft.Maximizer.ThresholdHint","Set the input drive threshold.",threshold_db,-30.,0.,false," dB");
    lofi_slider(ui,"SkyLive.Craft.Maximizer.Ceiling","Ceiling","SkyLive.Craft.Maximizer.CeilingHint","Set the maximum output level.",ceiling_db,-30.,0.,false," dB");
    lofi_slider(ui,"SkyLive.Craft.Maximizer.Release","Release","SkyLive.Craft.Maximizer.ReleaseHint","Set how quickly gain recovers.",release_ms,1.,1000.,true," ms");
   });
  },
  AudioFilter::CraftDeEsser{frequency_hz,range_db,threshold_db,listen,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.DeEsser.Description","Reduce sibilance with a high-frequency detector and dynamic shelf."));
   egui::Grid::new("craft-de-esser-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.DeEsser.Frequency","Frequency","SkyLive.Craft.DeEsser.FrequencyHint","Choose the frequency of the sibilance detector.",frequency_hz,2000.,16000.,true," Hz");
    lofi_slider(ui,"SkyLive.Craft.DeEsser.Range","Range","SkyLive.Craft.DeEsser.RangeHint","Set the maximum reduction in high-frequency level.",range_db,0.,40.,false," dB");
    lofi_slider(ui,"SkyLive.Craft.DeEsser.Threshold","Threshold","SkyLive.Craft.DeEsser.ThresholdHint","Set the detector threshold.",threshold_db,-60.,0.,false," dB");
   });
   ui.checkbox(listen,crate::i18n::tr_key_or(ui,"SkyLive.Craft.DeEsser.Listen","Listen"))
    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.DeEsser.ListenHint","Monitor only the detected high-frequency signal."));
  },
  AudioFilter::CraftDcRemoval{cutoff_hz,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dc.Description","Remove DC offset with a low-frequency high-pass filter."));
   egui::Grid::new("craft-dc-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.Dc.Cutoff","Cutoff","SkyLive.Craft.Dc.CutoffHint","Set the low-frequency cutoff for DC offset removal.",cutoff_hz,1.,40.,true," Hz");
   });
  },
  AudioFilter::CraftLoFi{bits,sample_rate,noise_pct,mix_pct,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.LoFi.Description","SoundCraft Lo-Fi reduces bit depth and sample rate, with optional noise."));
   egui::Grid::new("craft-lofi-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    lofi_slider(ui,"SkyLive.Craft.LoFi.Bits","Bit depth","SkyLive.Craft.LoFi.BitsHint","Lower values increase quantization distortion.",bits,1.,24.,false," bits");
    lofi_slider(ui,"SkyLive.Craft.LoFi.Rate","Sample rate","SkyLive.Craft.LoFi.RateHint","Lower rates hold samples longer for a decimated sound.",sample_rate,500.,48000.,true," Hz");
    lofi_slider(ui,"SkyLive.Craft.LoFi.Noise","Noise","SkyLive.Craft.LoFi.NoiseHint","Add noise before sample reduction.",noise_pct,0.,100.,false," %");
    lofi_slider(ui,"SkyLive.Craft.LoFi.Mix","Mix","SkyLive.Craft.LoFi.MixHint","Blend the processed sound with the original.",mix_pct,0.,100.,false," %");
   });
  },
  AudioFilter::CraftSaturator{drive_db,bias_pct,tone_hz,mix_pct,output_db,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Saturator.Description","SoundCraft Saturator adds harmonics with asymmetry and tone controls."));
   egui::Grid::new("craft-saturator-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    sat_slider(ui,"SkyLive.Craft.Saturator.Drive","Drive",drive_db,0.,36.,false," dB");
    sat_slider(ui,"SkyLive.Craft.Saturator.Bias","Asymmetry",bias_pct,0.,100.,false," %");
    sat_slider(ui,"SkyLive.Craft.Saturator.Tone","Tone",tone_hz,1000.,20000.,true," Hz");
    sat_slider(ui,"SkyLive.Craft.Saturator.Mix","Mix",mix_pct,0.,100.,false," %");
    sat_slider(ui,"SkyLive.Craft.Saturator.Output","Output",output_db,-24.,12.,false," dB");
   });
  },
  AudioFilter::CraftModDelay{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz,mode,stereo_spread,phaser_stages,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this Craft effect. Re-enabling starts a fresh delay/reverb state; its authored settings are retained."));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Modulation.Description","SoundCraft modulation: Mod Delay, Chorus, Flanger and Phaser. Changing mode resets the effect history."));
   let old_mode=*mode;
   let modes=[("SkyLive.Craft.Modulation.Delay","Mod Delay"),("SkyLive.Craft.Modulation.Chorus","Chorus"),("SkyLive.Craft.Modulation.Flanger","Flanger"),("SkyLive.Craft.Modulation.Phaser","Phaser")];
   let (selected_key,selected_name)=modes[usize::from(*mode).min(3)];
   egui::ComboBox::from_id_salt("craft-modulation-mode").selected_text(crate::i18n::tr_key_or(ui,selected_key,selected_name)).show_ui(ui,|ui|{
    for (index,(key,name)) in modes.iter().enumerate(){ui.selectable_value(mode,index as u8,crate::i18n::tr_key_or(ui,key,name));}
   });
   if old_mode!=*mode {match *mode {1=>{*time_ms=12.;*feedback_pct=0.;*mix_pct=50.;*mod_rate_hz=0.8;*mod_depth_pct=50.;*stereo_spread=100.;*lowpass_hz=12000.;},2=>{*time_ms=2.;*feedback_pct=50.;*mix_pct=50.;*mod_rate_hz=0.25;*mod_depth_pct=70.;*lowpass_hz=12000.;},3=>{*time_ms=1.;*feedback_pct=40.;*mix_pct=50.;*mod_rate_hz=0.5;*mod_depth_pct=60.;*lowpass_hz=800.;},_=>{*time_ms=250.;*feedback_pct=30.;*mix_pct=30.;*mod_rate_hz=0.5;*mod_depth_pct=0.;*lowpass_hz=12000.;}}}
   egui::Grid::new("craft-mod-delay-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    if *mode!=3 {slider(ui,"Delay Time (ms)",time_ms,if *mode==2{0.1}else{1.},if *mode==1{30.}else if *mode==2{10.}else{2000.},true," ms","Delay time glides over 120 ms when edited, preserving existing echoes.");}
    else {let label=ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Phaser.Stages","Stages"));egui::ComboBox::from_id_salt("craft-phaser-stages").selected_text(phaser_stages.to_string()).show_ui(ui,|ui|{for count in [2u8,4,6,8,12]{ui.selectable_value(phaser_stages,count,count.to_string());}}).response.labelled_by(label.id).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Craft.Phaser.StagesHint","Number of all-pass stages; more stages produce a denser phaser effect."));ui.end_row();}
    slider(ui,"Feedback (%)",feedback_pct,-99.,99.,false," %","Negative feedback inverts repeated echoes. Values near 99% produce long tails.");
    slider(ui,"Mix (%)",mix_pct,0.,100.,false," %","Dry/wet mix: 0% keeps the dry signal; 100% outputs only the effect.");
    if *mode==3{lofi_slider(ui,"SkyLive.Craft.Phaser.Rate","Mod Rate (Hz)","SkyLive.Craft.Phaser.RateHint","Rate of the phaser sweep.",mod_rate_hz,0.01,10.,true," Hz");}else{slider(ui,"Mod Rate (Hz)",mod_rate_hz,0.01,10.,true," Hz","Rate of the sine delay modulation. Odd channels have a quarter-cycle phase offset.");}
    if *mode==0{slider(ui,"Mod Depth (%)",mod_depth_pct,0.,100.,false," %","Modulation depth: 100% moves the delay by up to 5 ms.");}else if *mode==3{lofi_slider(ui,"SkyLive.Craft.Phaser.Depth","Mod Depth (%)","SkyLive.Craft.Phaser.DepthHint","Sweep depth in octaves around the center frequency.",mod_depth_pct,0.,100.,false," %");}else{lofi_slider(ui,"SkyLive.Craft.Modulation.Depth","Mod Depth (%)","SkyLive.Craft.Modulation.DepthHint","Modulation depth controls the delay sweep.",mod_depth_pct,0.,100.,false," %");}
    if *mode==0{slider(ui,"Feedback LPF (Hz)",lowpass_hz,500.,20000.,true," Hz","Low-pass filter in the feedback path darkens repeated echoes.");}else if *mode==3 {lofi_slider(ui,"SkyLive.Craft.Phaser.Center","Center (Hz)","SkyLive.Craft.Phaser.CenterAnyHint","Center frequency of the phaser sweep.",lowpass_hz,100.,4000.,true," Hz");}
    if *mode==1{lofi_slider(ui,"SkyLive.Craft.Modulation.Spread","Stereo Spread (%)","SkyLive.Craft.Modulation.SpreadHint","Adjust the modulation phase offset between stereo channels.",stereo_spread,0.,100.,false," %");}
   });
  },
  AudioFilter::CraftSoundReverb{plate,predelay_ms,decay_seconds,size_pct,damping_hz,diffusion_pct,width_pct,lowcut_hz,mix_pct,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"));
   ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Craft.SoundReverb.Description","SoundCraft room and plate reverb. Changing mode clears the current tail."));
   egui::ComboBox::from_id_salt("sound-reverb-mode").selected_text(if *plate {crate::i18n::tr_key_or(ui,"SkyLive.Craft.SoundReverb.Plate","Plate")} else {crate::i18n::tr_key_or(ui,"SkyLive.Craft.SoundReverb.Room","Room")}).show_ui(ui,|ui|{
    ui.selectable_value(plate,false,crate::i18n::tr_key_or(ui,"SkyLive.Craft.SoundReverb.Room","Room"));
    ui.selectable_value(plate,true,crate::i18n::tr_key_or(ui,"SkyLive.Craft.SoundReverb.Plate","Plate"));
   });
   egui::Grid::new("sound-reverb-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    slider(ui,"Pre-delay (ms)",predelay_ms,0.,250.,false," ms","Time before the reverb starts.");
    slider(ui,"Decay RT60 (s)",decay_seconds,0.1,20.,true," s","Time for the reverb to decay by 60 dB.");
    slider(ui,"Room Size (%)",size_pct,0.,100.,false," %","Set the size of the reverb space.");
    lofi_slider(ui,"SkyLive.Craft.SoundReverb.Damping","Damping","SkyLive.Craft.SoundReverb.DampingHint","Set the high-frequency damping cutoff.",damping_hz,1000.,20000.,true," Hz");
    lofi_slider(ui,"SkyLive.Craft.SoundReverb.Diffusion","Diffusion","SkyLive.Craft.SoundReverb.DiffusionHint","Set the density of early reflections.",diffusion_pct,0.,100.,false," %");
    lofi_slider(ui,"SkyLive.Craft.SoundReverb.Width","Stereo Width","SkyLive.Craft.SoundReverb.WidthHint","Adjust the width of the reverberant signal.",width_pct,0.,100.,false," %");
    lofi_slider(ui,"SkyLive.Craft.SoundReverb.Lowcut","Low Cut","SkyLive.Craft.SoundReverb.LowcutHint","Remove low frequencies from the reverb output.",lowcut_hz,20.,1000.,true," Hz");
    slider(ui,"Mix (%)",mix_pct,0.,100.,false," %","Blend the processed sound with the original.");
   });
  },
  AudioFilter::CraftFdnReverb{predelay_ms,decay_seconds,damping_pct,size_pct,mix_pct,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this Craft effect. Re-enabling starts a fresh delay/reverb state; its authored settings are retained."));
   ui.label(crate::i18n::tr(ui,"FilmCraft FDN Reverb: eight feedback lines with a Householder matrix, pre-delay and high-frequency damping."));
   egui::Grid::new("craft-fdn-reverb-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    slider(ui,"Pre-delay (ms)",predelay_ms,0.,200.,false," ms","Time before the reverberant field. The original algorithm keeps at least one sample of pre-delay.");
    slider(ui,"Decay RT60 (s)",decay_seconds,0.1,20.,true," s","Nominal time for the reverb feedback field to decay by 60 dB.");
    slider(ui,"HF Damping (%)",damping_pct,0.,100.,false," %","Higher damping makes high frequencies decay faster in the feedback loops.");
    slider(ui,"Room Size (%)",size_pct,0.,100.,false," %","Changes the eight delay lengths with the donor's smooth room-size mapping.");
    slider(ui,"Mix (%)",mix_pct,0.,100.,false," %","Dry/wet mix: 0% keeps the dry signal; 100% outputs only the effect.");
   });
  },_=>{}
 }
 if !matches!(filter,AudioFilter::CraftSignalGenerator{..}|AudioFilter::CraftDeEsser{..}|AudioFilter::CraftDither{..}|AudioFilter::CraftRectifier{..}|AudioFilter::CraftSaturator{..}|AudioFilter::CraftLoFi{..}|AudioFilter::CraftDcRemoval{..}) {ui.label(crate::i18n::tr(ui,"Live parameter edits retain effect history and smooth to new values. Tails process while real source PCM, including silence, continues; source EOF does not synthesize extra audio."));}
}
