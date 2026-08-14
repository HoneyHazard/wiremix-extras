use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use serde::{de::Error, Deserialize};

use crate::config::Theme;

// This is what actually gets parsed from the config.
#[derive(Deserialize, Debug)]
pub struct ThemeOverlay {
    inherit: Option<String>,
    default_device: Option<StyleDef>,
    default_stream: Option<StyleDef>,
    selector: Option<StyleDef>,
    tab: Option<StyleDef>,
    tab_selected: Option<StyleDef>,
    tab_marker: Option<StyleDef>,
    list_more: Option<StyleDef>,
    divider: Option<StyleDef>,
    node_title: Option<StyleDef>,
    node_target: Option<StyleDef>,
    volume: Option<StyleDef>,
    volume_empty: Option<StyleDef>,
    volume_filled: Option<StyleDef>,
    meter_inactive: Option<StyleDef>,
    meter_inactive_overload: Option<StyleDef>,
    meter_active: Option<StyleDef>,
    meter_overload: Option<StyleDef>,
    meter_center_inactive: Option<StyleDef>,
    meter_center_active: Option<StyleDef>,
    meter_split_inactive: Option<StyleDef>,
    meter_split_active: Option<StyleDef>,
    meter_split_overload: Option<StyleDef>,
    meter_split_center_inactive: Option<StyleDef>,
    meter_split_center_active: Option<StyleDef>,
    config_device: Option<StyleDef>,
    config_profile: Option<StyleDef>,
    row_hidden: Option<StyleDef>,
    // Whole-row overlays: span everything above, from node_title/config_device
    // through config_profile, in both the node list and the Configuration tab.
    row_selected: Option<StyleDef>,
    row_unselected: Option<StyleDef>,
    dropdown_icon: Option<StyleDef>,
    dropdown_border: Option<StyleDef>,
    dropdown_item: Option<StyleDef>,
    dropdown_selected: Option<StyleDef>,
    dropdown_more: Option<StyleDef>,
    help_border: Option<StyleDef>,
    help_item: Option<StyleDef>,
    help_more: Option<StyleDef>,
}

#[derive(Deserialize, Debug)]
struct StyleDef {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub underline_color: Option<Color>,
    #[serde(default = "default_modifier")]
    pub add_modifier: Modifier,
    #[serde(default = "default_modifier")]
    pub sub_modifier: Modifier,
}

fn default_modifier() -> Modifier {
    Modifier::empty()
}

impl From<StyleDef> for Style {
    fn from(def: StyleDef) -> Self {
        Self {
            fg: def.fg,
            bg: def.bg,
            underline_color: def.underline_color,
            add_modifier: def.add_modifier,
            sub_modifier: def.sub_modifier,
        }
    }
}

impl TryFrom<ThemeOverlay> for Theme {
    type Error = anyhow::Error;

