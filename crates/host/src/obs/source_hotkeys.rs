//! Exact Windows Slideshow/local Media Source hotkeys. No registry/native calls.
use super::*;
use skylive_core::{HotkeyChord,SourcePlaybackAction as Action,SourcePlaybackBinding,SourcePlaybackHotkeys,SourceAudioAction as Audio,SourceAudioBinding,SourceAudioHotkeys,UserPreferences};
const SLIDESHOW:[(&str,Action);5]=[("SlideShow.PlayPause",Action::PlayPause),("SlideShow.Restart",Action::Restart),("SlideShow.Stop",Action::Stop),("SlideShow.PreviousSlide",Action::Previous),("SlideShow.NextSlide",Action::Next)];
const MEDIA:[(&str,Action);4]=[("MediaSource.Play",Action::Play),("MediaSource.Pause",Action::Pause),("MediaSource.Restart",Action::Restart),("MediaSource.Stop",Action::Stop)];
#[derive(Clone,Copy,PartialEq,Eq)]
pub(super) enum Provider {Slideshow,Media,Other}
impl Provider {
    fn controls(self)->&'static [(&'static str,Action)]{match self{Self::Slideshow=>&SLIDESHOW,Self::Media=>&MEDIA,Self::Other=>&[]}}
}
// Exact parent-source names registered by Freeze0.3.5. Empty arrays are
// neutral preserved metadata; nonempty bindings remain explicitly unsupported.
const FREEZE_NEUTRAL:[&str;3]=["Freeze.Enable","Freeze.Disable","Freeze.Screenshot"];
const AUDIO:[(&str,Audio);4]=[("libobs.mute",Audio::Mute),("libobs.unmute",Audio::Unmute),("libobs.push-to-mute",Audio::PushToMute),("libobs.push-to-talk",Audio::PushToTalk)];
const NAMED:[(&str,&str);20]=[("OBS_KEY_NUMLOCK","NumLock"),("OBS_KEY_NUMASTERISK","NumpadMultiply"),("OBS_KEY_NUMPLUS","NumpadAdd"),("OBS_KEY_NUMMINUS","NumpadSubtract"),("OBS_KEY_NUMPERIOD","NumpadDecimal"),("OBS_KEY_NUMSLASH","NumpadDivide"),("OBS_KEY_RETURN","Enter"),("OBS_KEY_TAB","Tab"),("OBS_KEY_BACKSPACE","Backspace"),("OBS_KEY_INSERT","Insert"),("OBS_KEY_DELETE","Delete"),("OBS_KEY_HOME","Home"),("OBS_KEY_END","End"),("OBS_KEY_LEFT","ArrowLeft"),("OBS_KEY_RIGHT","ArrowRight"),("OBS_KEY_UP","ArrowUp"),("OBS_KEY_DOWN","ArrowDown"),("OBS_KEY_PAGEUP","PageUp"),("OBS_KEY_PAGEDOWN","PageDown"),("OBS_KEY_SPACE","Space")];
fn key_from_obs(value:&str,path:&str)->Result<String,ObsError>{
    if value.len()>64{return Err(issue(path,"source_hotkey_key_limit","OBS source hotkey key token exceeds 64 bytes"));}
    if let Some((_,key))=NAMED.iter().find(|(obs,_)|*obs==value){return Ok((*key).into());}
    let key=value.strip_prefix("OBS_KEY_").ok_or_else(||issue(path,"source_hotkey_key_unsupported","OBS source hotkeys require an exact supported OBS_KEY token; aliases are not guessed"))?;
    if let Some(digit)=key.strip_prefix("NUM").filter(|digit|digit.len()==1&&digit.as_bytes()[0].is_ascii_digit()){return Ok(format!("Numpad{digit}"));}
    let ascii=key.len()==1&&key.bytes().all(|byte|byte.is_ascii_uppercase()||byte.is_ascii_digit());
    let function=key.strip_prefix('F').and_then(|value|value.parse::<u8>().ok()).is_some_and(|number|(1..=24).contains(&number)&&key==format!("F{number}"));
    if ascii||function{Ok(key.into())}else{Err(issue(path,"source_hotkey_key_unsupported","This OBS source hotkey key is outside the exact Windows subset; modifier-only and punctuation keys are not converted"))}
}
fn key_to_obs(value:&str,path:&str)->Result<String,ObsError>{
    if let Some((obs,_))=NAMED.iter().find(|(_,key)|*key==value){return Ok((*obs).into());}
    if let Some(digit)=value.strip_prefix("Numpad").filter(|digit|digit.len()==1&&digit.as_bytes()[0].is_ascii_digit()){return Ok(format!("OBS_KEY_NUM{digit}"));}
    let obs=format!("OBS_KEY_{value}");if key_from_obs(&obs,path)?==value{Ok(obs)}else{Err(issue(path,"source_hotkey_key_unsupported","Native source hotkey has no exact OBS key representation"))}
}
fn chord(value:&Value,path:&str)->Result<HotkeyChord,ObsError>{
    let row=object(value,path)?;
    if row.keys().any(|key|!["key","control","alt","shift","command"].contains(&key.as_str())){return Err(issue(path,"source_hotkey_fields_unsupported","OBS source hotkey entries support only key, control, alt, shift and command fields; unknown data is not dropped"));}
    if boolean(row,"command",false,path)?{return Err(issue(path,"source_hotkey_modifier_unsupported","Command, Meta and Windows-key source hotkeys cannot be converted to native Ctrl bindings"));}
    let key=key_from_obs(string(row,"key",path)?,path)?;
    let chord=HotkeyChord::new(&key,boolean(row,"control",false,path)?,boolean(row,"alt",false,path)?,boolean(row,"shift",false,path)?).map_err(|error|issue(path,"source_hotkey_invalid",error.to_string()))?;
    if !skylive_core::global_key_supported(&chord){return Err(issue(path,"source_hotkey_key_unsupported","Native global source hotkey policy does not support this key"));}Ok(chord)
}
fn table<'a>(source:&'a Map<String,Value>,path:&str)->Result<Option<&'a Map<String,Value>>,ObsError>{
    source.get("hotkeys").map(|value|object(value,&format!("{path}.hotkeys"))).transpose()
}
pub(super) fn parse(source:&Map<String,Value>,path:&str,provider:Provider)->Result<SourcePlaybackHotkeys,ObsError>{
    let Some(table)=table(source,path)?else{return Ok(SourcePlaybackHotkeys::default());};if table.len()>12{return Err(issue(&format!("{path}.hotkeys"),"source_hotkey_limit","OBS source hotkey object exceeds the represented twelve-name subset"));}let mut bindings=Vec::new();
    for(name,value)in table{if name.len()>128{return Err(issue(&format!("{path}.hotkeys"),"source_hotkey_name_limit","OBS source hotkey action name exceeds 128 bytes"));}let at=format!("{path}.hotkeys.{name}");let rows=array(value,&at)?;
        if AUDIO.iter().any(|(key,_)|*key==name.as_str()){continue;}
        if FREEZE_NEUTRAL.contains(&name.as_str()){if !rows.is_empty(){return Err(issue(&at,"freeze_hotkeys_unsupported","Freeze hotkeys are not implemented in the manual port; nonempty bindings cannot be imported"));}continue;}
        let action=provider.controls().iter().find(|(key,_)|*key==name.as_str()).map(|(_,action)|*action).ok_or_else(||issue(&at,"source_hotkey_action_unsupported","Unknown OBS source hotkey action for this provider; frontend bindings and toggle aliases are not converted"))?;
        let wire_limit=if provider==Provider::Media&&matches!(action,Action::Play|Action::Pause){4}else{2};
        if rows.len()>wire_limit{return Err(issue(&at,"source_hotkey_limit","OBS Media Source Play/Pause arrays admit at most four shortcuts; other controls admit two"));}
        if !rows.is_empty()&&!cfg!(target_os="windows"){return Err(issue(&at,"source_hotkey_platform_unsupported","OBS source hotkey interchange currently preserves Windows Ctrl semantics only"));}
        let mut action_chords=HashSet::new();
        for(index,row)in rows.iter().enumerate(){
            let at=format!("{at}[{index}]");let chord=chord(row,&at)?;
            if !action_chords.insert(chord.clone()){return Err(issue(&at,"source_hotkey_duplicate_row","OBS source hotkey repeats a shortcut within one action; duplicate rows are not discarded"));}
            bindings.push(SourcePlaybackBinding{operation:action,chord});
        }
    }
    if provider==Provider::Media {
        if bindings.len()>12{return Err(issue(&format!("{path}.hotkeys"),"source_hotkey_limit","OBS Media Source hotkey wire data exceeds twelve rows"));}
        // The exact original pair applies Play iff not Playing, otherwise Pause.
        // Its intersection is the current Host's single pressed-event Toggle.
        // This normalizes binding sets, not libobs held-key/pair latch state.
        let common:Vec<_>=bindings.iter().filter(|binding|binding.operation==Action::Play&&bindings.iter().any(|other|other.operation==Action::Pause&&other.chord==binding.chord)).map(|binding|binding.chord.clone()).collect();
        let mut normalized=Vec::with_capacity(bindings.len().saturating_sub(common.len()));
        for mut binding in bindings {
            if common.contains(&binding.chord){
                if binding.operation==Action::Pause{continue;}
                if binding.operation==Action::Play{binding.operation=Action::PlayPause;}
            }
            normalized.push(binding);
        }
        bindings=normalized;
    }
    SourcePlaybackHotkeys::new(bindings).map_err(|error|issue(&format!("{path}.hotkeys"),"source_hotkey_conflict",error.to_string()))
}
/// Same canonical uniqueness policy is checked again by SetSourcePlaybackHotkeys
/// after IDs resolve. This preflight avoids image/filesystem work on bad chords.
pub(super) fn preflight<'a>(sources:impl IntoIterator<Item=(&'a SourcePlaybackHotkeys,&'a SourceAudioHotkeys)>,preferences:&UserPreferences)->Result<(),ObsError>{
    let mut chords=HashSet::new();for binding in preferences.hotkeys().bindings().iter().chain(preferences.global_hotkeys().bindings()){chords.insert(&binding.chord);}
    let mut count=0usize;for(index,(playback,audio))in sources.into_iter().enumerate(){for chord in playback.chords().chain(audio.chords()){
        count+=1;if count>skylive_core::MAX_SOURCE_HOTKEYS{return Err(issue("$.sources.hotkeys","source_hotkey_limit","OBS source controls exceed the shared limit of 106 bindings"));}
        if !chords.insert(chord){return Err(issue(&format!("$.sources[{index}].hotkeys"),"source_hotkey_scope_conflict","OBS source shortcut conflicts with another source or existing local/global application shortcut"));}
    }}Ok(())
}
pub(super) fn export(source:&skylive_core::Source,object:&mut Map<String,Value>,path:&str)->Result<(),ObsError>{
    let provider=match source.kind(){SourceKind::ImageSlideShow{..}=>Provider::Slideshow,SourceKind::MediaSource{..}=>Provider::Media,_=>Provider::Other};let bindings=source.playback_hotkeys();
    if !bindings.is_empty()&&provider==Provider::Other{return Err(issue(path,"source_hotkey_provider_unsupported","This source provider has no exact OBS playback hotkey representation"));}
    if bindings.bindings().iter().any(|binding|!provider.controls().iter().any(|(_,action)|*action==binding.operation)&&!(provider==Provider::Media&&binding.operation==Action::PlayPause)){
        return Err(issue(path,"source_hotkey_action_unsupported","Playback shortcut action has no exact OBS representation for this source provider"));
    }
    if !bindings.is_empty()&&!cfg!(target_os="windows"){return Err(issue(path,"source_hotkey_platform_unsupported","OBS source hotkey interchange currently preserves Windows Ctrl semantics only"));}
    // Reject unsupported sidecar data before regenerating known controls. Empty
    // libobs keys are neutral metadata and remain verbatim, never active aliases.
    let _=parse(object,path,provider)?;let _=parse_audio(object,path,true)?;let mut output=Map::new();if let Some(old)=table(object,path)?{for name in FREEZE_NEUTRAL{if let Some(value)=old.get(name){output.insert(name.into(),value.clone());}}}
    for &(name,action)in provider.controls(){let mut rows=Vec::new();for binding in bindings.bindings().iter().filter(|binding|binding.operation==action||provider==Provider::Media&&matches!(action,Action::Play|Action::Pause)&&binding.operation==Action::PlayPause){let mut row=Map::new();row.insert("key".into(),Value::String(key_to_obs(binding.chord.key(),path)?));if binding.chord.primary(){row.insert("control".into(),Value::Bool(true));}if binding.chord.alt(){row.insert("alt".into(),Value::Bool(true));}if binding.chord.shift(){row.insert("shift".into(),Value::Bool(true));}rows.push(Value::Object(row));}output.insert(name.into(),Value::Array(rows));}
    export_audio(source,&mut output,path)?;
    object.insert("hotkeys".into(),Value::Object(output));Ok(())
}

