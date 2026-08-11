//! Mixer configuration.

mod char_set;
mod filter;
mod help;
mod keybinding;
mod matching;
mod name_override;
mod name_template;
mod names;
pub mod property_key;
mod theme;

use std::collections::HashMap;
use std::convert::TryFrom;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{style::Style, widgets::block::BorderType};
use serde::Deserialize;
use toml;

use crate::app::Action;
pub use crate::config::matching::MatchCondition;
use crate::opt::Opt;

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Config {
    pub remote: Option<String>,
    pub fps: Option<f32>,
    pub mouse: bool,
    pub peaks: Peaks,
    pub char_set: CharSet,
    pub theme: Theme,
    pub max_volume_percent: f32,
    pub enforce_max_volume: bool,
    pub keybindings: HashMap<KeyEvent, Action>,
    pub help: help::Help,
    pub names: Names,
    pub tab: usize,
    pub tabs: Vec<TabKind>,
    pub lazy_capture: bool,
    /// Whether a node's volume is ever shown as more than one bar/row when
    /// its setting is linked (ganged) - "unified" (always one) or
    /// "always" (always split, per `split_style`). See `unified_imbalance`
    /// for how an imbalanced node is indicated while this is "unified".
    /// Independent of `channel_mode`, which always forces a split,
    /// individually-cursored display regardless of this setting.
    pub channel_display: ChannelDisplay,
    /// Only consulted when `channel_display` is "unified": how an
    /// imbalanced node (channels that don't all hold the same value) is
    /// indicated without actually splitting the whole list's display.
    pub unified_imbalance: UnifiedImbalance,
    /// Rendering style whenever a node's volume actually is split
    /// (`channel_display = "always"`, `unified_imbalance = "split"`
    /// triggering for one imbalanced node, or `channel_mode` being on).
    /// "radiating" renders a lone simple pair on one fixed-height row;
    /// anything with more channels (extra singles alongside a pair, more
    /// than one pair, or no pair at all) gets one row per detected
    /// pair/channel instead, each pair still radiating on its own row.
    pub split_style: SplitStyle,
    /// Initial value of the "Channel mode" setting axis (linked vs
    /// individual - see `Action::ToggleChannelMode`). Independent of
    /// `channel_display`/`unified_imbalance`/`split_style`, which
    /// control *display*, not which channels an adjustment affects.
    pub channel_mode: bool,
    /// How a radiating pair row (split_style = "radiating") labels which
    /// physical pair it's showing - only matters once more than one row
    /// can appear in the same node's split display (a lone pair takes
    /// the classic unlabeled single-row fast path instead). "verbose"
    /// spells out that it's a pair ("F L/R"); "short" is just the group
    /// name ("F").
    pub pair_label_style: PairLabelStyle,
    pub filters: Vec<MatchCondition>,
}

/// Represents a configuration deserialized from a file. This gets baked into a
/// Config, which, for example, has a single char_set and theme.
#[derive(Deserialize, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    remote: Option<String>,
    #[serde(default = "default_fps")]
    fps: Option<f32>,
    #[serde(default = "default_mouse")]
    mouse: bool,
    #[serde(default = "default_peaks")]
    peaks: Option<Peaks>,
    #[serde(default = "default_char_set_name")]
    char_set: String,
    #[serde(default = "default_theme_name")]
    theme: String,
    #[serde(default = "default_max_volume_percent")]
    max_volume_percent: Option<f32>,
    #[serde(default = "default_enforce_max_volume")]
    enforce_max_volume: bool,
    #[serde(
        default = "Keybinding::defaults",
        deserialize_with = "Keybinding::merge"
    )]
    keybindings: HashMap<KeyEvent, Action>,
    #[serde(default)]
    names: Names,
    #[serde(
        default = "CharSet::defaults",
        deserialize_with = "CharSet::merge"
    )]
    char_sets: HashMap<String, CharSet>,
    #[serde(default = "Theme::defaults", deserialize_with = "Theme::merge")]
    themes: HashMap<String, Theme>,
    #[serde(default = "default_tab")]
    tab: Option<TabKind>,
    #[serde(default = "default_tabs")]
    tabs: Vec<TabKind>,
    #[serde(default = "default_lazy_capture")]
    lazy_capture: bool,
    #[serde(default = "default_channel_display")]
    channel_display: Option<ChannelDisplay>,
    #[serde(default = "default_unified_imbalance")]
    unified_imbalance: Option<UnifiedImbalance>,
    #[serde(default = "default_split_style")]
    split_style: Option<SplitStyle>,
    #[serde(default = "default_channel_mode")]
    channel_mode: bool,
    #[serde(default = "default_pair_label_style")]
    pair_label_style: Option<PairLabelStyle>,
    #[serde(default = "Filter::defaults", deserialize_with = "Filter::merge")]
    filters: Vec<Filter>,
}