    fn try_from(overlay: ThemeOverlay) -> Result<Self, Self::Error> {
        let mut theme: Self = match overlay.inherit.as_deref() {
            Some("default") => Theme::default(),
            Some("nocolor") => Theme::nocolor(),
            Some("plain") => Theme::plain(),
            Some("redshift") => Theme::redshift(),
            Some(inherit) => {
                anyhow::bail!("'{}' is not a built-in theme", inherit)
            }
            None => Theme::default(),
        };

        macro_rules! set {
            ($field:ident) => {
                if let Some($field) = overlay.$field {
                    theme.$field = $field.into();
                }
            };
        }

        // Same as `set!`, but for the optional `meter_split_*` fields,
        // whose unset state (`None`) is itself meaningful (falls back to
        // the corresponding stock `meter_*` field at render time) rather
        // than just "use the built-in default".
        macro_rules! set_optional {
            ($field:ident) => {
                if let Some($field) = overlay.$field {
                    theme.$field = Some($field.into());
                }
            };
        }

        set!(default_device);
        set!(default_stream);
        set!(selector);
        set!(tab);
        set!(tab_selected);
        set!(tab_marker);
        set!(list_more);
        set!(divider);
        set!(node_title);
        set!(node_target);
        set!(volume);
        set!(volume_empty);
        set!(volume_filled);
        set!(meter_inactive);
        set!(meter_inactive_overload);
        set!(meter_active);
        set!(meter_overload);
        set!(meter_center_inactive);
        set!(meter_center_active);
        set_optional!(meter_split_inactive);
        set_optional!(meter_split_active);
        set_optional!(meter_split_overload);
        set_optional!(meter_split_center_inactive);
        set_optional!(meter_split_center_active);
        set!(config_device);
        set!(config_profile);
        set!(row_hidden);
        set!(row_selected);
        set!(row_unselected);
        set!(dropdown_icon);
        set!(dropdown_border);
        set!(dropdown_item);
        set!(dropdown_selected);
        set!(dropdown_more);
        set!(help_border);
        set!(help_item);
        set!(help_more);

        Ok(theme)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            default_device: Style::default(),
            default_stream: Style::default(),
            selector: Style::default().fg(Color::LightCyan),
            tab: Style::default(),
            tab_selected: Style::default().fg(Color::LightCyan),
            tab_marker: Style::default().fg(Color::LightCyan),
            list_more: Style::default().fg(Color::DarkGray),
            divider: Style::default().fg(Color::DarkGray),
            node_title: Style::default(),
            node_target: Style::default(),
            volume: Style::default(),
            volume_empty: Style::default().fg(Color::DarkGray),
            volume_filled: Style::default().fg(Color::LightBlue),
            // A dim shade of the same hue meter_active/meter_overload use
            // in their own zone, rather than a flat DarkGray - previews
            // which zone each not-yet-lit position belongs to (green vs.
            // red) the way a physical VU meter's unlit red zone still
            // reads as "red," just dark, even with the needle elsewhere.
            meter_inactive: Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::DIM),
            meter_inactive_overload: Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::DIM),
            meter_active: Style::default().fg(Color::LightGreen),
            meter_overload: Style::default().fg(Color::Red),
            meter_center_inactive: Style::default().fg(Color::DarkGray),
            meter_center_active: Style::default().fg(Color::LightGreen),
            meter_split_inactive: None,
            meter_split_active: None,
            meter_split_overload: None,
            meter_split_center_inactive: None,
            meter_split_center_active: None,
            config_device: Style::default(),
            config_profile: Style::default(),
            row_hidden: Style::default().fg(Color::DarkGray),
            row_selected: Style::default(),
            row_unselected: Style::default(),
            dropdown_icon: Style::default(),
            dropdown_border: Style::default(),
            dropdown_item: Style::default(),
            dropdown_selected: Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::REVERSED),
            dropdown_more: Style::default().fg(Color::DarkGray),
            help_border: Style::default(),
            help_item: Style::default(),
            help_more: Style::default().fg(Color::DarkGray),
        }
    }
}

impl Theme {
    pub fn defaults() -> HashMap<String, Theme> {
        HashMap::from([
            (String::from("default"), Theme::default()),
            (String::from("nocolor"), Theme::nocolor()),
            (String::from("plain"), Theme::plain()),
            (String::from("redshift"), Theme::redshift()),
        ])
    }

