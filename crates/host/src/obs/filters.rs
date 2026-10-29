//! Strict OBS Color Correction v2 source objects. Raw provenance is keyed only
//! by an engine-issued filter identity, never by position/effect equality.
use super::*;
#[path="scale_filter_labels.rs"] mod scale_filter_labels;
use skylive_core::{MAX_VIDEO_FILTERS, VideoEffect, VideoFilter, VideoFilterId};

pub(super) struct InputFilter {
    pub filter: VideoFilter,
    pub raw: Map<String, Value>,
    pub mask_path:Option<std::path::PathBuf>,
    pub lut_path:Option<std::path::PathBuf>,
}

pub(super) fn parse(
    map: &Map<String, Value>,
    path: &str,
    _is_scene: bool,
    uuids: &mut HashSet<String>,
    base_directory:Option<&std::path::Path>,
) -> Result<Vec<InputFilter>, ObsError> {
    let Some(value) = map.get("filters") else {
        return Ok(Vec::new());
    };
    let list = array(value, &format!("{path}.filters"))?;
    if list.len() > MAX_VIDEO_FILTERS + skylive_core::MAX_AUDIO_FILTERS {
        return Err(issue(
            &format!("{path}.filters"),
            "limit",
            "at most 32 video and eight audio filters per source",
        ));
    }
    let mut names = HashSet::new();
    let mut output = Vec::with_capacity(list.len());
    for (index, value) in list.iter().enumerate() {
        let path = format!("{path}.filters[{index}]");
        let map = object(value, &path)?;
        if matches!(map.get("id").and_then(Value::as_str),Some("compressor_filter"|"noise_suppress_filter"|"gain_filter"|"noise_gate_filter"|"limiter_filter"|"expander_filter"|"invert_polarity_filter")){continue;}
        if output.len()==MAX_VIDEO_FILTERS{return Err(issue(&path,"limit","at most 32 video filters per source"));}
        known(
            map,
            &[
                "prev_ver",
                "name",
                "uuid",
                "id",
                "versioned_id",
                "settings",
                "mixers",
                "sync",
                "flags",
                "volume",
                "balance",
                "enabled",
                "muted",
                "push-to-mute",
                "push-to-mute-delay",
                "push-to-talk",
                "push-to-talk-delay",
                "hotkeys",
                "deinterlace_mode",
                "deinterlace_field_order",
                "monitoring_type",
                "monitoring_enabled",
                "private_settings",
                "filters",
            ],
            &path,
        )?;
        let provider = string(map, "id", &path)?;
        let versioned = map.get("versioned_id").and_then(Value::as_str);
        if matches!(provider,"freeze_filter"|"gpu_delay"|"clut_filter"|"scroll_filter"|"scale_filter"|"luma_key_filter")&&map.contains_key("versioned_id")&&versioned.is_none(){return Err(issue(&path,"invalid","Filter versioned_id must be text when present"));}
        if !matches!(
            (provider, versioned),
            ("crop_filter", None | Some("crop_filter"))
                | ("color_filter", Some("color_filter_v2"))
                | ("color_key_filter", Some("color_key_filter_v2"))
                | ("chroma_key_filter", Some("chroma_key_filter_v2"))
                | ("luma_key_filter", None | Some("luma_key_filter") | Some("luma_key_filter_v2"))
                | ("sharpness_filter", Some("sharpness_filter_v2"))
                | ("mask_filter",Some("mask_filter_v2"))
                | ("clut_filter",None|Some("clut_filter"))
                | ("scroll_filter",None|Some("scroll_filter"))
                | ("scale_filter",None|Some("scale_filter"))
                | ("gpu_delay",None|Some("gpu_delay"))
                | ("freeze_filter",None|Some("freeze_filter"))
        ) {
            return Err(issue(
                &path,
                "unsupported_filter",
                "only explicit supported OBS providers are accepted; legacy v1 is not v2",
            ));
        }
        for key in [
            "sync",
            "flags",
            "push-to-mute-delay",
            "push-to-talk-delay",
            "deinterlace_mode",
            "deinterlace_field_order",
            "monitoring_type",
        ] {
            neutral(map, key, json!(0), &path)?;
        }
        for key in [
            "muted",
            "push-to-mute",
            "push-to-talk",
            "monitoring_enabled",
        ] {
            neutral(map, key, json!(false), &path)?;
        }
        for key in ["hotkeys", "private_settings"] {
            neutral(map, key, json!({}), &path)?;
        }
        neutral(map, "filters", json!([]), &path)?;
        audio_mixers::neutral(map,&path)?;
        neutral(map, "volume", json!(1.0), &path)?;
        neutral(map, "balance", json!(0.5), &path)?;
        if map
            .get("prev_ver")
            .is_some_and(|v| v.as_u64().is_none_or(|v| v > u64::from(u32::MAX)))
        {
            return Err(issue(
                &format!("{path}.prev_ver"),
                "invalid",
                "expected a uint32 version",
            ));
        }
        let name = string(map, "name", &path)?;
        if !names.insert(name.to_owned()) {
            return Err(issue(
                &format!("{path}.name"),
                "identity_conflict",
                "duplicate sibling filter name",
            ));
        }
        if let Some(value) = map.get("uuid") {
            let uuid = value.as_str().ok_or_else(|| {
                issue(&format!("{path}.uuid"), "invalid", "UUID must be a string")
            })?;
            if !uuid_valid(uuid) || !uuids.insert(uuid.to_owned()) {
                return Err(issue(
                    &format!("{path}.uuid"),
                    "identity_conflict",
                    "invalid or duplicate filter UUID",
                ));
            }
        }
        let settings_path = format!("{path}.settings");
        let settings = object(required(map, "settings", &path)?, &settings_path)?;
        let mut mask_path=None;let mut lut_path=None;
        let effect = if provider=="freeze_filter"{parse_freeze(settings,&settings_path)?}
        else if provider=="gpu_delay"{known(settings,&["delay_ms"],&settings_path)?;let delay_ms=match settings.get("delay_ms"){None=>0,Some(value)=>value.as_u64().filter(|ms|*ms<=500).ok_or_else(||issue(&settings_path,"invalid","Render Delay requires an integer delay_ms in0..500"))? as u16};VideoEffect::RenderDelay{delay_ms}}
        else if provider=="scale_filter"{parse_scale(settings,&settings_path)?}
        else if provider=="scroll_filter"{parse_scroll(settings,&settings_path)?}
        else if provider=="clut_filter"{let (effect,path)=parse_lut(settings,&settings_path,base_directory)?;lut_path=path;effect}
        else if provider=="mask_filter" {let (effect,path)=parse_mask(settings,&settings_path,base_directory)?;mask_path=path;effect}
        else if provider=="crop_filter" {parse_crop_pad(settings,&settings_path)?}
        else if provider=="luma_key_filter" || provider=="sharpness_filter" {
            parse_luma_sharpen(settings,&settings_path,provider=="sharpness_filter",provider=="luma_key_filter"&&versioned!=Some("luma_key_filter_v2"))?
        } else if provider != "color_filter" {
            parse_keyer(settings, &settings_path, provider == "chroma_key_filter")?
        } else {
            known(
                settings,
                &[
                    "gamma",
                    "contrast",
                    "brightness",
                    "saturation",
                    "hue_shift",
                    "opacity",
                    "color_multiply",
                    "color_add",
                ],
                &settings_path,
            )?;
            let scalar = |key, default, min, max| -> Result<f32, ObsError> {
                let v = number(settings, key, default, &settings_path)?;
                if !(min..=max).contains(&v) {
                    return Err(issue(
                        &format!("{settings_path}.{key}"),
                        "invalid",
                        "value is outside the OBS v2 UI range",
                    ));
                }
                Ok(v as f32)
            };
            let gamma = number(settings, "gamma", 0.0, &settings_path)?;
            if !(-3.0..=3.0).contains(&gamma) {
                return Err(issue(
                    &format!("{settings_path}.gamma"),
                    "invalid",
                    "gamma outside OBS v2 range",
                ));
            }
            VideoEffect::ColorCorrection {
                gamma,
                contrast: scalar("contrast", 0.0, -4.0, 4.0)?,
                brightness: scalar("brightness", 0.0, -1.0, 1.0)?,
                saturation: scalar("saturation", 0.0, -1.0, 5.0)?,
                hue_shift: scalar("hue_shift", 0.0, -180.0, 180.0)?,
                opacity: scalar("opacity", 1.0, 0.0, 1.0)?,
                color_multiply: color(settings, "color_multiply", 0x00ff_ffff, &settings_path)?,
                color_add: color(settings, "color_add", 0, &settings_path)?,
            }
        };
        let mut filter = VideoFilter::new(effect)
            .and_then(|f| f.with_name(name.to_owned()))
            .map_err(|e| issue(&path, "engine_validation", e.to_string()))?;
        filter.enabled = boolean(map, "enabled", true, &path)?;
        output.push(InputFilter {
            filter,
            raw: map.clone(),mask_path,lut_path,
        });
    }
    Ok(output)
}
fn color(
    map: &Map<String, Value>,
    key: &str,
    default: u32,
    path: &str,
) -> Result<[u8; 3], ObsError> {
    let packed = match map.get(key) {
        None => default,
        Some(v) => v
            .as_u64()
            .filter(|v| *v <= u64::from(u32::MAX))
            .ok_or_else(|| {
                issue(
                    &format!("{path}.{key}"),
                    "invalid",
                    "expected a packed uint32 OBS color",
                )
            })? as u32,
    };
    let bytes = packed.to_le_bytes();
    Ok([bytes[0], bytes[1], bytes[2]])
}