#[derive(Deserialize, Default, Debug, Clone, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Peaks {
    Off,
    Mono,
    #[default]
    Auto,
}

#[derive(
    Deserialize, Default, Debug, Clone, Copy, PartialEq, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelDisplay {
    #[default]
    Unified,
    Always,
}

#[derive(
    Deserialize, Default, Debug, Clone, Copy, PartialEq, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum UnifiedImbalance {
    #[default]
    None,
    Cycle,
    Split,
}

#[derive(
    Deserialize, Default, Debug, Clone, Copy, PartialEq, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum SplitStyle {
    #[default]
    Radiating,
    Stacked,
}

#[derive(
    Deserialize, Default, Debug, Clone, Copy, PartialEq, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum PairLabelStyle {
    #[default]
    Verbose,
    Short,
}

/// Bundles the independent axes that decide how a node's volume is
/// displayed/set, so functions that need all of them (mainly
/// `NodeWidget`/its height calculation) don't need a separate parameter
/// per axis. `channel_mode` and `channel_display` are runtime-mutable
/// (see `ObjectList`); `unified_imbalance`/`split_style`/
/// `pair_label_style` currently aren't (no toggle action yet -
/// config-only), but live here alongside the others so adding one later
/// doesn't change this bundle's shape.
#[derive(Debug, Clone, Copy)]
pub struct ChannelState {
    pub channel_mode: bool,
    pub channel_display: ChannelDisplay,
    pub unified_imbalance: UnifiedImbalance,
    pub split_style: SplitStyle,
    pub pair_label_style: PairLabelStyle,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Keybinding {
    pub key: KeyCode,
    #[serde(default = "Keybinding::default_modifiers")]
    pub modifiers: KeyModifiers,
    pub action: Action,
}

#[derive(Deserialize, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct Names {
    #[serde(default = "Names::default_stream")]
    pub stream: Vec<names::NameTemplate>,
    #[serde(default = "Names::default_endpoint")]
    pub endpoint: Vec<names::NameTemplate>,
    #[serde(default = "Names::default_device")]
    pub device: Vec<names::NameTemplate>,
    #[serde(default)]
    pub overrides: Vec<NameOverride>,
}

