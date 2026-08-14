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
    pub show_dividers: bool,
    pub compact_layout: bool,
    pub row_selected_extend_above: bool,
    pub row_selected_extend_below: bool,
    pub max_volume_percent: f32,
    pub enforce_max_volume: bool,
    pub keybindings: HashMap<KeyEvent, Action>,
    pub help: help::Help,
    pub names: Names,
    pub tab: usize,
    pub tabs: Vec<TabKind>,
    pub lazy_capture: bool,
    pub max_concurrent_captures: Option<usize>,
    pub max_concurrent_captures_global: Option<usize>,
    pub capture_hidden: bool,
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
    /// Which `ChannelView`s `Action::CycleView` steps through, and in
    /// what order - see `ChannelView`. Removing one excludes it from
    /// cycling without disabling it entirely; it's still reachable via
    /// `Action::SelectView`. Must be non-empty.
    pub view_cycle: Vec<ChannelView>,
    /// Bar/meter row layout for `Unified` view - see `MeterLayout`.
    pub unified_meter_layout: MeterLayout,
    /// Bar/meter row layout for `Linked` view - see `MeterLayout`.
    pub linked_meter_layout: MeterLayout,
    /// Bar/meter row layout for `Channels` view - see `MeterLayout`.
    pub channels_meter_layout: MeterLayout,
    /// Configurable, default on. A lone stereo pair's `StereoVolumeWidget`
    /// (the classic single-row fast path - it never shows a group label
    /// the way a multi-row block's `RadiatingRowWidget` does) shrinks its
    /// own label area down to just what a plain `"{percent}%"` needs,
    /// handing the rest to the volume bars - a real width increase, not
    /// a token one, at the cost of no longer sharing a bar-start column
    /// with `RadiatingRowWidget` rows elsewhere in the same view.
    pub expand_unused_label_space: bool,
    /// Opt-in, default off. An unpaired channel's row in a
    /// `split_style = "radiating"` block normally occupies just the left
    /// half of the row's column grid (mirroring where a paired row's own
    /// left bar would be, so every row in the block starts/ends its bar
    /// at the same columns) - when this is on, it stretches across the
    /// row's full remaining width instead.
    pub expand_unpaired_channel_bars: bool,
    pub filters: Vec<MatchCondition>,
}

/// Overrides for one view's bar/meter row layout: the percentage of a
/// row's combined volume+meter width given to the meter side, the blank
/// gap between them, and the blank margin reserved at the row's right
/// edge (all once `peaks` is on - `gap`/`meter_width_percent` are moot
/// with `peaks = "off"`, nothing to gap/split against). `meter_width_percent`
/// left `None` (its default) reproduces stock wiremix's own proportional
/// ratio; setting it opts that one field, for that one view, into a
/// fixed-column override instead. `right_margin` is always a fixed
/// column count; `gap` is a *floor* rather than a fixed override - the
/// actual gap scales up above it with the meter side's own available
/// width (see `effective_gap` in `node_widget.rs`), rather than staying
/// visually cramped in a wide terminal. Both are carved out of the
/// meter/monitor side's own share of the row, not the volume side's, so
/// widening either never costs the volume area any width. The three
/// fields and three views are all independent of each other. See
/// `Config::meter_layout` for how a render picks which of the three (one
/// per `ChannelView`) applies.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeterLayout {
    pub meter_width_percent: Option<f32>,
    pub gap: u16,
    pub right_margin: u16,
}

// This is what actually gets parsed from the config - see `MeterLayout`.
#[derive(Deserialize, Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
struct MeterLayoutFile {
    meter_width_percent: Option<f32>,
    #[serde(default = "default_gap")]
    gap: u16,
    #[serde(default = "default_right_margin")]
    right_margin: u16,
}

// A small gap by default, carved from the meter side alone - still
// fully overridable per view with any other fixed value, including 0.
fn default_gap() -> u16 {
    2
}

// About half of the default gap's own trailing counterpart - a small
// margin by default, carved from the meter side alone - still fully
// overridable per view with any other fixed value, including 0.
fn default_right_margin() -> u16 {
    3
}

impl Default for MeterLayoutFile {
    fn default() -> Self {
        Self {
            meter_width_percent: None,
            gap: default_gap(),
            right_margin: default_right_margin(),
        }
    }
}

