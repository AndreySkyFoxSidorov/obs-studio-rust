//! Authored audio capture identities and global source roots. Native devices,
//! negotiated PCM formats, clocks and DSP state remain host/runtime concerns.
use crate::{Error, Id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFlow {
    Input,
    Output,
}
/// Stable Windows process identity. A PID alone is never a capture binding.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ProcessIdentityData")]
pub struct WindowsProcessIdentity {
    pid: u32,
    creation_time_100ns: u64,
    executable: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessIdentityData { pid: u32, creation_time_100ns: u64, executable: String }
impl TryFrom<ProcessIdentityData> for WindowsProcessIdentity {
    type Error = Error;
    fn try_from(value: ProcessIdentityData) -> Result<Self, Error> {
        Self::new(value.pid, value.creation_time_100ns, value.executable)
    }
}
impl WindowsProcessIdentity {
    pub fn new(pid: u32, creation_time_100ns: u64, executable: String) -> Result<Self, Error> {
        let value = Self { pid, creation_time_100ns, executable }; value.validate()?; Ok(value.normalized())
    }
    pub fn pid(&self) -> u32 { self.pid }
    pub fn creation_time_100ns(&self) -> u64 { self.creation_time_100ns }
    pub fn executable(&self) -> &str { &self.executable }
    fn validate(&self) -> Result<(), Error> {
        if self.pid == 0 || self.creation_time_100ns == 0 || self.executable.trim().is_empty()
            || self.executable.len() > 4096 || self.executable.chars().any(char::is_control)
            || !((self.executable.as_bytes().get(1) == Some(&b':') && matches!(self.executable.as_bytes().get(2),Some(b'\\' | b'/'))) || self.executable.starts_with(r"\\")) {
            return Err(Error::Invalid("Windows process identity requires nonzero PID/creation time and an absolute executable path of at most 4096 UTF-8 bytes"));
        }
        Ok(())
    }
    fn normalized(self) -> Self { Self { executable: self.executable.into_boxed_str().into_string(), ..self } }
}

/// Persistent OBS-style selection, distinct from the live PID lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowsAudioWindowPriority { Title, Class, Executable }
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "WindowsAudioWindowData")]
pub struct WindowsAudioWindowSelector { title:String, class:String, executable:String, priority:WindowsAudioWindowPriority }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsAudioWindowData { title:String, class:String, executable:String, priority:WindowsAudioWindowPriority }
impl TryFrom<WindowsAudioWindowData> for WindowsAudioWindowSelector {
    type Error=Error;
    fn try_from(value:WindowsAudioWindowData)->Result<Self,Error>{Self::new(value.title,value.class,value.executable,value.priority)}
}
impl WindowsAudioWindowSelector {
    pub fn new(title:String,class:String,executable:String,priority:WindowsAudioWindowPriority)->Result<Self,Error>{
        if [&title,&class,&executable].iter().any(|s|s.len()>4096||s.contains('\0')){return Err(Error::Invalid("application audio window selector fields exceed 4096 bytes or contain NUL"));}
        Ok(Self {title,class,executable,priority})
    }
    pub fn title(&self)->&str{&self.title}
    pub fn class(&self)->&str{&self.class}
    pub fn executable(&self)->&str{&self.executable}
    pub fn priority(&self)->WindowsAudioWindowPriority{self.priority}
    fn validate(&self)->Result<(),Error>{Self::new(self.title.clone(),self.class.clone(),self.executable.clone(),self.priority).map(|_|())}
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioTarget {
    Default,
    WindowsEndpoint { id: String },
    WindowsProcess { identity: WindowsProcessIdentity },
    WindowsWindow { selector: WindowsAudioWindowSelector },
    MacDevice { uid: String },
    MacSystem,
}
impl AudioTarget {
    fn validate(&self) -> Result<(), Error> {
        if let Self::WindowsProcess { identity } = self { identity.validate()?; }
        if let Self::WindowsWindow { selector } = self { selector.validate()?; }
        let id = match self {
            Self::WindowsEndpoint { id } => Some(id),
            Self::MacDevice { uid } => Some(uid),
            _ => None,
        };
        if id.is_some_and(|id| {
            id.trim().is_empty() || id.len() > 1024 || id.chars().any(char::is_control)
        }) {
            return Err(Error::Invalid(
                "audio device identity requires 1..=1024 UTF-8 bytes without controls",
            ));
        }
        Ok(())
    }
    fn normalized(self) -> Self {
        match self {
            Self::WindowsEndpoint { id } => Self::WindowsEndpoint {
                id: id.into_boxed_str().into_string(),
            },
            Self::WindowsProcess { identity } => Self::WindowsProcess { identity: identity.normalized() },
            Self::MacDevice { uid } => Self::MacDevice {
                uid: uid.into_boxed_str().into_string(),
            },
            other => other,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "BindingData")]
pub struct AudioCaptureBinding {
    flow: AudioFlow,
    target: AudioTarget,
    use_device_timing: bool,
    exclude_own_audio: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingData {
    flow: AudioFlow,
    target: AudioTarget,
    use_device_timing: bool,
    exclude_own_audio: bool,
}
impl TryFrom<BindingData> for AudioCaptureBinding {
    type Error = Error;
    fn try_from(value: BindingData) -> Result<Self, Error> {
        Self::new(
            value.flow,
            value.target,
            value.use_device_timing,
            value.exclude_own_audio,
        )
    }
}
impl AudioCaptureBinding {
    pub fn new(
        flow: AudioFlow,
        target: AudioTarget,
        use_device_timing: bool,
        exclude_own_audio: bool,
    ) -> Result<Self, Error> {
        let binding = Self {
            flow,
            target,
            use_device_timing,
            exclude_own_audio,
        };
        binding.validate()?;
        Ok(Self {
            target: binding.target.normalized(),
            ..binding
        })
    }
    pub fn flow(&self) -> AudioFlow {
        self.flow
    }
    pub fn target(&self) -> &AudioTarget {
        &self.target
    }
    pub fn use_device_timing(&self) -> bool {
        self.use_device_timing
    }
    pub fn exclude_own_audio(&self) -> bool {
        self.exclude_own_audio
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.target.validate()?;
        if self.flow == AudioFlow::Input
            && (matches!(self.target, AudioTarget::MacSystem | AudioTarget::WindowsProcess { .. } | AudioTarget::WindowsWindow { .. }) || self.exclude_own_audio)
        {
            return Err(Error::Invalid(
                "system audio and own-output exclusion require output flow",
            ));
        }
        if matches!(self.target, AudioTarget::WindowsProcess { .. } | AudioTarget::WindowsWindow { .. }) && self.exclude_own_audio {
            return Err(Error::Invalid("process audio captures the selected process tree; own-output exclusion is unsupported"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSlot {
    Desktop1,
    Desktop2,
    Mic1,
    Mic2,
    Mic3,
    Mic4,
}
impl AudioSlot {
    pub fn flow(self) -> AudioFlow {
        match self {
            Self::Desktop1 | Self::Desktop2 => AudioFlow::Output,
            _ => AudioFlow::Input,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalAudioSource {
    slot: AudioSlot,
    source: Id,
}
impl GlobalAudioSource {
    pub(crate) fn new(slot: AudioSlot, source: Id) -> Self {
        Self { slot, source }
    }
    pub fn slot(&self) -> AudioSlot {
        self.slot
    }
    pub fn source(&self) -> Id {
        self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Session, SourceKind, Transform, TransformAction};
    use std::sync::Arc;
    fn input() -> AudioCaptureBinding {
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap()
    }
    fn output() -> AudioCaptureBinding {
        AudioCaptureBinding::new(AudioFlow::Output, AudioTarget::Default, true, false).unwrap()
    }
    fn global(
        session: &mut Session,
        slot: AudioSlot,
        name: &str,
        binding: AudioCaptureBinding,
    ) -> Id {
        session
            .apply(Command::AddGlobalAudioCaptureSource {
                slot,
                name: name.into(),
                binding,
            })
            .unwrap()
            .created
            .unwrap()
    }
    #[test]
    fn binding_has_bounded_exact_identities_and_validates_only_offline_semantics() {
        let mut id = String::with_capacity(4096);
        id.push_str("Endpoint:{A-b}:мікрофон");
        let expected = id.clone();
        let binding = AudioCaptureBinding::new(
            AudioFlow::Input,
            AudioTarget::WindowsEndpoint { id },
            true,
            false,
        )
        .unwrap();
        assert_eq!(binding.flow(), AudioFlow::Input);
        assert!(binding.use_device_timing());
        assert!(!binding.exclude_own_audio());
        assert_eq!(
            binding.target(),
            &AudioTarget::WindowsEndpoint { id: expected }
        );
        let AudioTarget::WindowsEndpoint { id } = binding.target() else {
            panic!()
        };
        assert_eq!(id.capacity(), id.len());
        let mut uid = String::with_capacity(4096);
        uid.push_str("aggregate:Device-А");
        let binding = AudioCaptureBinding::new(
            AudioFlow::Output,
            AudioTarget::MacDevice { uid },
            false,
            true,
        )
        .unwrap();
        let AudioTarget::MacDevice { uid } = binding.target() else {
            panic!()
        };
        assert_eq!(uid, "aggregate:Device-А");
        assert_eq!(uid.capacity(), uid.len());
        assert!(binding.exclude_own_audio());
        assert!(
            AudioCaptureBinding::new(AudioFlow::Output, AudioTarget::MacSystem, true, true).is_ok()
        );
        assert!(
            AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::MacSystem, false, false)
                .is_err()
        );
        assert!(
            AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, true).is_err()
        );
        for target in [
            AudioTarget::WindowsEndpoint { id: "".into() },
            AudioTarget::WindowsEndpoint { id: "a\0b".into() },
            AudioTarget::MacDevice { uid: " \t".into() },
            AudioTarget::MacDevice {
                uid: "a".repeat(1025),
            },
        ] {
            assert!(AudioCaptureBinding::new(AudioFlow::Output, target, true, false).is_err());
        }
        assert!(
            AudioCaptureBinding::new(
                AudioFlow::Input,
                AudioTarget::MacDevice {
                    uid: "a".repeat(1024)
                },
                false,
                false
            )
            .is_ok()
        );
    }
    #[test]
    fn scene_audio_has_no_video_and_rebinding_preserves_one_shared_gain_mute_owner() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Mic".into(),
                binding: input(),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        session
            .apply(Command::AddExistingSource { scene, source })
            .unwrap();
        let before = session.snapshot();
        assert_eq!(
            before.source(source).unwrap().kind().video_dimensions(),
            None
        );
        assert_eq!(before.source(source).unwrap().kind().dimensions(), (0, 0));
        assert!(
            before
                .scene(scene)
                .unwrap()
                .items()
                .iter()
                .all(|item| item.transform() == &Transform::full(1, 1))
        );
        assert!(
            !session
                .apply(Command::RebindAudioCaptureSource {
                    source,
                    binding: input()
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        for command in [
            Command::SetTransform {
                scene,
                item,
                transform: Transform::full(1, 1),
            },
            Command::TransformItem {
                scene,
                item,
                action: TransformAction::Reset,
            },
            Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::FitToCanvas,
                frame_width: 16,
                frame_height: 16,
            },
        ] {
            assert!(session.apply(command).is_err());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.5,
                muted: true,
            })
            .unwrap();
        let replacement = AudioCaptureBinding::new(
            AudioFlow::Input,
            AudioTarget::MacDevice {
                uid: "device-two".into(),
            },
            true,
            false,
        )
        .unwrap();
        session
            .apply(Command::RebindAudioCaptureSource {
                source,
                binding: replacement.clone(),
            })
            .unwrap();
        let after = session.snapshot();
        assert_eq!(after.sources().len(), 1);
        assert_eq!(
            after.source(source).unwrap().kind(),
            &SourceKind::AudioCapture {
                binding: replacement
            }
        );
        assert_eq!(
            (
                after.source(source).unwrap().gain(),
                after.source(source).unwrap().muted()
            ),
            (0.5, true)
        );
        assert!(
            after
                .scene(scene)
                .unwrap()
                .items()
                .iter()
                .all(|item| item.transform() == &Transform::full(1, 1))
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            &SourceKind::AudioCapture { binding: input() }
        );
        assert_eq!(session.snapshot().source(source).unwrap().gain(), 0.5);
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            after.source(source).unwrap().kind()
        );
    }
    #[test]
    fn scene_and_global_roots_share_source_state_and_prune_only_after_the_final_reference() {
        let mut session = Session::new();
        let scene = session.preview();
        let source = global(&mut session, AudioSlot::Desktop1, "Desktop", output());
        assert!(session.snapshot().scene(scene).unwrap().items().is_empty());
        assert_eq!(session.snapshot().sources().len(), 1);
        assert_eq!(
            (
                session.snapshot().global_audio()[0].slot(),
                session.snapshot().global_audio()[0].source()
            ),
            (AudioSlot::Desktop1, source)
        );
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.75,
                muted: true,
            })
            .unwrap();
        let item = session
            .apply(Command::AddExistingSource { scene, source })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::SetGlobalAudio {
                slot: AudioSlot::Desktop2,
                source: Some(source),
            })
            .unwrap();
        assert_eq!(session.snapshot().sources().len(), 1);
        session
            .apply(Command::SetGlobalAudio {
                slot: AudioSlot::Desktop1,
                source: None,
            })
            .unwrap();
        session.apply(Command::RemoveItem { scene, item }).unwrap();
        assert!(session.snapshot().source(source).is_some());
        session
            .apply(Command::SetGlobalAudio {
                slot: AudioSlot::Desktop2,
                source: None,
            })
            .unwrap();
        assert!(session.snapshot().source(source).is_none());
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().source(source).unwrap().gain(), 0.75);
        assert!(session.snapshot().source(source).unwrap().muted());
        assert_eq!(session.snapshot().global_audio()[0].source(), source);
        session.apply(Command::Redo).unwrap();
        assert!(session.snapshot().sources().is_empty());
        let before = session.snapshot();
        assert!(
            !session
                .apply(Command::SetGlobalAudio {
                    slot: AudioSlot::Desktop2,
                    source: None
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
    }
    #[test]
    fn wrong_flows_non_audio_targets_and_dangling_global_refs_roll_back_atomically() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Mic".into(),
                binding: input(),
            })
            .unwrap();
        let mic = session.snapshot().sources()[0].id();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Color".into(),
                rgba: [255; 4],
            })
            .unwrap();
        let color = session.snapshot().sources()[1].id();
        let before = session.snapshot();
        for command in [
            Command::SetGlobalAudio {
                slot: AudioSlot::Desktop1,
                source: Some(mic),
            },
            Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(color),
            },
            Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(Id::new(u64::MAX)),
            },
            Command::AddGlobalAudioCaptureSource {
                slot: AudioSlot::Desktop1,
                name: "Wrong".into(),
                binding: input(),
            },
            Command::RebindAudioCaptureSource {
                source: color,
                binding: input(),
            },
        ] {
            assert!(session.apply(command).is_err());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        session
            .apply(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(mic),
            })
            .unwrap();
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::RebindAudioCaptureSource {
                    source: mic,
                    binding: output()
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert!(session.snapshot().global_audio().is_empty());
        let mut invalid = (*before).clone();
        invalid
            .global_audio
            .push(GlobalAudioSource::new(AudioSlot::Mic1, mic));
        assert!(invalid.validate().is_err());
        let mut invalid = (*before).clone();
        invalid.global_audio = vec![GlobalAudioSource::new(AudioSlot::Mic2, Id::new(u64::MAX))];
        assert!(invalid.validate().is_err());
        let mut invalid = (*before).clone();
        invalid.global_audio.clear();
        invalid.scenes[0].items.retain(|item| item.source() != mic);
        assert!(
            invalid.validate().is_err(),
            "audio source cannot become an unused orphan"
        );
        let mut invalid = (*before).clone();
        invalid.scenes[0].items[0].transform.width = 2.0;
        assert!(
            invalid.validate().is_err(),
            "nonvisual storage cannot masquerade as a visual item"
        );
    }
    #[test]
    fn replacing_global_sources_keeps_scene_owners_and_issued_ids_survive_undo() {
        let mut session = Session::new();
        let scene = session.preview();
        let first = global(&mut session, AudioSlot::Mic1, "First", input());
        let second = global(&mut session, AudioSlot::Mic1, "Second", input());
        assert!(session.snapshot().source(first).is_none());
        assert_eq!(session.snapshot().sources().len(), 1);
        let item = session
            .apply(Command::AddExistingSource {
                scene,
                source: second,
            })
            .unwrap()
            .created
            .unwrap();
        let third = global(&mut session, AudioSlot::Mic1, "Third", input());
        assert!(session.snapshot().source(second).is_some());
        assert!(session.snapshot().source(third).is_some());
        session.apply(Command::RemoveItem { scene, item }).unwrap();
        assert!(session.snapshot().source(second).is_none());
        session.apply(Command::Undo).unwrap();
        assert!(session.snapshot().source(second).is_some());
        let mut branch = Session::new();
        let first = global(&mut branch, AudioSlot::Mic1, "First", input());
        branch.apply(Command::Undo).unwrap();
        let next = global(&mut branch, AudioSlot::Mic2, "Next", input());
        assert!(next.get() > first.get());
        assert!(branch.snapshot().source(first).is_none());
    }
    #[test]
    fn six_typed_slots_default_empty_and_preserve_no_op_snapshots() {
        let mut session = Session::new();
        assert!(session.snapshot().global_audio().is_empty());
        assert!(session.snapshot().validate().is_ok());
        for slot in [
            AudioSlot::Desktop1,
            AudioSlot::Desktop2,
            AudioSlot::Mic1,
            AudioSlot::Mic2,
            AudioSlot::Mic3,
            AudioSlot::Mic4,
        ] {
            let binding = if slot.flow() == AudioFlow::Input {
                input()
            } else {
                output()
            };
            global(&mut session, slot, &format!("{slot:?}"), binding);
        }
        let before = session.snapshot();
        assert_eq!(before.sources().len(), 6);
        assert_eq!(before.global_audio().len(), 6);
        assert!(before.scenes()[0].items().is_empty());
        assert!(before.validate().is_ok());
        let root = before.global_audio()[0];
        assert!(
            !session
                .apply(Command::SetGlobalAudio {
                    slot: root.slot(),
                    source: Some(root.source())
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let mut invalid = (*before).clone();
        invalid.global_audio.push(root);
        assert!(matches!(invalid.validate(), Err(Error::Limit(_))));
    }
}