/// libobs/obs-source.c297..370: common pair intersection is one canonical
/// ToggleMute route; exclusive Mute/Unmute rows keep their exact names.
pub(super) fn parse_audio(source:&Map<String,Value>,path:&str,eligible:bool)->Result<SourceAudioHotkeys,ObsError>{
    let Some(table)=table(source,path)?else{return Ok(SourceAudioHotkeys::default())};
    let mut bindings=Vec::new();
    for &(name,operation)in &AUDIO{let Some(value)=table.get(name)else{continue};let at=format!("{path}.hotkeys.{name}");let rows=array(value,&at)?;
        let limit=if matches!(operation,Audio::Mute|Audio::Unmute){4}else{2};
        if rows.len()>limit{return Err(issue(&at,"source_audio_hotkey_limit","OBS Mute/Unmute arrays admit four shortcuts; held controls admit two"));}
        if !rows.is_empty()&&!eligible{return Err(issue(&at,"source_audio_hotkey_provider","Mute shortcuts require an audio-capable input source."));}
        if !rows.is_empty()&&!cfg!(target_os="windows"){return Err(issue(&at,"source_hotkey_platform_unsupported","OBS source hotkey interchange currently preserves Windows Ctrl semantics only"));}
        let mut seen=HashSet::new();for(index,value)in rows.iter().enumerate(){let at=format!("{at}[{index}]");let chord=chord(value,&at)?;if !seen.insert(chord.clone()){return Err(issue(&at,"source_hotkey_duplicate_row","OBS source hotkey repeats a shortcut within one action; duplicate rows are not discarded"));}bindings.push(SourceAudioBinding{operation,chord});}
    }
    let common:Vec<_>=bindings.iter().filter(|binding|binding.operation==Audio::Mute&&bindings.iter().any(|other|other.operation==Audio::Unmute&&other.chord==binding.chord)).map(|binding|binding.chord.clone()).collect();
    let mut normalized=Vec::with_capacity(bindings.len().saturating_sub(common.len()));
    for mut binding in bindings{if common.contains(&binding.chord){if binding.operation==Audio::Unmute{continue;}binding.operation=Audio::ToggleMute;}normalized.push(binding);}
    SourceAudioHotkeys::new(normalized).map_err(|error|issue(&format!("{path}.hotkeys"),"source_audio_hotkey_conflict",error.to_string()))
}
fn export_audio(source:&skylive_core::Source,output:&mut Map<String,Value>,path:&str)->Result<(),ObsError>{
    let bindings=source.audio_hotkeys();bindings.validate_kind(source.kind()).map_err(|error|issue(path,"source_audio_hotkey_provider",error.to_string()))?;
    if !bindings.is_empty()&&!cfg!(target_os="windows"){return Err(issue(path,"source_hotkey_platform_unsupported","OBS source hotkey interchange currently preserves Windows Ctrl semantics only"));}
    // Unknown/unsupported raw keys were checked before this regeneration. Do
    // not invent a libobs Toggle name: both original pair arrays carry it.
    if !source.kind().supports_audio_settings()&&bindings.is_empty(){return Ok(())}
    for &(name,operation)in &AUDIO{let mut rows=Vec::new();for binding in bindings.bindings().iter().filter(|binding|binding.operation==operation||matches!(operation,Audio::Mute|Audio::Unmute)&&binding.operation==Audio::ToggleMute){
        let mut row=Map::new();row.insert("key".into(),Value::String(key_to_obs(binding.chord.key(),path)?));if binding.chord.primary(){row.insert("control".into(),Value::Bool(true));}if binding.chord.alt(){row.insert("alt".into(),Value::Bool(true));}if binding.chord.shift(){row.insert("shift".into(),Value::Bool(true));}rows.push(Value::Object(row));
    }output.insert(name.into(),Value::Array(rows));}Ok(())
}
