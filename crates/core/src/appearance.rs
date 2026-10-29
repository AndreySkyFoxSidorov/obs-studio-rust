//! Global UI meaning only. No egui, OS theme, font or viewport dependency.
use crate::Error;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceTheme {
    #[default]
    Yami,
    Dark,
    Acri,
    Classic,
    Rachni,
    Light,
    System,
}
impl InterfaceTheme {
    pub const ALL: [Self; 7] = [Self::Yami, Self::Dark, Self::Light, Self::Acri, Self::Classic, Self::Rachni, Self::System];
    pub fn requires_extended_theme_format(self) -> bool { matches!(self, Self::Acri | Self::Classic | Self::Rachni) }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "AppearanceData")]
pub struct Appearance {
    theme: InterfaceTheme,
    scale_percent: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppearanceData {
    theme: InterfaceTheme,
    scale_percent: u16,
}
impl TryFrom<AppearanceData> for Appearance {
    type Error = Error;
    fn try_from(value: AppearanceData) -> Result<Self, Error> {
        Self::new(value.theme, value.scale_percent)
    }
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: InterfaceTheme::Yami,
            scale_percent: 100,
        }
    }
}
impl Appearance {
    pub fn new(theme: InterfaceTheme, scale_percent: u16) -> Result<Self, Error> {
        let value = Self {
            theme,
            scale_percent,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn theme(self) -> InterfaceTheme {
        self.theme
    }
    pub fn scale_percent(self) -> u16 {
        self.scale_percent
    }
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn validate(self) -> Result<(), Error> {
        if !(75..=200).contains(&self.scale_percent) {
            return Err(Error::Invalid(
                "Interface scale must be between 75% and 200%.",
            ));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_appearance_and_default_are_valid() {
        for theme in InterfaceTheme::ALL {
            for scale in [75, 100, 125, 200] {
                let value = Appearance::new(theme, scale).unwrap();
                assert_eq!(value.theme(), theme);
                assert_eq!(value.scale_percent(), scale);
            }
        }
        for scale in [0, 74, 201, u16::MAX] {
            assert!(Appearance::new(InterfaceTheme::Yami, scale).is_err());
        }
        assert!(Appearance::default().is_default());
    }
    #[test]
    fn preference_publication_is_noop_safe_outside_document_history_and_receipts_are_exact() {
        use crate::{Command, Session};
        let mut session = Session::new();
        let initial = session.snapshot();
        let saved = (session.identity(), initial.revision());
        assert!(
            !session
                .apply(Command::SetAppearance {
                    appearance: Appearance::default()
                })
                .unwrap()
                .changed
        );
        assert!(std::sync::Arc::ptr_eq(&initial, &session.snapshot()));
        let value = Appearance::new(InterfaceTheme::Light, 175).unwrap();
        session
            .apply(Command::SetAppearance { appearance: value })
            .unwrap();
        assert_eq!(session.snapshot().preferences().appearance(), value);
        assert!(session.is_dirty());
        assert!(!session.is_document_dirty());
        assert!(!session.can_undo());
        assert_eq!(session.document_revision(), 0);
        assert!(!session.mark_saved(saved.0, saved.1));
        let changed = session.snapshot();
        assert!(
            !session
                .apply(Command::SetAppearance { appearance: value })
                .unwrap()
                .changed
        );
        assert!(std::sync::Arc::ptr_eq(&changed, &session.snapshot()));
        session
            .apply(Command::AddScene {
                name: "Scene history".into(),
            })
            .unwrap();
        let appearance = session.snapshot().preferences().appearance();
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().preferences().appearance(), appearance);
        assert_eq!(session.snapshot().scenes(), initial.scenes());
        let before = session.snapshot();
        assert!(
            session
                .apply_at(
                    Some(before.revision() - 1),
                    Command::SetAppearance {
                        appearance: Appearance::default()
                    }
                )
                .is_err()
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
    }
}
