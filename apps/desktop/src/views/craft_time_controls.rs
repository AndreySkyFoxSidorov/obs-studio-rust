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
  AudioFilter::CraftModDelay{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz,enabled}=>{
   ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this Craft effect. Re-enabling starts a fresh delay/reverb state; its authored settings are retained."));
   ui.label(crate::i18n::tr(ui,"SoundCraft Mod Delay: cubic modulated echo with a low-pass feedback path and stereo phase spread."));
   egui::Grid::new("craft-mod-delay-controls").num_columns(2).spacing([12.,6.]).show(ui,|ui|{
    slider(ui,"Delay Time (ms)",time_ms,1.,2000.,true," ms","Delay time glides over 120 ms when edited, preserving existing echoes.");
    slider(ui,"Feedback (%)",feedback_pct,-99.,99.,false," %","Negative feedback inverts repeated echoes. Values near 99% produce long tails.");
    slider(ui,"Mix (%)",mix_pct,0.,100.,false," %","Dry/wet mix: 0% keeps the dry signal; 100% outputs only the effect.");
    slider(ui,"Mod Rate (Hz)",mod_rate_hz,0.01,10.,true," Hz","Rate of the sine delay modulation. Odd channels have a quarter-cycle phase offset.");
    slider(ui,"Mod Depth (%)",mod_depth_pct,0.,100.,false," %","Modulation depth: 100% moves the delay by up to 5 ms.");
    slider(ui,"Feedback LPF (Hz)",lowpass_hz,500.,20000.,true," Hz","Low-pass filter in the feedback path darkens repeated echoes.");
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
 if !matches!(filter,AudioFilter::CraftDither{..}|AudioFilter::CraftRectifier{..}|AudioFilter::CraftSaturator{..}|AudioFilter::CraftLoFi{..}|AudioFilter::CraftDcRemoval{..}) {ui.label(crate::i18n::tr(ui,"Live parameter edits retain effect history and smooth to new values. Tails process while real source PCM, including silence, continues; source EOF does not synthesize extra audio."));}
}