    fn nocolor() -> Self {
        Self {
            default_device: Style::default(),
            default_stream: Style::default(),
            selector: Style::default().add_modifier(Modifier::BOLD),
            tab: Style::default(),
            tab_selected: Style::default().add_modifier(Modifier::BOLD),
            tab_marker: Style::default().add_modifier(Modifier::BOLD),
            list_more: Style::default(),
            divider: Style::default(),
            node_title: Style::default(),
            node_target: Style::default(),
            volume: Style::default(),
            volume_empty: Style::default().add_modifier(Modifier::DIM),
            volume_filled: Style::default().add_modifier(Modifier::BOLD),
            meter_inactive: Style::default().add_modifier(Modifier::DIM),
            meter_inactive_overload: Style::default()
                .add_modifier(Modifier::DIM),
            meter_active: Style::default().add_modifier(Modifier::BOLD),
            meter_overload: Style::default().add_modifier(Modifier::BOLD),
            meter_center_inactive: Style::default().add_modifier(Modifier::DIM),
            meter_center_active: Style::default().add_modifier(Modifier::BOLD),
            meter_split_inactive: None,
            meter_split_active: None,
            meter_split_overload: None,
            meter_split_center_inactive: None,
            meter_split_center_active: None,
            config_device: Style::default(),
            config_profile: Style::default(),
            row_hidden: Style::default().add_modifier(Modifier::DIM),
            row_selected: Style::default(),
            row_unselected: Style::default(),
            dropdown_icon: Style::default(),
            dropdown_border: Style::default(),
            dropdown_item: Style::default(),
            dropdown_selected: Style::default()
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            dropdown_more: Style::default(),
            help_border: Style::default(),
            help_item: Style::default(),
            help_more: Style::default(),
        }
    }

    fn plain() -> Self {
        Self {
            default_device: Style::default(),
            default_stream: Style::default(),
            selector: Style::default(),
            tab: Style::default(),
            tab_selected: Style::default(),
            tab_marker: Style::default(),
            list_more: Style::default(),
            divider: Style::default(),
            node_title: Style::default(),
            node_target: Style::default(),
            volume: Style::default(),
            volume_empty: Style::default(),
            volume_filled: Style::default(),
            meter_inactive: Style::default(),
            meter_inactive_overload: Style::default(),
            meter_active: Style::default(),
            meter_overload: Style::default(),
            meter_center_inactive: Style::default(),
            meter_center_active: Style::default(),
            meter_split_inactive: None,
            meter_split_active: None,
            meter_split_overload: None,
            meter_split_center_inactive: None,
            meter_split_center_active: None,
            config_device: Style::default(),
            config_profile: Style::default(),
            row_hidden: Style::default(),
            row_selected: Style::default(),
            row_unselected: Style::default(),
            dropdown_icon: Style::default(),
            dropdown_border: Style::default(),
            dropdown_item: Style::default(),
            dropdown_selected: Style::default(),
            dropdown_more: Style::default(),
            help_border: Style::default(),
            help_item: Style::default(),
            help_more: Style::default(),
        }
    }

    /// A warm, low-blue dark theme - built for use under strong blue-light
    /// filtering (redshift, gammastep, f.lux, night-light) at warm color
    /// temperatures (~2700K-3400K), where the blue channel is cut by
    /// roughly half and green is dimmed slightly while red is left
    /// untouched or even boosted. Semantic meaning is remapped onto the
    /// red -> orange -> amber -> yellow -> yellow-green ramp, which
    /// keeps the most contrast headroom under that kind of filter.
    /// Inherits from `default()` for anything not overridden below.
    fn redshift() -> Self {
        Self {
            selector: Style::default()
                .fg(Color::Rgb(0xFF, 0xD3, 0x4D))
                .add_modifier(Modifier::BOLD),
            tab_selected: Style::default()
                .fg(Color::Rgb(0xFF, 0xD3, 0x4D))
                .bg(Color::Rgb(0x3A, 0x2A, 0x00))
                .add_modifier(Modifier::BOLD),
            tab_marker: Style::default()
                .fg(Color::Rgb(0xFF, 0xD3, 0x4D))
                .bg(Color::Rgb(0x3A, 0x2A, 0x00)),
            default_device: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            default_stream: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            volume_filled: Style::default().fg(Color::Rgb(0xFF, 0x96, 0x40)),
            volume_empty: Style::default().fg(Color::DarkGray),
            meter_active: Style::default().fg(Color::Rgb(0x9A, 0xCD, 0x32)),
            meter_center_active: Style::default()
                .fg(Color::Rgb(0x9A, 0xCD, 0x32))
                .add_modifier(Modifier::BOLD),
            meter_overload: Style::default()
                .fg(Color::Rgb(0xFF, 0x3B, 0x30))
                .add_modifier(Modifier::BOLD),
            meter_inactive: Style::default().fg(Color::DarkGray),
            meter_center_inactive: Style::default().fg(Color::DarkGray),
            row_selected: Style::default()
                .fg(Color::Rgb(0xFF, 0xD3, 0x4D))
                .bg(Color::Rgb(0x3A, 0x14, 0x14))
                .add_modifier(Modifier::BOLD),
            row_unselected: Style::default().bg(Color::Rgb(0x1F, 0x0D, 0x0D)),
            dropdown_selected: Style::default()
                .fg(Color::Rgb(0xFF, 0xD3, 0x4D))
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            dropdown_border: Style::default().fg(Color::DarkGray),
            list_more: Style::default().fg(Color::DarkGray),
            dropdown_more: Style::default().fg(Color::DarkGray),
            help_border: Style::default().fg(Color::DarkGray),
            help_more: Style::default().fg(Color::DarkGray),
            ..Theme::default()
        }
    }

