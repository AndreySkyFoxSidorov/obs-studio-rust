//! Bounded presentation-only discovery; callers retain actual filter factories.
use eframe::egui;

#[derive(Clone, Copy)]
pub(super) struct Choice<'a> {
    pub label: &'a str,
    pub hint: &'a str,
    pub enabled: bool,
    pub disabled_hint: Option<&'a str>,
    pub group: Option<&'a str>,
}

pub(super) fn show(ui: &mut egui::Ui, choices: &[Choice<'_>]) -> Option<usize> {
    if choices.len() > 64 {
        ui.label(crate::i18n::tr(ui, "Too many filter choices. Close and reopen this menu."));
        return None;
    }
    let query_id = ui.id().with("filter-add-query");
    let disabled_id = ui.id().with("filter-add-disabled");
    let mut query = ui.data_mut(|data| data.get_temp::<String>(query_id)).unwrap_or_default();
    let mut show_disabled = ui.data_mut(|data| data.get_temp::<bool>(disabled_id)).unwrap_or(false);
    // The popup may be narrower than the full editor at large language scales.
    ui.set_max_width(ui.available_width().min(440.0));
    let label = ui.label(crate::i18n::tr(ui, "Search filters"));
    ui.add(egui::TextEdit::singleline(&mut query)
        .id_salt("filter-add-search")
        .char_limit(128)
        .desired_width(ui.available_width()))
        .labelled_by(label.id)
        .on_hover_text(crate::i18n::tr(ui, "Find a filter by its translated name, original name or description."));
    ui.checkbox(&mut show_disabled, crate::i18n::tr(ui, "Show disabled modules"))
        .on_hover_text(crate::i18n::tr(ui, "Inspect unavailable filters. Enable their module in Tools > Plugins before adding them."));
    ui.separator();
    let search = query.trim().to_lowercase();
    let mut chosen = None;
    let mut matches = 0usize;
    let mut last_group = None;
    // Providers may append factories independently. Keep each provider together
    // while returning the ORIGINAL choice index to the typed factory owner.
    // Fixed storage covers the already-admitted64 choices without another list.
    let mut order=[0usize;64];
    let mut ordered=0;
    for (first,anchor) in choices.iter().enumerate(){
        if choices[..first].iter().any(|previous|previous.group==anchor.group){continue;}
        for (index,choice) in choices.iter().enumerate(){
            if choice.group==anchor.group{order[ordered]=index;ordered+=1;}
        }
    }
    let height = ui.available_height().clamp(48.0, 360.0);
    egui::ScrollArea::vertical().id_salt("filter-add-results").max_height(height).show(ui, |ui| {
        for &index in &order[..ordered] {
            let choice=&choices[index];
            if !choice.enabled && !show_disabled {
                continue;
            }
            let text = if choice.label=="Add Craft Dither" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.Add",choice.label)} else if choice.label=="Add Craft Rectifier" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Rectifier.Add",choice.label)} else if choice.label=="Add Craft DC Removal" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dc.Add",choice.label)} else if choice.label=="Add Craft Lo-Fi" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.LoFi.Add",choice.label)} else if choice.label=="Add Craft Saturator" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Saturator.Add",choice.label)} else if choice.label=="Gamma/Pedestal/Gain (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Gpg",choice.label)} else if choice.label=="Channel Mixer (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.ChannelMixer",choice.label)} else if choice.label=="Color Balance (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.ColorBalance",choice.label)} else if choice.label=="Tritone (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Tritone",choice.label)} else if choice.label=="Tint (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Tint",choice.label)} else if choice.label=="Vibrance" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Vibrance",choice.label)} else {crate::i18n::tr(ui, choice.label)};
            let hint = if choice.label=="Add Craft Dither" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dither.Hint",choice.hint)} else if choice.label=="Add Craft Rectifier" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Rectifier.Hint",choice.hint)} else if choice.label=="Add Craft DC Removal" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Dc.Hint",choice.hint)} else if choice.label=="Add Craft Lo-Fi" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.LoFi.Hint",choice.hint)} else if choice.label=="Add Craft Saturator" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.Saturator.Hint",choice.hint)} else if choice.label=="Video Delay (Async)" {crate::i18n::tr_key_or(ui,"AsyncVideoDelay.FactoryHint",choice.hint)} else if choice.label=="Gamma/Pedestal/Gain (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.GpgHint",choice.hint)} else if choice.label=="Sepia (Photo Filter)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.SepiaFactoryHint",choice.hint)} else if choice.label=="Photo Filter" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.PhotoFilterFactoryHint",choice.hint)} else if choice.label=="Channel Mixer (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.ChannelMixerHint",choice.hint)} else if choice.label=="Color Balance (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.ColorBalanceHint",choice.hint)} else if choice.label=="Tritone (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.TritoneFactoryHint",choice.hint)} else if choice.label=="Tint (EffectCraft)" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.TintFactoryHint",choice.hint)} else if choice.label=="Vibrance" {crate::i18n::tr_key_or(ui,"SkyLive.Craft.VibranceFactoryHint",choice.hint)} else {crate::i18n::tr(ui, choice.hint)};
            let group = choice.group.map(|value| crate::i18n::tr(ui, value));
            if !search.is_empty() && ![text.as_str(), hint.as_str(), choice.label, choice.hint,
                group.as_deref().unwrap_or(""), choice.group.unwrap_or("")]
                .iter().any(|value| value.to_lowercase().contains(&search)) {
                continue;
            }
            if choice.group != last_group {
                if let Some(group) = group {
                    if matches > 0 { ui.separator(); }
                    ui.strong(group);
                }
                last_group = choice.group;
            }
            matches += 1;
            let response = ui.push_id(index, |ui| {
                ui.add_enabled(choice.enabled, egui::Button::new(text).wrap()
                    .min_size(egui::vec2(ui.available_width(), 28.0)))
                    .on_hover_text(hint)
                    .on_disabled_hover_text(match choice.disabled_hint {
                        Some(value)=>crate::i18n::tr_key_or(ui,"AsyncVideoDelay.SourceUnavailable",value),
                        None=>crate::i18n::tr(ui,"Enable this filter's module in Tools > Plugins before adding it."),
                    })
            }).inner;
            if response.clicked() {
                chosen = Some(index);
                ui.close_menu();
                break;
            }
        }
        if matches == 0 {
            ui.label(crate::i18n::tr(ui, "No filters match this search."));
        }
    });
    if chosen.is_some() {
        query.clear();
    }
    ui.data_mut(|data| {
        data.insert_temp(query_id, query);
        data.insert_temp(disabled_id, show_disabled);
    });
    chosen
}