#[derive(PartialEq, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum OverrideType {
    Stream,
    Endpoint,
    Device,
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct NameOverride {
    pub types: Vec<OverrideType>,
    pub matches: Vec<MatchCondition>,
    pub templates: Vec<names::NameTemplate>,
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct CharSet {
    pub default_device: String,
    pub default_stream: String,
    pub selector_top: String,
    pub selector_middle: String,
    pub selector_bottom: String,
    pub tab_marker_left: String,
    pub tab_marker_right: String,
    pub list_more: String,
    pub volume_empty: String,
    pub volume_filled: String,
    pub meter_left_inactive: String,
    pub meter_left_active: String,
    pub meter_left_overload: String,
    pub meter_right_inactive: String,
    pub meter_right_active: String,
    pub meter_right_overload: String,
    pub meter_center_left_inactive: String,
    pub meter_center_left_active: String,
    pub meter_center_right_inactive: String,
    pub meter_center_right_active: String,
    /// Per-channel-row monitor overrides (`split_style = "stacked"`'s
    /// `ChannelRowWidget` rows and `split_style = "radiating"`'s
    /// `RadiatingRowWidget` rows) - `None` means "not configured", which
    /// falls back to the corresponding `meter_left`/`meter_right`/
    /// `meter_center_*` field above, so a row's monitor gauge looks
    /// identical to a whole node's until a theme opts in to something
    /// distinct for split rows specifically. The classic single-row
    /// `MeterWidget` never consults these - only per-row split monitors
    /// do.
    pub meter_channel_left_inactive: Option<String>,
    pub meter_channel_left_active: Option<String>,
    pub meter_channel_left_overload: Option<String>,
    pub meter_channel_right_inactive: Option<String>,
    pub meter_channel_right_active: Option<String>,
    pub meter_channel_right_overload: Option<String>,
    pub meter_channel_center_left_inactive: Option<String>,
    pub meter_channel_center_left_active: Option<String>,
    pub meter_channel_center_right_inactive: Option<String>,
    pub meter_channel_center_right_active: Option<String>,
    pub dropdown_icon: String,
    pub dropdown_selector: String,
    pub dropdown_more: String,
    pub dropdown_border: BorderType,
    pub help_more: String,
    pub help_border: BorderType,
}

#[derive(Deserialize, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Theme {
    pub default_device: Style,
    pub default_stream: Style,
    pub selector: Style,
    pub tab: Style,
    pub tab_selected: Style,
    pub tab_marker: Style,
    pub list_more: Style,
    pub node_title: Style,
    pub node_target: Style,
    pub volume: Style,
    pub volume_empty: Style,
    pub volume_filled: Style,
    pub meter_inactive: Style,
    pub meter_active: Style,
    pub meter_overload: Style,
    pub meter_center_inactive: Style,
    pub meter_center_active: Style,
    pub config_device: Style,
    pub config_profile: Style,
    pub dropdown_icon: Style,
    pub dropdown_border: Style,
    pub dropdown_item: Style,
    pub dropdown_selected: Style,
    pub dropdown_more: Style,
    pub help_border: Style,
    pub help_item: Style,
    pub help_more: Style,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub id: Option<String>,
    pub matches: Vec<MatchCondition>,
}

#[derive(
    Deserialize, Default, Debug, Clone, Copy, PartialEq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum TabKind {
    #[default]
    Playback,
    Recording,
    Output,
    Input,
    Configuration,
}

fn default_fps() -> Option<f32> {
    Some(60.0)
}

fn default_mouse() -> bool {
    true
}

fn default_peaks() -> Option<Peaks> {
    Some(Peaks::default())
}

fn default_tab() -> Option<TabKind> {
    Some(TabKind::default())
}

fn default_tabs() -> Vec<TabKind> {
    vec![
        TabKind::Playback,
        TabKind::Recording,
        TabKind::Output,
        TabKind::Input,
        TabKind::Configuration,
    ]
}

fn default_char_set_name() -> String {
    String::from("default")
}

fn default_theme_name() -> String {
    String::from("default")
}

fn default_max_volume_percent() -> Option<f32> {
    Some(150.0)
}

fn default_enforce_max_volume() -> bool {
    false
}

fn default_lazy_capture() -> bool {
    false
}

fn default_channel_display() -> Option<ChannelDisplay> {
    Some(ChannelDisplay::default())
}

fn default_unified_imbalance() -> Option<UnifiedImbalance> {
    Some(UnifiedImbalance::default())
}

fn default_split_style() -> Option<SplitStyle> {
    Some(SplitStyle::default())
}

fn default_channel_mode() -> bool {
    false
}

fn default_pair_label_style() -> Option<PairLabelStyle> {
    Some(PairLabelStyle::default())
}