pub(super) fn export(
    filters: &[VideoFilter],
    metadata: Option<&HashMap<VideoFilterId, Map<String, Value>>>,
    uuids: &mut HashSet<String>,
    path: &str,
) -> Result<Value, ObsError> {
    let mut output = Vec::with_capacity(filters.len());
    for (index, filter) in filters.iter().enumerate() {
        let path = format!("{path}.filters[{index}]");
        let id = filter.id().ok_or_else(|| {
            issue(
                &path,
                "identity_conflict",
                "canonical filter has no identity",
            )
        })?;
        let mut raw = metadata
            .and_then(|m| m.get(&id))
            .cloned()
            .unwrap_or_default();
        let uuid = raw
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| generated_uuid(Id::new(id.get())));
        if !uuid_valid(&uuid) || !uuids.insert(uuid.clone()) {
            return Err(issue(
                &format!("{path}.uuid"),
                "identity_conflict",
                "duplicate or invalid output filter UUID",
            ));
        }
        let original = raw.get("settings").and_then(Value::as_object);
        let packed = |key: &str, rgb: [u8; 3]| {
            let alpha = original
                .and_then(|m| m.get(key))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32
                & 0xff00_0000;
            alpha | u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0])
        };
        let (provider, settings) = match filter.effect {
            VideoEffect::Freeze{settings}=>("freeze_filter",export_freeze(settings,original,&path)?),
            VideoEffect::RenderDelay{delay_ms}=>{let mut settings=Map::new();settings.insert("delay_ms".into(),json!(delay_ms));("gpu_delay",settings)},
            effect@VideoEffect::ScaleFilter{..}=>("scale_filter",export_scale(effect,original,&path)?),
            VideoEffect::Scroll{speed_x,speed_y,limit_width,limit_height,width,height,looping}=>{
                let mut settings=Map::new();let original_float=|key:&str,value:f32|original.and_then(|map|map.get(key)).filter(|raw|raw.as_f64().is_some_and(|number|(number as f32).to_bits()==value.to_bits())).cloned().unwrap_or_else(||json!(value));
                settings.insert("speed_x".into(),original_float("speed_x",speed_x));settings.insert("speed_y".into(),original_float("speed_y",speed_y));settings.insert("limit_cx".into(),json!(limit_width));settings.insert("limit_cy".into(),json!(limit_height));settings.insert("cx".into(),json!(width));settings.insert("cy".into(),json!(height));settings.insert("loop".into(),json!(looping));if let Some(value)=original.and_then(|map|map.get("limit_size")){settings.insert("limit_size".into(),value.clone());}("scroll_filter",settings)
            }
            VideoEffect::ApplyLut{amount,passthrough_alpha}=>{
                let file=filter.lut_asset().map_or("",|asset|asset.path());
                if !file.is_empty()&&!std::path::Path::new(file).is_absolute(){return Err(issue(&path,"relative_lut_path","LUT export requires a prepared absolute asset path"));}
                let mut settings=Map::new();settings.insert("image_path".into(),json!(file));settings.insert("passthrough_alpha".into(),json!(passthrough_alpha));
                settings.insert("clut_amount".into(),original.and_then(|map|map.get("clut_amount")).filter(|value|value.as_f64().is_some_and(|number|(number as f32).to_bits()==amount.to_bits())).cloned().unwrap_or_else(||json!(amount)));
                if let Some(info)=original.and_then(|map|map.get("sdr_only_info")){settings.insert("sdr_only_info".into(),info.clone());}
                ("clut_filter",settings)
            }
            VideoEffect::ImageMask {method,color,opacity,stretch}=>{
                let file=filter.mask_image().map_or("",|a|a.path());
                if !file.is_empty()&&!std::path::Path::new(file).is_absolute(){return Err(issue(&path,"relative_mask_path","mask export requires a prepared absolute asset path"));}
                let mut settings=Map::new();settings.insert("type".into(),json!(mask_effect_file(method)));settings.insert("image_path".into(),json!(file));settings.insert("color".into(),json!(packed("color",color)));
                let value=original.and_then(|m|m.get("opacity")).filter(|v|v.as_f64().is_some_and(|n|(n as f32).to_bits()==opacity.to_bits())).cloned().unwrap_or_else(||json!(opacity));
                settings.insert("opacity".into(),value);settings.insert("stretch".into(),json!(stretch));
                if let Some(info)=original.and_then(|m|m.get("sdr_only_info")){settings.insert("sdr_only_info".into(),info.clone());}
                ("mask_filter",settings)
            }

            VideoEffect::ColorCorrection {
                gamma,
                contrast,
                brightness,
                saturation,
                hue_shift,
                opacity,
                color_multiply,
                color_add,
            } => {
                let mut settings = Map::new();
                let original_gamma = original
                    .and_then(|m| m.get("gamma"))
                    .filter(|v| v.as_f64().is_some_and(|g| g.to_bits() == gamma.to_bits()))
                    .cloned()
                    .unwrap_or_else(|| json!(gamma));
                settings.insert("gamma".into(), original_gamma);
                for (key, value) in [
                    ("contrast", contrast),
                    ("brightness", brightness),
                    ("saturation", saturation),
                    ("hue_shift", hue_shift),
                    ("opacity", opacity),
                ] {
                    let number = original
                        .and_then(|m| m.get(key))
                        .filter(|old| {
                            old.as_f64()
                                .is_some_and(|v| (v as f32).to_bits() == value.to_bits())
                        })
                        .cloned()
                        .unwrap_or_else(|| json!(value));
                    settings.insert(key.into(), number);
                }
                settings.insert(
                    "color_multiply".into(),
                    json!(packed("color_multiply", color_multiply)),
                );
                settings.insert("color_add".into(), json!(packed("color_add", color_add)));
                ("color_filter", settings)
            }
            VideoEffect::CropPad {relative,left,top,right,bottom,width,height}=>{
                let mut settings=Map::new();settings.insert("relative".into(),json!(relative));
                for (key,value) in [("left",i64::from(left)),("top",i64::from(top)),("right",i64::from(right)),("bottom",i64::from(bottom)),("cx",i64::from(width)),("cy",i64::from(height))] {settings.insert(key.into(),json!(value));}
                ("crop_filter",settings)
            }
            effect @ VideoEffect::LumaKey {..}|effect @ VideoEffect::Sharpen {..}=>export_luma_sharpen(effect,original),
            effect @ VideoEffect::ColorKey { .. } | effect @ VideoEffect::ChromaKey { .. } => {
                export_keyer(effect, original)
            }
            _ => {
                return Err(issue(
                    &path,
                    "unsupported_filter",
                    "only supported OBS v2 video filters can be exported",
                ));
            }
        };
        raw.insert("name".into(), json!(filter.name()));
        raw.insert("uuid".into(), json!(uuid));
        raw.insert("id".into(), json!(provider));
        raw.insert("versioned_id".into(),json!(if matches!(provider,"freeze_filter"|"gpu_delay"|"crop_filter"|"clut_filter"|"scroll_filter"|"scale_filter") || matches!(filter.effect,VideoEffect::LumaKey{legacy:true,..}) {provider.to_owned()}else{format!("{provider}_v2")}));
        raw.insert("enabled".into(), json!(filter.enabled));
        raw.insert("settings".into(), Value::Object(settings));
        output.push(Value::Object(raw));
    }
    Ok(Value::Array(output))
}