impl MeterLayoutFile {
    fn validate(self, label: &str) -> anyhow::Result<MeterLayout> {
        if let Some(percent) = self.meter_width_percent {
            if !(1.0..=99.0).contains(&percent) {
                anyhow::bail!(
                    "{label}.meter_width_percent {percent} must be \
                     between 1 and 99 - to hide the meter entirely, use \
                     peaks = \"off\" instead"
                );
            }
        }
        Ok(MeterLayout {
            meter_width_percent: self.meter_width_percent,
            gap: self.gap,
            right_margin: self.right_margin,
        })
    }
}

/// Represents a configuration deserialized from a file. This gets baked into a
/// Config, which, for example, has a single char_set and theme.
#[derive(Deserialize, Debug)]
#[cfg_attr(test, derive(PartialEq))]
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
    #[serde(default = "default_show_dividers")]
    show_dividers: bool,
    #[serde(default = "default_compact_layout")]
    compact_layout: bool,
    #[serde(default = "default_row_selected_extend")]
    row_selected_extend_above: bool,
    #[serde(default = "default_row_selected_extend")]
    row_selected_extend_below: bool,
    #[serde(default = "default_max_volume_percent")]
    max_volume_percent: Option<f32>,
    #[serde(default = "default_enforce_max_volume")]
    enforce_max_volume: bool,
    #[serde(default = "default_lenient_config")]
    lenient_config: bool,
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
    max_concurrent_captures: Option<usize>,
    max_concurrent_captures_global: Option<usize>,
    #[serde(default = "default_capture_hidden")]
    capture_hidden: bool,
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
    #[serde(default = "default_view_cycle")]
    view_cycle: Vec<ChannelView>,
    #[serde(default)]
    unified_meter_layout: MeterLayoutFile,
    #[serde(default)]
    linked_meter_layout: MeterLayoutFile,
    #[serde(default)]
    channels_meter_layout: MeterLayoutFile,
    #[serde(default = "default_expand_unused_label_space")]
    expand_unused_label_space: bool,
    #[serde(default = "default_expand_unpaired_channel_bars")]
    expand_unpaired_channel_bars: bool,
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

