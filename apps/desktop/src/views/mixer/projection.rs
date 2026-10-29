//! Visibility/order are UI projections, independent of native capture success.
use super::*;

pub(super) struct Row<'a> {
    pub source: &'a Source,
    pub editable: bool,
    pub global: bool,
    pub active: bool,
    pub preview: bool,
    pub pinned: bool,
    pub hidden: bool,
}
impl Row<'_> {
    pub fn category(&self) -> &'static str {
        if !self.editable {
            "Retired Program source"
        } else if self.global {
            "Basic.AudioMixer.Category.Global"
        } else if self.pinned {
            "Basic.AudioMixer.Category.Pinned"
        } else if self.hidden {
            "Basic.AudioMixer.Category.Hidden"
        } else if !self.active && self.preview {
            "Basic.AudioMixer.Category.Preview"
        } else if !self.active {
            "Basic.AudioMixer.Category.Inactive"
        } else {
            "Basic.AudioMixer.Category.Active"
        }
    }
}
pub(super) fn rows<'a>(
    model: &'a StudioSnapshot,
    session: u64,
    program: &'a [FrameScene],
    preview: Option<Id>,
) -> Vec<Row<'a>> {
    let globals: HashSet<_> = model
        .global_audio()
        .iter()
        .map(|slot| slot.source())
        .collect();
    let mut used = HashSet::new();
    let mut retained = Vec::new();
    for view in program {
        if view.validate_session(session).is_err() {
            continue;
        }
        if let Ok(sources) = view.visible_sources(model) {
            for source in sources {
                if source.kind().has_audio() {
                    used.insert(source.id());
                    if model.source(source.id()).is_none()
                        && !retained
                            .iter()
                            .any(|existing: &&Source| existing.id() == source.id())
                    {
                        retained.push(source);
                    }
                }
            }
        }
    }
    let previewed: HashSet<_> = preview
        .map(|scene| {
            let view = FrameScene::Live(scene);
            view.visible_sources(model).map(|sources| sources.iter().map(|source|source.id()).collect()).unwrap_or_default()
        })
        .unwrap_or_default();
    let prefs = model.preferences().mixer_display();
    let mut result = Vec::new();
    for source in model
        .sources()
        .iter()
        .filter(|source| source.kind().has_audio())
        .chain(retained)
    {
        let global = globals.contains(&source.id());
        let flags = source.mixer_state();
        // Global assignment is a role, never a destructive rewrite of saved flags.
        let hidden = flags.hidden() && !global;
        let pinned = flags.pinned() && !global;
        let active = global || used.contains(&source.id());
        let preview = previewed.contains(&source.id());
        let visible = global
            || pinned
            || hidden && prefs.show_hidden()
            || !hidden && (active || preview || prefs.show_inactive());
        if visible {
            result.push(Row {
                source,
                editable: model.source(source.id()).is_some(),
                global,
                active,
                preview,
                pinned,
                hidden,
            });
        }
    }
    result.sort_by_cached_key(|row| {
        let mut weight = if row.global {
            0
        } else if row.pinned {
            20
        } else {
            40
        };
        if row.hidden && prefs.keep_hidden_last() {
            weight += 5;
            if row.preview {
                weight -= 1;
            }
        }
        if !row.active && prefs.keep_inactive_last() {
            weight += 5;
            if row.preview {
                weight -= 1;
            }
        }
        (
            weight,
            row.source.name().to_lowercase(),
            row.source.id().get(),
        )
    });
    result
}