fn parse_crop_pad(settings:&Map<String,Value>,path:&str)->Result<VideoEffect,ObsError>{
    known(settings,&["relative","left","top","right","bottom","cx","cy"],path)?;
    let integer=|key:&str,min:i64,max:i64|->Result<i64,ObsError>{
        let value=match settings.get(key){None=>0,Some(v)=>v.as_i64().ok_or_else(||issue(&format!("{path}.{key}"),"invalid","expected an integer"))?};
        if !(min..=max).contains(&value){return Err(issue(&format!("{path}.{key}"),"invalid","outside OBS Crop/Pad range"));}Ok(value)
    };
    Ok(VideoEffect::CropPad {relative:boolean(settings,"relative",true,path)?,left:integer("left",-8192,8192)? as i32,
        top:integer("top",-8192,8192)? as i32,right:integer("right",-8192,8192)? as i32,bottom:integer("bottom",-8192,8192)? as i32,
        width:integer("cx",0,8192)? as u32,height:integer("cy",0,8192)? as u32})
}
fn parse_luma_sharpen(settings:&Map<String,Value>,path:&str,sharpen:bool,legacy:bool)->Result<VideoEffect,ObsError> {
    known(settings,if sharpen {&["sharpness"][..]}else{&["luma_max","luma_min","luma_max_smooth","luma_min_smooth"][..]},path)?;
    let scalar=|key,default|->Result<f32,ObsError>{let value=number(settings,key,default,path)?;
        if !(0.0..=1.0).contains(&value){return Err(issue(&format!("{path}.{key}"),"invalid","value is outside the OBS v2 UI range"));}Ok(value as f32)};
    Ok(if sharpen {VideoEffect::Sharpen {sharpness:scalar("sharpness",0.08)?}}
        else{VideoEffect::LumaKey {luma_max:scalar("luma_max",1.0)?,luma_min:scalar("luma_min",0.0)?,luma_max_smooth:scalar("luma_max_smooth",0.0)?,luma_min_smooth:scalar("luma_min_smooth",0.0)?,legacy}})
}
fn export_luma_sharpen(effect:VideoEffect,original:Option<&Map<String,Value>>)->(&'static str,Map<String,Value>) {
    let (provider,values):(_,Vec<(&str,f32)>)=match effect {
        VideoEffect::LumaKey {luma_max,luma_min,luma_max_smooth,luma_min_smooth,..}=>("luma_key_filter",vec![("luma_max",luma_max),("luma_min",luma_min),("luma_max_smooth",luma_max_smooth),("luma_min_smooth",luma_min_smooth)]),
        VideoEffect::Sharpen {sharpness}=>("sharpness_filter",vec![("sharpness",sharpness)]),
        _=>unreachable!("validated OBS luma/sharpen export route"),
    };
    let mut settings=Map::new();
    for (key,value) in values {let number=original.and_then(|map|map.get(key)).filter(|old|old.as_f64().is_some_and(|number|(number as f32).to_bits()==value.to_bits())).cloned().unwrap_or_else(||json!(value));settings.insert(key.into(),number);}
    (provider,settings)
}