impl ConfigFile {
    /// Override configuration with command-line arguments.
    pub fn apply_opt(&mut self, opt: &Opt) {
        if let Some(remote) = &opt.remote {
            self.remote = Some(remote.clone());
        }

        if let Some(fps) = opt.fps {
            self.fps = (fps != 0.0).then_some(fps);
        }

        if opt.no_mouse {
            self.mouse = false;
        }

        if opt.mouse {
            self.mouse = true;
        }

        if let Some(peaks) = &opt.peaks {
            self.peaks = Some(peaks.clone());
        }

        if let Some(char_set) = &opt.char_set {
            self.char_set = char_set.clone();
        }

        if let Some(theme) = &opt.theme {
            self.theme = theme.clone();
        }

        if let Some(tab) = &opt.tab {
            self.tab = Some(*tab);
        }

        if let Some(tabs) = &opt.tabs {
            self.tabs = tabs.clone();
        }

        if let Some(max_volume_percent) = &opt.max_volume_percent {
            self.max_volume_percent = Some(*max_volume_percent);
        }

        if opt.no_enforce_max_volume {
            self.enforce_max_volume = false;
        }

        if opt.enforce_max_volume {
            self.enforce_max_volume = true;
        }

        if opt.no_lazy_capture {
            self.lazy_capture = false;
        }

        if opt.lazy_capture {
            self.lazy_capture = true;
        }

        if let Some(channel_display) = &opt.channel_display {
            self.channel_display = Some(*channel_display);
        }

        if let Some(unified_imbalance) = &opt.unified_imbalance {
            self.unified_imbalance = Some(*unified_imbalance);
        }

        if let Some(split_style) = &opt.split_style {
            self.split_style = Some(*split_style);
        }

        if opt.no_channel_mode {
            self.channel_mode = false;
        }

        if opt.channel_mode {
            self.channel_mode = true;
        }

        if let Some(pair_label_style) = &opt.pair_label_style {
            self.pair_label_style = Some(*pair_label_style);
        }
    }
}

impl TryFrom<ConfigFile> for Config {
    type Error = anyhow::Error;

    fn try_from(mut config_file: ConfigFile) -> Result<Self, Self::Error> {
        let Some(char_set) =
            config_file.char_sets.remove(&config_file.char_set)
        else {
            anyhow::bail!(
                "char_set '{}' does not exist",
                &config_file.char_set
            );
        };

        let Some(theme) = config_file.themes.remove(&config_file.theme) else {
            anyhow::bail!("theme '{}' does not exist", &config_file.theme);
        };

        let filters = config_file
            .filters
            .into_iter()
            .flat_map(|f| f.matches)
            .collect();

        let help = help::Help::from(&config_file.keybindings);

        if let Some(max_volume_percent) = config_file.max_volume_percent {
            if max_volume_percent < 0.0 {
                anyhow::bail!(
                    "max_volume_percent {max_volume_percent} is negative"
                );
            }
        }

        if config_file.tabs.is_empty() {
            anyhow::bail!("tabs must be non-empty");
        }

        let tab = config_file
            .tabs
            .iter()
            .position(|&t| t == config_file.tab.unwrap_or_default())
            .context("initial tab not found in tabs")?;

        // Emulate signals. This is intentionally done after generating help.
        config_file
            .keybindings
            .extend(Keybinding::control_char_keybindings());

        Ok(Self {
            remote: config_file.remote,
            fps: config_file.fps.filter(|&fps| fps != 0.0),
            mouse: config_file.mouse,
            peaks: config_file.peaks.unwrap_or_default(),
            max_volume_percent: config_file
                .max_volume_percent
                .unwrap_or_default(),
            enforce_max_volume: config_file.enforce_max_volume,
            char_set,
            theme,
            keybindings: config_file.keybindings,
            help,
            names: config_file.names,
            tab,
            tabs: config_file.tabs,
            lazy_capture: config_file.lazy_capture,
            channel_display: config_file.channel_display.unwrap_or_default(),
            unified_imbalance: config_file
                .unified_imbalance
                .unwrap_or_default(),
            split_style: config_file.split_style.unwrap_or_default(),
            channel_mode: config_file.channel_mode,
            pair_label_style: config_file.pair_label_style.unwrap_or_default(),
            filters,
        })
    }
}