    /// Merge deserialized themes with defaults
    pub fn merge<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<String, Theme>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let configured =
            HashMap::<String, ThemeOverlay>::deserialize(deserializer)?;
        let mut merged = configured
            .into_iter()
            .map(|(key, value)| {
                Theme::try_from(value)
                    .map_err(D::Error::custom)
                    .map(move |theme| (key, theme))
            })
            .collect::<Result<HashMap<String, Theme>, D::Error>>()?;
        if !merged.contains_key("default") {
            merged.insert(String::from("default"), Theme::default());
        }
        if !merged.contains_key("nocolor") {
            merged.insert(String::from("nocolor"), Theme::nocolor());
        }
        if !merged.contains_key("plain") {
            merged.insert(String::from("plain"), Theme::plain());
        }
        if !merged.contains_key("redshift") {
            merged.insert(String::from("redshift"), Theme::redshift());
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherit_nonexistent() {
        let config = r#"
        inherit = "doesntexist"
        tab_selected = { }
        "#;

        let overlay = toml::from_str::<ThemeOverlay>(config).unwrap();
        let theme = Theme::try_from(overlay);
        assert!(theme.is_err());
    }

    #[test]
    fn inherit() {
        for (builtin_key, builtin) in Theme::defaults().iter() {
            let config = format!(
                r#"
            inherit = "{builtin_key}"
            tab_selected = {{ }}
            "#
            );

            let overlay = toml::from_str::<ThemeOverlay>(&config).unwrap();
            let theme = Theme::try_from(overlay).unwrap();
            assert_eq!(theme.tab_selected, Style::default());
            assert_eq!(theme.selector, builtin.selector);
        }
    }

    #[test]
    fn meter_split_colors_are_unset_by_default_and_configurable() {
        for builtin in Theme::defaults().values() {
            assert_eq!(builtin.meter_split_inactive, None);
            assert_eq!(builtin.meter_split_active, None);
            assert_eq!(builtin.meter_split_overload, None);
            assert_eq!(builtin.meter_split_center_inactive, None);
            assert_eq!(builtin.meter_split_center_active, None);
        }

        let config = r#"
        meter_split_inactive = { fg = "Black" }
        "#;
        let overlay = toml::from_str::<ThemeOverlay>(config).unwrap();
        let theme = Theme::try_from(overlay).unwrap();
        assert_eq!(
            theme.meter_split_inactive,
            Some(Style::default().fg(Color::Black))
        );
        // Only the configured field changes - everything else, including
        // the other meter_split_* fields, stays unset.
        assert_eq!(theme.meter_split_active, None);
    }
}