fn parse_keyer(
    settings: &Map<String, Value>,
    path: &str,
    chroma: bool,
) -> Result<VideoEffect, ObsError> {
    let mut keys = vec![
        "key_color_type",
        "key_color",
        "similarity",
        "smoothness",
        "opacity",
        "contrast",
        "brightness",
        "gamma",
    ];
    if chroma {
        keys.push("spill");
    }
    known(settings, &keys, path)?;
    let ty = match settings.get("key_color_type") {
        None => "green",
        Some(v) => v
            .as_str()
            .ok_or_else(|| issue(path, "invalid", "key color type must be a string"))?,
    };
    let color = color(settings, "key_color", 0x0000ff00, path)?;
    let integer = |key, default| -> Result<u16, ObsError> {
        let value = match settings.get(key) {
            None => default,
            Some(v) => v.as_u64().ok_or_else(|| {
                issue(
                    &format!("{path}.{key}"),
                    "invalid",
                    "expected an integer in 1..1000",
                )
            })?,
        };
        if !(1..=1000).contains(&value) {
            return Err(issue(
                &format!("{path}.{key}"),
                "invalid",
                "expected an integer in 1..1000",
            ));
        }
        Ok(value as u16)
    };
    let scalar = |key, min, max| -> Result<f64, ObsError> {
        let value = number(
            settings,
            key,
            if key == "opacity" { 1.0 } else { 0.0 },
            path,
        )?;
        if !(min..=max).contains(&value) {
            return Err(issue(
                &format!("{path}.{key}"),
                "invalid",
                "value outside OBS v2 range",
            ));
        }
        Ok(value)
    };
    let similarity = integer("similarity", if chroma { 400 } else { 80 })?;
    let smoothness = integer("smoothness", if chroma { 80 } else { 50 })?;
    let opacity = scalar("opacity", 0.0, 1.0)? as f32;
    let contrast = scalar("contrast", -4.0, 4.0)?;
    let brightness = scalar("brightness", -1.0, 1.0)? as f32;
    let gamma = scalar("gamma", -1.0, 1.0)?;
    if chroma {
        use skylive_core::ChromaKeyType as T;
        let key_color_type = match ty {
            "green" => T::Green,
            "blue" => T::Blue,
            "magenta" => T::Magenta,
            "custom" => T::Custom,
            _ => return Err(issue(path, "invalid", "unsupported chroma key preset")),
        };
        Ok(VideoEffect::ChromaKey {
            key_color_type,
            key_color: color,
            similarity,
            smoothness,
            spill: integer("spill", 100)?,
            opacity,
            contrast,
            brightness,
            gamma,
        })
    } else {
        use skylive_core::ColorKeyType as T;
        let key_color_type = match ty {
            "green" => T::Green,
            "blue" => T::Blue,
            "red" => T::Red,
            "magenta" => T::Magenta,
            "custom" => T::Custom,
            _ => return Err(issue(path, "invalid", "unsupported color key preset")),
        };
        Ok(VideoEffect::ColorKey {
            key_color_type,
            key_color: color,
            similarity,
            smoothness,
            opacity,
            contrast,
            brightness,
            gamma,
        })
    }
}
fn export_keyer(
    effect: VideoEffect,
    original: Option<&Map<String, Value>>,
) -> (&'static str, Map<String, Value>) {
    let (provider, ty, color, similarity, smoothness, spill, opacity, contrast, brightness, gamma) =
        match effect {
            VideoEffect::ColorKey {
                key_color_type,
                key_color,
                similarity,
                smoothness,
                opacity,
                contrast,
                brightness,
                gamma,
            } => {
                use skylive_core::ColorKeyType as T;
                (
                    "color_key_filter",
                    match key_color_type {
                        T::Green => "green",
                        T::Blue => "blue",
                        T::Red => "red",
                        T::Magenta => "magenta",
                        T::Custom => "custom",
                    },
                    key_color,
                    similarity,
                    smoothness,
                    None,
                    opacity,
                    contrast,
                    brightness,
                    gamma,
                )
            }
            VideoEffect::ChromaKey {
                key_color_type,
                key_color,
                similarity,
                smoothness,
                spill,
                opacity,
                contrast,
                brightness,
                gamma,
            } => {
                use skylive_core::ChromaKeyType as T;
                (
                    "chroma_key_filter",
                    match key_color_type {
                        T::Green => "green",
                        T::Blue => "blue",
                        T::Magenta => "magenta",
                        T::Custom => "custom",
                    },
                    key_color,
                    similarity,
                    smoothness,
                    Some(spill),
                    opacity,
                    contrast,
                    brightness,
                    gamma,
                )
            }
            _ => unreachable!("caller admits keyer variants only"),
        };
    let alpha = original
        .and_then(|s| s.get("key_color"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32
        & 0xff000000;
    let mut settings = Map::new();
    settings.insert("key_color_type".into(), json!(ty));
    settings.insert(
        "key_color".into(),
        json!(alpha | u32::from_le_bytes([color[0], color[1], color[2], 0])),
    );
    settings.insert("similarity".into(), json!(similarity));
    settings.insert("smoothness".into(), json!(smoothness));
    if let Some(spill) = spill {
        settings.insert("spill".into(), json!(spill));
    }
    for (key, value) in [("gamma", gamma), ("contrast", contrast)] {
        let v = original
            .and_then(|s| s.get(key))
            .filter(|v| {
                v.as_f64()
                    .is_some_and(|old| old.to_bits() == value.to_bits())
            })
            .cloned()
            .unwrap_or_else(|| json!(value));
        settings.insert(key.into(), v);
    }
    for (key, value) in [("brightness", brightness), ("opacity", opacity)] {
        let v = original
            .and_then(|s| s.get(key))
            .filter(|v| {
                v.as_f64()
                    .is_some_and(|old| (old as f32).to_bits() == value.to_bits())
            })
            .cloned()
            .unwrap_or_else(|| json!(value));
        settings.insert(key.into(), v);
    }
    (provider, settings)
}

fn mask_effect_file(method:skylive_core::ImageMaskMethod)->&'static str {match method {
    skylive_core::ImageMaskMethod::MaskColor=>"mask_color_filter.effect",skylive_core::ImageMaskMethod::MaskAlpha=>"mask_alpha_filter.effect",
    skylive_core::ImageMaskMethod::BlendMultiply=>"blend_mul_filter.effect",skylive_core::ImageMaskMethod::BlendAddition=>"blend_add_filter.effect",skylive_core::ImageMaskMethod::BlendSubtraction=>"blend_sub_filter.effect"}}