impl Config {
    /// Returns the configuration file path.
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(xdg_config) = env::var("XDG_CONFIG_HOME") {
            return Some(Path::new(&xdg_config).join("wiremix/wiremix.toml"));
        }

        if let Ok(home) = env::var("HOME") {
            return Some(Path::new(&home).join(".config/wiremix/wiremix.toml"));
        }

        None
    }

    /// Parse configuration from the file at the supplied path.
    pub fn try_new(
        path: Option<&Path>,
        opt: &Opt,
    ) -> Result<Self, anyhow::Error> {
        let mut config_file: ConfigFile = match path {
            Some(path) if path.exists() => {
                let context = || {
                    format!(
                        "Failed to read configuration from file '{}'",
                        path.display()
                    )
                };

                let toml_str =
                    fs::read_to_string(path).with_context(context)?;

                toml::from_str(&toml_str).with_context(context)?
            }
            _ => toml::from_str("")?,
        };
        // Override with command-line options
        config_file.apply_opt(opt);
        let config_file = config_file;

        Self::try_from(config_file)
    }

    #[cfg(test)]
    pub fn from_toml_str(toml: &str) -> Self {
        let config_file: ConfigFile = toml::from_str(toml).unwrap();
        Self::try_from(config_file).unwrap()
    }
}

#[cfg(test)]
/// Parse a config file without applying any defaults.
pub mod strict {
    use super::*;

    use serde::de::Error;

    use crate::config::char_set::CharSetOverlay;
    use crate::config::theme::ThemeOverlay;