/// One of the three high-level ways the object list can display and
/// target a node's volume - a convenience wrapper over `channel_mode`/
/// `channel_display`, derived from them rather than stored as separate
/// state (see `ObjectList::channel_view`), used only for `Action::
/// SelectView`/`Action::CycleView` and `Config::view_cycle`.
/// "unified" = `channel_mode = false`, `channel_display = "unified"`
/// (one collapsed bar/row). "linked" = `channel_mode = false`,
/// `channel_display = "always"` (always split, but volume keys still
/// adjust every channel together). "channels" = `channel_mode = true`
/// (always split, volume keys target only the cursored channel).
#[derive(
    Deserialize,
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelView {
    #[default]
    Unified,
    Linked,
    Channels,
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

impl ChannelState {
    /// The `ChannelView` this state amounts to - see `ChannelView`'s own
    /// doc comment for the exact mapping. Used both by `ObjectList`
    /// (`Action::CycleView`'s notion of "current view") and by
    /// `node_widget` (to decide whether a row should use `Unified`
    /// view's stock-identical layout or the aligned/aggressive one
    /// shared by `Linked`/`Channels`).
    pub fn view(&self) -> ChannelView {
        if self.channel_mode {
            ChannelView::Channels
        } else if self.channel_display == ChannelDisplay::Always {
            ChannelView::Linked
        } else {
            ChannelView::Unified
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct Keybinding {
    pub key: KeyCode,
    #[serde(default = "Keybinding::default_modifiers")]
    pub modifiers: KeyModifiers,
    pub action: Action,
}

#[derive(Deserialize, Debug)]
#[cfg_attr(test, derive(PartialEq))]
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
    pub hidden_instance: String,
    pub hidden_permanent: String,
    pub selector_top: String,
    pub selector_middle: String,
    pub selector_bottom: String,
    pub tab_marker_left: String,
    pub tab_marker_right: String,
    pub list_more: String,
    pub divider: String,
    pub volume_empty: String,
    pub volume_filled: String,
    pub meter_left_inactive: String,
    pub meter_left_inactive_overload: String,
    pub meter_left_active: String,
    pub meter_left_overload: String,
    pub meter_right_inactive: String,
    pub meter_right_inactive_overload: String,
    pub meter_right_active: String,
    pub meter_right_overload: String,
    pub meter_center_left_inactive: String,
    pub meter_center_left_active: String,
    pub meter_center_right_inactive: String,
    pub meter_center_right_active: String,
    /// Monitor glyphs used whenever the active view (`ChannelView`) is
    /// `Linked` or `Channels` rather than `Unified` - `None` means "not
    /// configured", which falls back to the corresponding `meter_left`/
    /// `meter_right`/`meter_center_*` field above, so a split-view
    /// monitor gauge looks identical to `Unified`'s until a theme opts
    /// in to something distinct. `Unified` view never consults these.
    pub meter_split_left_inactive: Option<String>,
    pub meter_split_left_active: Option<String>,
    pub meter_split_left_overload: Option<String>,
    pub meter_split_right_inactive: Option<String>,
    pub meter_split_right_active: Option<String>,
    pub meter_split_right_overload: Option<String>,
    pub meter_split_center_left_inactive: Option<String>,
    pub meter_split_center_left_active: Option<String>,
    pub meter_split_center_right_inactive: Option<String>,
    pub meter_split_center_right_active: Option<String>,
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
    pub divider: Style,
    pub node_title: Style,
    pub node_target: Style,
    pub volume: Style,
    pub volume_empty: Style,
    pub volume_filled: Style,
    pub meter_inactive: Style,
    pub meter_inactive_overload: Style,
    pub meter_active: Style,
    pub meter_overload: Style,
    pub meter_center_inactive: Style,
    pub meter_center_active: Style,
    /// Monitor colors used whenever the active view (`ChannelView`) is
    /// `Linked` or `Channels` rather than `Unified`, same "unset falls
    /// back to the stock meter_* field above" idea as `CharSet`'s
    /// `meter_split_*` glyph overrides.
    pub meter_split_inactive: Option<Style>,
    pub meter_split_active: Option<Style>,
    pub meter_split_overload: Option<Style>,
    pub meter_split_center_inactive: Option<Style>,
    pub meter_split_center_active: Option<Style>,
    pub config_device: Style,
    pub config_profile: Style,
    pub row_hidden: Style,
    pub row_selected: Style,
    pub row_unselected: Style,
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

fn default_lenient_config() -> bool {
    false
}

fn default_lazy_capture() -> bool {
    false
}

fn default_show_dividers() -> bool {
    false
}

fn default_compact_layout() -> bool {
    false
}

fn default_capture_hidden() -> bool {
    true
}

fn default_row_selected_extend() -> bool {
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

fn default_expand_unused_label_space() -> bool {
    true
}

fn default_expand_unpaired_channel_bars() -> bool {
    false
}

fn default_pair_label_style() -> Option<PairLabelStyle> {
    Some(PairLabelStyle::default())
}

fn default_view_cycle() -> Vec<ChannelView> {
    vec![
        ChannelView::Unified,
        ChannelView::Linked,
        ChannelView::Channels,
    ]
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

        if opt.no_show_dividers {
            self.show_dividers = false;
        }

        if opt.show_dividers {
            self.show_dividers = true;
        }

        if opt.no_compact_layout {
            self.compact_layout = false;
        }

        if opt.compact_layout {
            self.compact_layout = true;
        }

        if opt.no_lenient_config {
            self.lenient_config = false;
        }

        if opt.lenient_config {
            self.lenient_config = true;
        }

        if let Some(max_concurrent_captures) = &opt.max_concurrent_captures {
            self.max_concurrent_captures = Some(*max_concurrent_captures);
        }

        if let Some(max_concurrent_captures_global) =
            &opt.max_concurrent_captures_global
        {
            self.max_concurrent_captures_global =
                Some(*max_concurrent_captures_global);
        }

        if opt.no_capture_hidden {
            self.capture_hidden = false;
        }

        if opt.capture_hidden {
            self.capture_hidden = true;
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

    /// Parses `toml_str` into a `ConfigFile`, then applies `opt`'s
    /// overrides. Unknown fields anywhere in the file - top-level or
    /// nested inside `[[keybindings]]`, `[themes.*]`, `[char_sets.*]`,
    /// etc. - are collected rather than failing on just the first one
    /// found. If `lenient_config` ends up `false` (the default, after
    /// `opt` has had a chance to override it), any unknown fields turn
    /// into a single aggregated error; if `true`, each one is only
    /// logged as a warning and parsing proceeds using defaults for the
    /// rest of that value.
    fn parse(toml_str: &str, opt: &Opt) -> anyhow::Result<Self> {
        let mut unknown_fields = Vec::new();
        let deserializer = toml::Deserializer::parse(toml_str)?;
        let mut config_file: Self =
            serde_ignored::deserialize(deserializer, |path| {
                unknown_fields.push(path.to_string());
            })?;

        config_file.apply_opt(opt);

        if !unknown_fields.is_empty() {
            if config_file.lenient_config {
                for field in &unknown_fields {
                    eprintln!(
                        "wiremix: warning: ignoring unknown configuration \
                         field '{field}'"
                    );
                }
            } else {
                anyhow::bail!(
                    "unknown configuration field(s): {} (pass \
                     --lenient-config, or set lenient_config = true, to \
                     ignore instead of failing)",
                    unknown_fields.join(", ")
                );
            }
        }

        Ok(config_file)
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

        let unified_meter_layout = config_file
            .unified_meter_layout
            .validate("unified_meter_layout")?;
        let linked_meter_layout = config_file
            .linked_meter_layout
            .validate("linked_meter_layout")?;
        let channels_meter_layout = config_file
            .channels_meter_layout
            .validate("channels_meter_layout")?;

        if config_file.tabs.is_empty() {
            anyhow::bail!("tabs must be non-empty");
        }

        if config_file.view_cycle.is_empty() {
            anyhow::bail!("view_cycle must be non-empty");
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
            show_dividers: config_file.show_dividers,
            compact_layout: config_file.compact_layout,
            row_selected_extend_above: config_file.row_selected_extend_above,
            row_selected_extend_below: config_file.row_selected_extend_below,
            keybindings: config_file.keybindings,
            help,
            names: config_file.names,
            tab,
            tabs: config_file.tabs,
            lazy_capture: config_file.lazy_capture,
            max_concurrent_captures: config_file.max_concurrent_captures,
            max_concurrent_captures_global: config_file
                .max_concurrent_captures_global,
            capture_hidden: config_file.capture_hidden,
            channel_display: config_file.channel_display.unwrap_or_default(),
            unified_imbalance: config_file
                .unified_imbalance
                .unwrap_or_default(),
            split_style: config_file.split_style.unwrap_or_default(),
            channel_mode: config_file.channel_mode,
            pair_label_style: config_file.pair_label_style.unwrap_or_default(),
            view_cycle: config_file.view_cycle,
            unified_meter_layout,
            linked_meter_layout,
            channels_meter_layout,
            expand_unused_label_space: config_file.expand_unused_label_space,
            expand_unpaired_channel_bars: config_file
                .expand_unpaired_channel_bars,
            filters,
        })
    }
}

impl Config {
    /// This view's `MeterLayout` overrides - see `MeterLayout`'s own doc
    /// comment.
    pub fn meter_layout(&self, view: ChannelView) -> MeterLayout {
        match view {
            ChannelView::Unified => self.unified_meter_layout,
            ChannelView::Linked => self.linked_meter_layout,
            ChannelView::Channels => self.channels_meter_layout,
        }
    }

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
        let config_file = match path {
            Some(path) if path.exists() => {
                let context = || {
                    format!(
                        "Failed to read configuration from file '{}'",
                        path.display()
                    )
                };

                let toml_str =
                    fs::read_to_string(path).with_context(context)?;

                ConfigFile::parse(&toml_str, opt).with_context(context)?
            }
            _ => ConfigFile::parse("", opt)?,
        };

        Self::try_from(config_file)
    }

    #[cfg(test)]
    pub fn from_toml_str(toml: &str) -> Self {
        let config_file = ConfigFile::parse(toml, &Opt::default()).unwrap();
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
        show_dividers: bool,
        compact_layout: bool,
        row_selected_extend_above: bool,
        row_selected_extend_below: bool,
        max_volume_percent: Option<f32>,
        enforce_max_volume: bool,
        lenient_config: bool,
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
        max_concurrent_captures: Option<usize>,
        max_concurrent_captures_global: Option<usize>,
        capture_hidden: bool,
        channel_display: Option<ChannelDisplay>,
        unified_imbalance: Option<UnifiedImbalance>,
        split_style: Option<SplitStyle>,
        channel_mode: bool,
        pair_label_style: Option<PairLabelStyle>,
        view_cycle: Vec<ChannelView>,
        unified_meter_layout: MeterLayoutFile,
        linked_meter_layout: MeterLayoutFile,
        channels_meter_layout: MeterLayoutFile,
        expand_unused_label_space: bool,
        expand_unpaired_channel_bars: bool,
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
                show_dividers: strict.show_dividers,
                compact_layout: strict.compact_layout,
                row_selected_extend_above: strict.row_selected_extend_above,
                row_selected_extend_below: strict.row_selected_extend_below,
                max_volume_percent: strict.max_volume_percent,
                enforce_max_volume: strict.enforce_max_volume,
                lenient_config: strict.lenient_config,
                keybindings: strict.keybindings,
                names: strict.names,
                char_sets: strict.char_sets,
                themes: strict.themes,
                tab: strict.tab,
                tabs: strict.tabs,
                lazy_capture: strict.lazy_capture,
                max_concurrent_captures: strict.max_concurrent_captures,
                max_concurrent_captures_global: strict
                    .max_concurrent_captures_global,
                capture_hidden: strict.capture_hidden,
                channel_display: strict.channel_display,
                unified_imbalance: strict.unified_imbalance,
                split_style: strict.split_style,
                channel_mode: strict.channel_mode,
                pair_label_style: strict.pair_label_style,
                view_cycle: strict.view_cycle,
                unified_meter_layout: strict.unified_meter_layout,
                linked_meter_layout: strict.linked_meter_layout,
                channels_meter_layout: strict.channels_meter_layout,
                expand_unused_label_space: strict.expand_unused_label_space,
                expand_unpaired_channel_bars: strict
                    .expand_unpaired_channel_bars,
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
    fn unknown_field_top_level_errors_by_default() {
        let result =
            ConfigFile::parse("unknown = \"unknown\"", &Opt::default());
        assert!(result.is_err());
    }

    #[test]
    fn unknown_field_keybinding_errors_by_default() {
        let config = r#"
        keybindings = [
            { key = { Char = "x" }, action = "Nothing", unknown = "unknown" },
        ]
        "#;
        assert!(ConfigFile::parse(config, &Opt::default()).is_err());
    }

    #[test]
    fn unknown_field_names_errors_by_default() {
        let config = "[names]\nunknown = \"unknown\"";
        assert!(ConfigFile::parse(config, &Opt::default()).is_err());
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
    fn unknown_field_name_override_errors_by_default() {
        let config = r#"
        [names]
        overrides = [
            {
                types = [ "stream" ],
                property = "node:node.name",
                value = "value",
                templates = [ "template" ],
                unknown = "unknown",
            },
        ]
        "#;
        assert!(ConfigFile::parse(config, &Opt::default()).is_err());
    }

    #[test]
    fn unknown_field_nested_theme_errors_by_default() {
        let config = "[themes.default]\nunknown = { }";
        assert!(ConfigFile::parse(config, &Opt::default()).is_err());
    }

    #[test]
    fn unknown_field_nested_char_set_errors_by_default() {
        let config = "[char_sets.default]\nunknown = \"x\"";
        assert!(ConfigFile::parse(config, &Opt::default()).is_err());
    }

    #[test]
    fn unknown_field_lenient_via_config_file_is_ignored() {
        let config = "lenient_config = true\nunknown = \"unknown\"";
        assert!(ConfigFile::parse(config, &Opt::default()).is_ok());
    }

    #[test]
    fn unknown_field_lenient_via_cli_flag_is_ignored() {
        let opt = Opt {
            lenient_config: true,
            ..Default::default()
        };
        let result = ConfigFile::parse("unknown = \"unknown\"", &opt);
        assert!(result.is_ok());
    }

    #[test]
    fn cli_no_lenient_config_overrides_config_file_lenient() {
        let opt = Opt {
            no_lenient_config: true,
            ..Default::default()
        };
        let config = "lenient_config = true\nunknown = \"unknown\"";
        assert!(ConfigFile::parse(config, &opt).is_err());
    }

    #[test]
    fn no_unknown_fields_ok_even_when_strict() {
        let result = ConfigFile::parse("fps = 30.0", &Opt::default());
        assert!(result.is_ok());
    }

    #[test]
    fn example_config_file_matches_default_config_file() {
        let toml_str = include_str!("../wiremix.toml");
        let example: strict::ConfigFile = toml::from_str(toml_str).unwrap();
        let default: ConfigFile = toml::from_str("").unwrap();
        let mut example: ConfigFile = example.into();

        // wiremix.toml also documents an example "redshift"/
        // "redshift_compact" theme/char_set beyond the built-in ones -
        // strip those extra entries before comparing, so this test still
        // verifies the *built-in* documented defaults match the
        // compiled-in Rust defaults without requiring every example
        // theme to be a no-op.
        example
            .themes
            .retain(|name, _| default.themes.contains_key(name));
        example
            .char_sets
            .retain(|name, _| default.char_sets.contains_key(name));

        assert_eq!(default, example);
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
    fn meter_layout_percent_out_of_range_is_error() {
        let too_low: ConfigFile =
            toml::from_str("[linked_meter_layout]\nmeter_width_percent = 0.0")
                .unwrap();
        assert!(Config::try_from(too_low).is_err());

        let too_high: ConfigFile = toml::from_str(
            "[channels_meter_layout]\nmeter_width_percent = 100.0",
        )
        .unwrap();
        assert!(Config::try_from(too_high).is_err());
    }

    #[test]
    fn view_cycle_defaults_to_all_three_and_is_configurable() {
        let default = Config::from_toml_str("");
        assert_eq!(
            default.view_cycle,
            vec![
                ChannelView::Unified,
                ChannelView::Linked,
                ChannelView::Channels
            ]
        );

        let configured =
            Config::from_toml_str("view_cycle = [\"linked\", \"channels\"]");
        assert_eq!(
            configured.view_cycle,
            vec![ChannelView::Linked, ChannelView::Channels]
        );
    }

    #[test]
    fn view_cycle_empty_is_error() {
        let config_file: ConfigFile =
            toml::from_str("view_cycle = []").unwrap();
        assert!(Config::try_from(config_file).is_err());
    }

    #[test]
    fn meter_layout_defaults_to_small_gap_and_margin_and_is_configurable_per_view(
    ) {
        // meter_width_percent stays unset by default (reproduces stock's
        // own proportional ratio); gap/right_margin default to small
        // fixed values (see default_gap/default_right_margin) rather
        // than stock's wider proportional ones, universally across all
        // three views.
        let stock_default = MeterLayout {
            meter_width_percent: None,
            gap: 2,
            right_margin: 3,
        };
        let default = Config::from_toml_str("");
        assert_eq!(default.meter_layout(ChannelView::Unified), stock_default);
        assert_eq!(default.meter_layout(ChannelView::Linked), stock_default);
        assert_eq!(default.meter_layout(ChannelView::Channels), stock_default);

        let configured = Config::from_toml_str(
            "[linked_meter_layout]\n\
             meter_width_percent = 30.0\n\
             right_margin = 4\n\
             [channels_meter_layout]\n\
             gap = 5",
        );
        assert_eq!(
            configured.meter_layout(ChannelView::Unified),
            stock_default,
            "unified_meter_layout wasn't touched, must stay on its \
             (small-gap/margin) defaults"
        );
        assert_eq!(
            configured.meter_layout(ChannelView::Linked),
            MeterLayout {
                meter_width_percent: Some(30.0),
                gap: 2,
                right_margin: 4,
            }
        );
        assert_eq!(
            configured.meter_layout(ChannelView::Channels),
            MeterLayout {
                meter_width_percent: None,
                gap: 5,
                right_margin: 3,
            }
        );
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