fn parse_mask(settings:&Map<String,Value>,path:&str,base:Option<&std::path::Path>)->Result<(VideoEffect,Option<std::path::PathBuf>),ObsError>{
    known(settings,&["type","image_path","color","opacity","stretch","sdr_only_info"],path)?;
    if settings.get("sdr_only_info").is_some_and(|v|!v.is_string()){return Err(issue(path,"invalid","mask information must be text"));}
    let effect_file=match settings.get("type"){None=>"mask_color_filter.effect",Some(value)=>value.as_str().ok_or_else(||issue(path,"invalid","mask type must be a string"))?};
    let method=skylive_core::ImageMaskMethod::ALL.into_iter().find(|method|mask_effect_file(*method)==effect_file).ok_or_else(||issue(path,"unsupported_filter","unsupported Image Mask/Blend method"))?;
    let opacity=number(settings,"opacity",1.0,path)?;if !(0.0..=1.0).contains(&opacity){return Err(issue(path,"invalid","mask opacity must be finite in0..1"));}
    let file=match settings.get("image_path"){None=>"",Some(value)=>value.as_str().ok_or_else(||issue(path,"invalid","mask image_path must be text"))?};
    if file.len()>4096||file.chars().any(char::is_control){return Err(issue(path,"invalid","mask image_path exceeds4096bytes or contains control characters"));}
    let file=if file.is_empty(){None}else{let file=std::path::PathBuf::from(file);Some(if file.is_absolute(){file}else{let base=base.filter(|p|p.is_absolute()).ok_or_else(||issue(path,"relative_mask_path","relative mask requires an explicit absolute import base"))?;base.join(file)})};
    Ok((VideoEffect::ImageMask {method,color:color(settings,"color",0x00ff_ffff,path)?,opacity:opacity as f32,stretch:boolean(settings,"stretch",false,path)?},file))
}

