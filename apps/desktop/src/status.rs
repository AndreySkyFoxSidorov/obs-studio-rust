//! Application chrome templates and opaque status arguments, never engine data.
use eframe::egui;

#[derive(Clone)]
pub(super) enum Status {
    Chrome(&'static str),
    Raw(String),
    Formatted {
        template: &'static str,
        arguments: Vec<(&'static str, String)>,
    },
}
#[derive(Default)]
pub(super) enum Presentation {
    #[default]
    Raw,
    Chrome(&'static str),
    Formatted {
        template: &'static str,
        arguments: Vec<(&'static str, String)>,
    },
}
impl From<&'static str> for Status {
    fn from(value: &'static str) -> Self {
        Self::Chrome(value)
    }
}
impl From<String> for Status {
    fn from(value: String) -> Self {
        Self::Raw(value)
    }
}
impl Status {
    pub(super) fn formatted(template: &'static str, arguments: &[(&'static str, String)]) -> Self {
        let raw = render_raw(template, arguments);
        if template.len() > 2048
            || arguments.len() > 8
            || arguments
                .iter()
                .map(|(_, value)| value.len())
                .sum::<usize>()
                > 32768
            || raw.len() > 32768
        {
            return Self::Raw(raw);
        }
        Self::Formatted {
            template,
            arguments: arguments.to_vec(),
        }
    }
    pub(super) fn into_parts(self) -> (String, Presentation) {
        match self {
            Self::Chrome(value) => (value.into(), Presentation::Chrome(value)),
            Self::Raw(value) => (value, Presentation::Raw),
            Self::Formatted {
                template,
                arguments,
            } => (
                render_raw(template, &arguments),
                Presentation::Formatted {
                    template,
                    arguments,
                },
            ),
        }
    }
}
impl Presentation {
    pub(super) fn text(&self, context: &egui::Context, raw: &str) -> String {
        match self {
            Self::Chrome(english) if raw == *english => crate::i18n::tr_context(context, english),
            Self::Formatted {
                template,
                arguments,
            } if render_raw(template, arguments) == raw => {
                crate::i18n::tf_context(context, template, arguments)
            }
            // Compatibility with direct status assignment in presentation fixtures.
            // Never translate an unrelated opaque value using stale metadata.
            _ => raw.to_owned(),
        }
    }
}
fn render_raw(template: &str, arguments: &[(&str, String)]) -> String {
    let mut used = vec![false; arguments.len()];
    let mut output = String::new();
    let mut chars = template.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch == '{' {
            if chars.peek().is_some_and(|(_, ch)| *ch == '{') {
                chars.next();
                output.push('{');
                continue;
            }
            let Some((end, _)) = chars.find(|(_, ch)| *ch == '}') else {
                return template.to_owned();
            };
            let token = &template[start..=end];
            let Some(index) = arguments
                .iter()
                .enumerate()
                .position(|(index, (key, _))| !used[index] && *key == token)
            else {
                return template.to_owned();
            };
            output.push_str(&arguments[index].1);
            used[index] = true;
        } else if ch == '}' {
            if chars.peek().is_none_or(|(_, ch)| *ch != '}') {
                return template.to_owned();
            }
            chars.next();
            output.push('}');
        } else {
            output.push(ch);
        }
    }
    if used.iter().all(|used| *used) {
        output
    } else {
        template.into()
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