    #[derive(Deserialize, Debug, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigFile {
        remote: Option<String>,
        fps: Option<f32>,
        mouse: bool,
        peaks: Option<Peaks>,
        char_set: String,
        theme: String,
        max_volume_percent: Option<f32>,
        enforce_max_volume: bool,
        #[serde(deserialize_with = "keybindings")]
        keybindings: HashMap<KeyEvent, Action>,
        names: Names,
        #[serde(deserialize_with = "charsets")]
        char_sets: HashMap<String, CharSet>,
        #[serde(deserialize_with = "themes")]
        themes: HashMap<String, Theme>,
        tab: Option<TabKind>,
        tabs: Vec<TabKind>,
        lazy_capture: bool,
        channel_display: Option<ChannelDisplay>,
        unified_imbalance: Option<UnifiedImbalance>,
        split_style: Option<SplitStyle>,
        channel_mode: bool,
        pair_label_style: Option<PairLabelStyle>,
        filters: Vec<Filter>,
    }

    impl From<ConfigFile> for super::ConfigFile {
        fn from(strict: ConfigFile) -> Self {
            super::ConfigFile {
                remote: strict.remote,
                fps: strict.fps,
                mouse: strict.mouse,
                peaks: strict.peaks,
                char_set: strict.char_set,
                theme: strict.theme,
                max_volume_percent: strict.max_volume_percent,
                enforce_max_volume: strict.enforce_max_volume,
                keybindings: strict.keybindings,
                names: strict.names,
                char_sets: strict.char_sets,
                themes: strict.themes,
                tab: strict.tab,
                tabs: strict.tabs,
                lazy_capture: strict.lazy_capture,
                channel_display: strict.channel_display,
                unified_imbalance: strict.unified_imbalance,
                split_style: strict.split_style,
                channel_mode: strict.channel_mode,
                pair_label_style: strict.pair_label_style,
                filters: strict.filters,
            }
        }
    }

    fn keybindings<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<KeyEvent, Action>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Vec::<Keybinding>::deserialize(deserializer)?
            .into_iter()
            .map(|keybinding| {
                (
                    KeyEvent::new(keybinding.key, keybinding.modifiers),
                    keybinding.action,
                )
            })
            .collect::<HashMap<KeyEvent, Action>>())
    }

    fn charsets<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<String, CharSet>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        HashMap::<String, CharSetOverlay>::deserialize(deserializer)?
            .into_iter()
            .map(|(key, value)| {
                CharSet::try_from(value)
                    .map_err(D::Error::custom)
                    .map(move |charset| (key, charset))
            })
            .collect::<Result<HashMap<String, CharSet>, D::Error>>()
    }

    fn themes<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<String, Theme>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        HashMap::<String, ThemeOverlay>::deserialize(deserializer)?
            .into_iter()
            .map(|(key, value)| {
                Theme::try_from(value)
                    .map_err(D::Error::custom)
                    .map(move |charset| (key, charset))
            })
            .collect::<Result<HashMap<String, Theme>, D::Error>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_field_config_file() {
        let config = r#"
        unknown = "unknown"
        "#;
        assert!(toml::from_str::<ConfigFile>(config).is_err());
    }

    #[test]
    fn unknown_field_keybinding() {
        let config = r#"
        key = { Char = "x" }
        action = "Nothing"
        unknown = "unknown"
        "#;
        assert!(toml::from_str::<Keybinding>(config).is_err());
    }

    #[test]
    fn keybinding_channel_absolute_volume() {
        let config = r#"
        key = { Char = "1" }
        action = { SetChannelAbsoluteVolume = [0, 0.50] }
        "#;
        let keybinding: Keybinding = toml::from_str(config).unwrap();
        assert_eq!(
            keybinding.action,
            Action::SetChannelAbsoluteVolume(0, 0.50)
        );
    }

    #[test]
    fn keybinding_channel_relative_volume() {
        let config = r#"
        key = { Char = "2" }
        action = { SetChannelRelativeVolume = [1, 0.01] }
        "#;
        let keybinding: Keybinding = toml::from_str(config).unwrap();
        assert_eq!(
            keybinding.action,
            Action::SetChannelRelativeVolume(1, 0.01)
        );
    }

    #[test]
    fn keybinding_toggle_channel_mode() {
        let config = r#"
        key = { Char = " " }
        action = "ToggleChannelMode"
        "#;
        let keybinding: Keybinding = toml::from_str(config).unwrap();
        assert_eq!(keybinding.action, Action::ToggleChannelMode);
    }

    #[test]
    fn keybinding_cycle_channel_display() {
        let config = r#"
        key = { Char = "v" }
        action = "CycleChannelDisplay"
        "#;
        let keybinding: Keybinding = toml::from_str(config).unwrap();
        assert_eq!(keybinding.action, Action::CycleChannelDisplay);
    }

    #[test]
    fn channel_display_parses_from_toml() {
        let config: ConfigFile =
            toml::from_str("channel_display = \"always\"").unwrap();
        assert_eq!(config.channel_display, Some(ChannelDisplay::Always));
    }

    #[test]
    fn unified_imbalance_parses_from_toml() {
        let config: ConfigFile =
            toml::from_str("unified_imbalance = \"cycle\"").unwrap();
        assert_eq!(config.unified_imbalance, Some(UnifiedImbalance::Cycle));
    }

    #[test]
    fn split_style_parses_from_toml() {
        let config: ConfigFile =
            toml::from_str("split_style = \"stacked\"").unwrap();
        assert_eq!(config.split_style, Some(SplitStyle::Stacked));
    }

    #[test]
    fn channel_mode_parses_from_toml() {
        let config: ConfigFile = toml::from_str("channel_mode = true").unwrap();
        assert!(config.channel_mode);
    }

    #[test]
    fn pair_label_style_parses_from_toml() {
        let config: ConfigFile =
            toml::from_str("pair_label_style = \"short\"").unwrap();
        assert_eq!(config.pair_label_style, Some(PairLabelStyle::Short));
    }

    #[test]
    fn pair_label_style_defaults_to_verbose() {
        let config = Config::from_toml_str("");
        assert_eq!(config.pair_label_style, PairLabelStyle::Verbose);
    }

    #[test]
    fn unknown_field_names() {
        let config = r#"
        unknown = "unknown"
        "#;
        assert!(toml::from_str::<Names>(config).is_err());
    }

    #[test]
    fn unknown_field_name_override() {
        let config = r#"
        types = [ "stream" ]
        property = "node:node.name"
        value = "value"
        templates = [ "template" ]
        unknown = "unknown"
        "#;
        assert!(toml::from_str::<NameOverride>(config).is_err());
    }

    #[test]
    fn example_config_file_matches_default_config_file() {
        let toml_str = include_str!("../wiremix.toml");
        let example: strict::ConfigFile = toml::from_str(toml_str).unwrap();
        let default: ConfigFile = toml::from_str("").unwrap();

        assert_eq!(default, example.into());
    }

    #[test]
    fn fps_defaults_to_60() {
        let config: ConfigFile = toml::from_str("").unwrap();
        assert_eq!(config.fps, Some(60.0));
    }

    #[test]
    fn fps_can_be_overridden() {
        let config: ConfigFile = toml::from_str("fps = 30.0").unwrap();
        assert_eq!(config.fps, Some(30.0));
    }

    #[test]
    fn fps_zero_means_unlimited() {
        let config_file: ConfigFile = toml::from_str("fps = 0.0").unwrap();
        let config = Config::try_from(config_file).unwrap();
        assert_eq!(config.fps, None);
    }

    #[test]
    fn opt_fps_zero_overrides_config_fps() {
        let mut config_file: ConfigFile = toml::from_str("fps = 30.0").unwrap();
        let opt = Opt {
            fps: Some(0.0),
            ..Default::default()
        };
        config_file.apply_opt(&opt);
        let config = Config::try_from(config_file).unwrap();
        assert_eq!(config.fps, None);
    }

    #[test]
    fn opt_fps_overrides_config_unlimited() {
        let mut config_file: ConfigFile = toml::from_str("fps = 0.0").unwrap();
        let opt = Opt {
            fps: Some(60.0),
            ..Default::default()
        };
        config_file.apply_opt(&opt);
        let config = Config::try_from(config_file).unwrap();
        assert_eq!(config.fps, Some(60.0));
    }

    #[test]
    fn opt_fps_none_preserves_config_fps() {
        let mut config_file: ConfigFile = toml::from_str("fps = 30.0").unwrap();
        config_file.apply_opt(&Default::default());
        let config = Config::try_from(config_file).unwrap();
        assert_eq!(config.fps, Some(30.0));
    }

    #[test]
    fn opt_fps_overrides_config_fps() {
        let mut config_file: ConfigFile = toml::from_str("fps = 20.0").unwrap();
        let opt = Opt {
            fps: Some(30.0),
            ..Default::default()
        };
        config_file.apply_opt(&opt);
        let config = Config::try_from(config_file).unwrap();
        assert_eq!(config.fps, Some(30.0));
    }

    #[test]
    fn tabs_empty_is_error() {
        let config_file: ConfigFile = toml::from_str("tabs = []").unwrap();
        assert!(Config::try_from(config_file).is_err());
    }

    #[test]
    fn tab_not_in_tabs_is_error() {
        let config = r#"
            tabs = ["output", "input"]
        "#;
        let config_file: ConfigFile = toml::from_str(config).unwrap();
        assert!(Config::try_from(config_file).is_err());
    }

    #[test]
    fn tab_index_resolves_to_position_in_tabs() {
        let config = r#"
            tab = "output"
            tabs = ["playback", "output", "input"]
        "#;
        let config = Config::from_toml_str(config);
        assert_eq!(config.tab, 1);
    }

    #[test]
    fn opt_tabs_overrides_config_tabs() {
        let mut config_file: ConfigFile = toml::from_str("").unwrap();
        let opt = Opt {
            tabs: Some(vec![TabKind::Playback, TabKind::Input]),
            ..Default::default()
        };
        config_file.apply_opt(&opt);
        let config = Config::try_from(config_file).unwrap();
        assert_eq!(config.tabs, vec![TabKind::Playback, TabKind::Input]);
    }

    #[test]
    fn name_override_with_matches() {
        let config = r#"
            [[names.overrides]]
            types = ["stream"]
            matches = [{ "node:node.name" = "spotify" }]
            templates = ["{node:node.name}"]
        "#;
        assert_eq!(Config::from_toml_str(config).names.overrides.len(), 1);
    }
}