fn parse_lut(settings:&Map<String,Value>,path:&str,base:Option<&std::path::Path>)->Result<(VideoEffect,Option<std::path::PathBuf>),ObsError>{
    known(settings,&["image_path","clut_amount","passthrough_alpha","sdr_only_info"],path)?;
    if settings.get("sdr_only_info").is_some_and(|value|!value.is_string()){return Err(issue(path,"invalid","LUT information must be text"));}
    let amount=number(settings,"clut_amount",1.0,path)?;if !(0.0..=1.0).contains(&amount){return Err(issue(path,"invalid","LUT amount must be finite in0..1"));}
    let file=match settings.get("image_path"){None=>"",Some(value)=>value.as_str().ok_or_else(||issue(path,"invalid","LUT image_path must be text"))?};
    if file.len()>4096||file.chars().any(char::is_control){return Err(issue(path,"invalid","LUT image_path exceeds4096 bytes or contains controls"));}
    let file=if file.is_empty(){None}else{let file=std::path::PathBuf::from(file);Some(if file.is_absolute(){file}else{base.filter(|path|path.is_absolute()).ok_or_else(||issue(path,"relative_lut_path","relative LUT requires an explicit absolute import base"))?.join(file)})};
    Ok((VideoEffect::ApplyLut{amount:amount as f32,passthrough_alpha:boolean(settings,"passthrough_alpha",false,path)?},file))
}

