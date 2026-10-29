//! Exact OBS32 compressor_filter subset. Keys resolve by names only at the
//! interchange boundary; native authored settings retain weak stable SourceIDs.
use super::*;
use skylive_core::{AudioFilter,MAX_AUDIO_FILTERS};
pub(super) struct Input{filter:AudioFilter,key:Option<String>}
pub(super) fn parse(map:&Map<String,Value>,path:&str,uuids:&mut HashSet<String>)->Result<Vec<Input>,ObsError>{
 let Some(value)=map.get("filters")else{return Ok(Vec::new());};
 let list=array(value,&format!("{path}.filters"))?;let mut result=Vec::new();let mut names=HashSet::new();
 for (index,value) in list.iter().enumerate(){
  let path=format!("{path}.filters[{index}]");let map=object(value,&path)?;
  let name=string(map,"name",&path)?;if !names.insert(name){return Err(issue(&path,"identity_conflict","duplicate sibling filter name"));}
  let provider=map.get("id").and_then(Value::as_str);
  if !matches!(provider,Some("compressor_filter"|"noise_suppress_filter"|"gain_filter"|"noise_gate_filter"|"limiter_filter"|"expander_filter"|"invert_polarity_filter")){continue;}
  let noise=provider==Some("noise_suppress_filter");
  known(map,&["prev_ver","name","uuid","id","versioned_id","settings","mixers","sync","flags","volume","balance","enabled","muted","push-to-mute","push-to-mute-delay","push-to-talk","push-to-talk-delay","hotkeys","deinterlace_mode","deinterlace_field_order","monitoring_type","monitoring_enabled","private_settings","filters"],&path)?;
  if noise{
   if map.get("versioned_id").and_then(Value::as_str)!=Some("noise_suppress_filter_v2"){return Err(issue(&path,"unsupported_filter","RNNoise requires the explicit OBS v2 filter; legacy Speex defaults are not substituted"));}
  }else if map.get("versioned_id").is_some_and(|id|id.as_str()!=provider){return Err(issue(&path,"unsupported_filter","unsupported audio filter version"));}
  for key in ["sync","flags","push-to-mute-delay","push-to-talk-delay","deinterlace_mode","deinterlace_field_order","monitoring_type"]{neutral(map,key,json!(0),&path)?;}
  for key in ["muted","push-to-mute","push-to-talk","monitoring_enabled"]{neutral(map,key,json!(false),&path)?;}
  for key in ["hotkeys","private_settings"]{neutral(map,key,json!({}),&path)?;}
  neutral(map,"filters",json!([]),&path)?;audio_mixers::neutral(map,&path)?;neutral(map,"volume",json!(1.0),&path)?;neutral(map,"balance",json!(0.5),&path)?;
  if let Some(value)=map.get("uuid"){let uuid=value.as_str().ok_or_else(||issue(&path,"invalid","compressor UUID must be text"))?;
   if !uuid_valid(uuid)||!uuids.insert(uuid.to_owned()){return Err(issue(&path,"identity_conflict","invalid or duplicate compressor UUID"));}
  }
  if map.get("prev_ver").is_some_and(|v|v.as_u64().is_none_or(|v|v>u64::from(u32::MAX))){return Err(issue(&path,"invalid","invalid compressor prev_ver"));}
  let settings=object(required(map,"settings",&path)?,&path)?;
  if noise{
   known(settings,&["method","suppress_level"],&path)?;
   if settings.get("method").is_some_and(|value|value.as_str()!=Some("rnnoise")){return Err(issue(&path,"unsupported_filter","only actual RNNoise is implemented; Speex is unavailable"));}
   // The inactive Speex slider is not modeled. Preserve strict representability
   // by accepting only its canonical v2 default rather than silently dropping it.
   neutral(settings,"suppress_level",json!(-30),&path)?;
   let enabled=map.get("enabled").map(|value|value.as_bool().ok_or_else(||issue(&path,"invalid","noise filter enabled must be boolean"))).transpose()?.unwrap_or(true);
   if result.len()==MAX_AUDIO_FILTERS{return Err(issue(&path,"limit","at most eight source audio filters"));}
   result.push(Input{filter:AudioFilter::ObsNoiseSuppression{enabled},key:None});continue;
  }
  if provider!=Some("compressor_filter"){
   use skylive_core::{ObsExpanderDetector as Detector,ObsExpanderPreset as Preset};
   let number=|key:&str,default:f32|->Result<f32,ObsError>{match settings.get(key){None=>Ok(default),Some(v)=>v.as_f64().filter(|v|v.is_finite()&&(*v as f32).is_finite()).map(|v|v as f32).ok_or_else(||issue(&path,"invalid",format!("invalid audio filter {key}")))}};
   let millis=|key:&str,default:u32|->Result<u32,ObsError>{match settings.get(key){None=>Ok(default),Some(v)=>v.as_u64().and_then(|v|u32::try_from(v).ok()).ok_or_else(||issue(&path,"invalid",format!("invalid audio filter {key}")))}};
   let enabled=boolean(map,"enabled",true,&path)?;
   let filter=match provider {
    Some("gain_filter")=>{known(settings,&["db"],&path)?;AudioFilter::ObsGain{db:number("db",0.)?,enabled}},
    Some("invert_polarity_filter")=>{known(settings,&[],&path)?;AudioFilter::ObsPolarityInvert{enabled}},
    Some("noise_gate_filter")=>{known(settings,&["open_threshold","close_threshold","attack_time","hold_time","release_time"],&path)?;AudioFilter::ObsNoiseGate{open_threshold_db:number("open_threshold",-26.)?,close_threshold_db:number("close_threshold",-32.)?,attack_ms:millis("attack_time",25)?,hold_ms:millis("hold_time",200)?,release_ms:millis("release_time",150)?,enabled}},
    Some("limiter_filter")=>{known(settings,&["threshold","release_time"],&path)?;AudioFilter::ObsLimiter{threshold_db:number("threshold",-6.)?,release_ms:millis("release_time",60)?,enabled}},
    Some("expander_filter")=>{
     known(settings,&["ratio","threshold","attack_time","release_time","output_gain","detector","presets"],&path)?;
     let preset=match settings.get("presets").map(Value::as_str).unwrap_or(Some("expander")){Some("expander")=>Preset::Expander,Some("gate")=>Preset::Gate,_=>return Err(issue(&path,"unsupported_filter","unknown expander preset"))};
     let detector=match settings.get("detector").map(Value::as_str).unwrap_or(Some("RMS")){Some("RMS")=>Detector::Rms,Some("peak")=>Detector::Peak,_=>return Err(issue(&path,"unsupported_filter","unknown expander detector"))};
     AudioFilter::ObsExpander{threshold_db:number("threshold",-40.)?,ratio:number("ratio",if preset==Preset::Gate{10.}else{2.})?,attack_ms:millis("attack_time",10)?,release_ms:millis("release_time",if preset==Preset::Gate{125}else{50})?,output_db:number("output_gain",0.)?,detector,preset,enabled}
    },_=>return Err(issue(&path,"unsupported_filter","unsupported audio filter")),
   };
   filter.validate().map_err(|error|issue(&path,"invalid",error.to_string()))?;
   if result.len()==MAX_AUDIO_FILTERS{return Err(issue(&path,"limit","at most eight source audio filters"));}
   result.push(Input{filter,key:None});continue;
  }
  known(settings,&["ratio","threshold","attack_time","release_time","output_gain","sidechain_source"],&path)?;
  let number=|key:&str,default:f32|->Result<f32,ObsError>{match settings.get(key){None=>Ok(default),Some(v)=>v.as_f64().filter(|v|v.is_finite()&&(*v as f32).is_finite()).map(|v|v as f32).ok_or_else(||issue(&path,"invalid",format!("invalid compressor {key}")))}};
  let millis=|key:&str,default:u32|->Result<u32,ObsError>{match settings.get(key){None=>Ok(default),Some(v)=>v.as_u64().and_then(|v|u32::try_from(v).ok()).ok_or_else(||issue(&path,"invalid",format!("invalid compressor {key}")))}};
  let key=match settings.get("sidechain_source"){None=>None,Some(v)=>{let name=v.as_str().ok_or_else(||issue(&path,"invalid","compressor sidechain_source must be text"))?;(name!="none").then(||name.to_owned())}};
  let enabled=map.get("enabled").map(|v|v.as_bool().ok_or_else(||issue(&path,"invalid","compressor enabled must be boolean"))).transpose()?.unwrap_or(true);
  let filter=AudioFilter::ObsCompressor{threshold_db:number("threshold",-18.)?,ratio:number("ratio",10.)?,attack_ms:millis("attack_time",6)?,release_ms:millis("release_time",60)?,output_db:number("output_gain",0.)?,sidechain:None,enabled};
  filter.validate().map_err(|error|issue(&path,"invalid",error.to_string()))?;
  if result.len()==MAX_AUDIO_FILTERS{return Err(issue(&path,"limit","at most eight source audio filters"));}
  result.push(Input{filter,key});
 }
 Ok(result)
}
pub(super) fn resolve(input:&[Input],names:&HashMap<String,usize>,mapping:&HashMap<usize,Id>)->Result<Vec<AudioFilter>,ObsError>{
 input.iter().map(|input|{
  let mut filter=input.filter;
  if let AudioFilter::ObsCompressor{sidechain,..}=&mut filter{
   *sidechain=input.key.as_ref().map(|name|names.get(name).and_then(|index|mapping.get(index)).copied()
    .ok_or_else(||issue("$.sources.filters.settings.sidechain_source","unsupported","sidechain key must resolve to a retained supported audio source; missing/unreferenced OBS names cannot be silently changed to self"))).transpose()?;
  }
  Ok(filter)
 }).collect()
}
pub(super) fn append(output:&mut Value,filters:&[AudioFilter],model:&StudioSnapshot)->Result<(),ObsError>{
 let output=output.as_array_mut().ok_or_else(||issue("$.sources.filters","invalid","filter export array"))?;
 for filter in filters{
  use skylive_core::{ObsExpanderDetector as Detector,ObsExpanderPreset as Preset};
  let simple=match *filter{
   AudioFilter::ObsGain{db,enabled}=>Some(("Gain","gain_filter",enabled,json!({"db":db}))),
   AudioFilter::ObsPolarityInvert{enabled}=>Some(("Invert Polarity","invert_polarity_filter",enabled,json!({}))),
   AudioFilter::ObsNoiseGate{open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms,enabled}=>Some(("Noise Gate","noise_gate_filter",enabled,json!({"open_threshold":open_threshold_db,"close_threshold":close_threshold_db,"attack_time":attack_ms,"hold_time":hold_ms,"release_time":release_ms}))),
   AudioFilter::ObsLimiter{threshold_db,release_ms,enabled}=>Some(("Limiter","limiter_filter",enabled,json!({"threshold":threshold_db,"release_time":release_ms}))),
   AudioFilter::ObsExpander{threshold_db,ratio,attack_ms,release_ms,output_db,detector,preset,enabled}=>Some(("Expander","expander_filter",enabled,json!({"threshold":threshold_db,"ratio":ratio,"attack_time":attack_ms,"release_time":release_ms,"output_gain":output_db,"detector":match detector{Detector::Rms=>"RMS",Detector::Peak=>"peak"},"presets":match preset{Preset::Expander=>"expander",Preset::Gate=>"gate"}}))),
   _=>None,
  };
  if let Some((base,id,enabled,settings))=simple{
   let mut chosen=None;
   for suffix in 1..=output.len()+1{let name=if suffix==1{base.to_owned()}else{format!("{base} {suffix}")};if output.iter().all(|value|value.get("name").and_then(Value::as_str)!=Some(name.as_str())){chosen=Some(name);break;}}
   output.push(json!({"name":chosen.ok_or_else(||issue("$.sources.filters","limit","audio filter export name allocation"))?,"id":id,"versioned_id":id,"enabled":enabled,"settings":settings}));continue;
  }
  if let AudioFilter::ObsNoiseSuppression{enabled}=*filter{
   let mut chosen=None;
   for suffix in 1..=output.len()+1{
    let name=if suffix==1{"Noise Suppression".to_owned()}else{format!("Noise Suppression {suffix}")};
    if output.iter().all(|value|value.get("name").and_then(Value::as_str)!=Some(name.as_str())){chosen=Some(name);break;}
   }
   output.push(json!({"name":chosen.ok_or_else(||issue("$.sources.filters","limit","noise filter export name allocation"))?,
     "id":"noise_suppress_filter","versioned_id":"noise_suppress_filter_v2","enabled":enabled,
     "settings":{"method":"rnnoise","suppress_level":-30}}));continue;
  }

  let AudioFilter::ObsCompressor{threshold_db,ratio,attack_ms,release_ms,output_db,sidechain,enabled}=*filter else{
   return Err(issue("$.sources.filters","unsupported","this audio filter is outside the represented OBS interchange subset"));
  };
  let key=match sidechain{None=>"none",Some(id)=>model.source(id).ok_or_else(||issue("$.sources.filters.sidechain_source","unsupported","orphan sidechain ID has no representable OBS source name"))?.name()};
  if sidechain.is_some()&&key=="none"{return Err(issue("$.sources.filters.sidechain_source","unsupported","OBS reserves the sidechain name none"));}
  let mut chosen=None;
  for suffix in 1..=output.len()+1{
   let name=if suffix==1{"Compressor".to_owned()}else{format!("Compressor {suffix}")};
   if output.iter().all(|value|value.get("name").and_then(Value::as_str)!=Some(name.as_str())){chosen=Some(name);break;}
  }
  output.push(json!({"name":chosen.ok_or_else(||issue("$.sources.filters","limit","compressor export name allocation"))?,"id":"compressor_filter","versioned_id":"compressor_filter","enabled":enabled,
   "settings":{"threshold":threshold_db,"ratio":ratio,"attack_time":attack_ms,"release_time":release_ms,"output_gain":output_db,"sidechain_source":key}}));
 }
 Ok(())
}