fn parse_scroll(settings:&Map<String,Value>,path:&str)->Result<VideoEffect,ObsError>{
    known(settings,&["speed_x","speed_y","limit_cx","limit_cy","cx","cy","loop","limit_size"],path)?;
    if settings.get("limit_size").is_some_and(|value|!value.is_boolean()){return Err(issue(path,"invalid","Scroll legacy limit_size must be boolean"));}
    let speed_x=number(settings,"speed_x",0.0,path)?;let speed_y=number(settings,"speed_y",0.0,path)?;
    if ![speed_x,speed_y].into_iter().all(|v|v.is_finite()&&(-500.0..=500.0).contains(&v)){return Err(issue(path,"invalid","Scroll speeds require finite values in -500..500 pixels per second"));}
    let dimension=|key:&str|->Result<u32,ObsError>{match settings.get(key){None=>Ok(100),Some(value)=>value.as_u64().filter(|v|(1..=8192).contains(v)).map(|v|v as u32).ok_or_else(||issue(path,"invalid","Scroll dimensions require integers in 1..8192"))}};
    Ok(VideoEffect::Scroll {speed_x:speed_x as f32,speed_y:speed_y as f32,limit_width:boolean(settings,"limit_cx",false,path)?,limit_height:boolean(settings,"limit_cy",false,path)?,width:dimension("cx")?,height:dimension("cy")?,looping:boolean(settings,"loop",true,path)?})
}

fn scale_target(value:&str,path:&str)->Result<skylive_core::ScaleFilterTarget,ObsError>{
    use skylive_core::ScaleFilterTarget as T;
    if scale_filter_labels::NONE.contains(&value){return Ok(T::None);}
    if scale_filter_labels::CANVAS.contains(&value){return Ok(T::Canvas);}
    let pair=if let Some(pair)=value.split_once('x'){(pair,false)}else if let Some(pair)=value.split_once(':'){(pair,true)}else{return Err(issue(path,"unsupported","unrecognized OBS scale resolution/aspect or localized selector"));};
    let term=|text:&str|text.trim().parse::<u32>().ok().filter(|n|(1..=8192).contains(n));
    let width=term(pair.0.0).ok_or_else(||issue(path,"invalid","scale dimensions/aspect terms require1..8192"))?;
    let height=term(pair.0.1).ok_or_else(||issue(path,"invalid","scale dimensions/aspect terms require1..8192"))?;
    Ok(if pair.1{T::Aspect{width,height}}else{T::Resolution{width,height}})
}
fn parse_scale(settings:&Map<String,Value>,path:&str)->Result<VideoEffect,ObsError>{
    known(settings,&["resolution","sampling","undistort"],path)?;
    let target=settings.get("resolution").map(|value|value.as_str().ok_or_else(||issue(path,"invalid","scale resolution must be text")).and_then(|value|scale_target(value,path))).transpose()?.unwrap_or_default();
    let sampling=match settings.get("sampling"){
        None=>skylive_core::ScaleFilterSampling::default(),
        Some(value)=>{let value=value.as_str().ok_or_else(||issue(path,"invalid","scale sampling must be text"))?;
            skylive_core::ScaleFilterSampling::ALL.into_iter().find(|sampling|sampling.wire().eq_ignore_ascii_case(value)).ok_or_else(||issue(path,"unsupported","unknown scale sampling method"))?}
    };
    Ok(VideoEffect::ScaleFilter{target,sampling,undistort:boolean(settings,"undistort",false,path)?})
}
fn export_scale(effect:VideoEffect,original:Option<&Map<String,Value>>,path:&str)->Result<Map<String,Value>,ObsError>{
    let VideoEffect::ScaleFilter{target,sampling,undistort}=effect else{return Err(issue(path,"invalid","not a scale filter"));};
    let old=original.and_then(|settings|settings.get("resolution")).and_then(Value::as_str).filter(|value|scale_target(value,path).ok()==Some(target));
    let resolution=if let Some(old)=old{old.to_owned()}else{match target{
        skylive_core::ScaleFilterTarget::None=>"None".into(),
        skylive_core::ScaleFilterTarget::Canvas=>return Err(issue(path,"localized_canvas_export","a newly authored dynamic Canvas filter needs the destination OBS language; choose an explicit pixel resolution for portable OBS export")),
        skylive_core::ScaleFilterTarget::Resolution{width,height}=>format!("{width}x{height}"),
        skylive_core::ScaleFilterTarget::Aspect{width,height}=>format!("{width}:{height}"),
    }};
    let mut settings=Map::new();settings.insert("resolution".into(),json!(resolution));
    let value=original.and_then(|settings|settings.get("sampling")).filter(|value|value.as_str().is_some_and(|value|sampling.wire().eq_ignore_ascii_case(value))).cloned().unwrap_or_else(||json!(sampling.wire()));
    settings.insert("sampling".into(),value);settings.insert("undistort".into(),json!(undistort));Ok(settings)
}

// Exeldro Freeze0.3.5: duration, refresh and manual mask. Unknown or other
// non-neutral automatic controls are errors, including on a disabled filter.
fn parse_freeze(settings:&Map<String,Value>,path:&str)->Result<VideoEffect,ObsError>{
 known(settings,&["duration","refresh_interval","start_delay","end_delay","activate_action","deactivate_action","show_action","hide_action","log_enable","mask","mask_left","mask_right","mask_top","mask_bottom"],path)?;
 for key in ["start_delay","end_delay","activate_action","deactivate_action","show_action","hide_action"]{neutral(settings,key,json!(0),path)?;}
 neutral(settings,"log_enable",json!(false),path)?;
 let time=|key:&str|->Result<u32,ObsError>{match settings.get(key){None=>Ok(0),Some(value)=>value.as_u64().filter(|v|*v<=100_000).map(|v|v as u32).ok_or_else(||issue(&format!("{path}.{key}"),"invalid","Freeze timings require 0..100000 milliseconds"))}};
 let mut percent=[0.0;4];
 for(index,key)in ["mask_left","mask_right","mask_top","mask_bottom"].into_iter().enumerate(){
  let value=match settings.get(key){None=>0.0,Some(value)=>value.as_f64().filter(|n|n.is_finite()&&(0.0..=100.0).contains(n)).ok_or_else(||issue(&format!("{path}.{key}"),"invalid","Freeze mask requires a percentage in 0..100"))?};
  percent[index]=value as f32;
 }
 let mask=boolean(settings,"mask",false,path)?;
 let settings=skylive_core::FreezeSettings::default().with_timing([time("duration")?,time("refresh_interval")?,0,0]).and_then(|value|value.with_mask(mask,percent)).map_err(|error|issue(path,"invalid",&error.to_string()))?;
 Ok(VideoEffect::Freeze{settings})
}
fn export_freeze(settings:skylive_core::FreezeSettings,original:Option<&Map<String,Value>>,path:&str)->Result<Map<String,Value>,ObsError>{
 settings.validate_manual().map_err(|error|issue(path,"unsupported_filter",&error.to_string()))?;
 let mut result=Map::new();
 for key in ["duration","refresh_interval","start_delay","end_delay","activate_action","deactivate_action","show_action","hide_action"]{result.insert(key.into(),json!(0));}
 result.insert("duration".into(),json!(settings.duration_ms()));result.insert("refresh_interval".into(),json!(settings.refresh_interval_ms()));
 result.insert("log_enable".into(),json!(false));result.insert("mask".into(),json!(settings.mask()));
 for(key,value)in ["mask_left","mask_right","mask_top","mask_bottom"].into_iter().zip(settings.mask_percent()){let encoded=original.and_then(|map|map.get(key)).filter(|raw|raw.as_f64().is_some_and(|number|(number as f32).to_bits()==value.to_bits())).cloned().unwrap_or_else(||json!(value));result.insert(key.into(),encoded);}
 Ok(result)
}
