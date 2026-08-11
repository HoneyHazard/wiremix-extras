//! Parse command-line arguments.

use std::path::PathBuf;

use clap::Parser;

use crate::config::{self, TabKind};

const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

#[derive(Parser, Default)]
#[clap(name = "wiremix", about = "PipeWire mixer")]
#[command(version = VERSION)]
pub struct Opt {
    /// Override default config file path
    #[clap(short = 'c', long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// The name of the remote to connect to
    #[clap(short, long, value_name = "NAME")]
    pub remote: Option<String>,

    /// Target frames per second (or 0 for unlimited)
    #[clap(short, long)]
    pub fps: Option<f32>,

    /// Character set to use [built-in sets: default, compat, extracompat]
    #[clap(short = 's', long, value_name = "NAME")]
    pub char_set: Option<String>,

    /// Theme to use [built-in themes: default, nocolor, plain]
    #[clap(short, long, value_name = "NAME")]
    pub theme: Option<String>,

    /// Audio peak meters
    #[clap(short, long, value_parser = clap::value_parser!(config::Peaks))]
    pub peaks: Option<config::Peaks>,

    /// Disable mouse support
    #[clap(long, conflicts_with = "mouse")]
    pub no_mouse: bool,

    /// Enable mouse support
    #[clap(long, conflicts_with = "no_mouse")]
    pub mouse: bool,

    /// Initial tab view
    #[clap(
        short = 'v',
        long,
        value_enum,
        value_parser = clap::value_parser!(TabKind),
    )]
    pub tab: Option<TabKind>,

    /// Which tabs are present and their order
    #[clap(short = 'T', long, num_args = 1.., value_enum)]
    pub tabs: Option<Vec<TabKind>>,

    /// Maximum volume for volume sliders
    #[clap(short = 'm', long, value_name = "PERCENT")]
    pub max_volume_percent: Option<f32>,

    /// Allow increasing volume past max-volume-percent
    #[clap(long, conflicts_with = "enforce_max_volume")]
    pub no_enforce_max_volume: bool,

    /// Prevent increasing volume past max-volume-percent
    #[clap(long, conflicts_with = "no_enforce_max_volume")]
    pub enforce_max_volume: bool,

    /// Monitor peak levels of all nodes
    #[clap(long, conflicts_with = "lazy_capture")]
    pub no_lazy_capture: bool,

    /// Only monitor peak levels of on-screen nodes (reduces CPU usage, but
    /// peaks appear with a slight delay)
    #[clap(long, conflicts_with = "no_lazy_capture")]
    pub lazy_capture: bool,

    /// Whether a node's volume is ever shown as more than one bar/row when
    /// its setting is linked - "unified" (always one, see
    /// --unified-imbalance) or "always" (always split, see --split-style)
    #[clap(long, value_parser = clap::value_parser!(config::ChannelDisplay))]
    pub channel_display: Option<config::ChannelDisplay>,

    /// When --channel-display=unified, how an imbalanced node (channels
    /// that don't all hold the same value) is indicated without actually
    /// splitting the display: "none" (no indication), "cycle" (briefly
    /// cycle through each channel's own label+value), or "split" (that one
    /// node splits, per --split-style, while balanced nodes stay unified)
    #[clap(long, value_parser = clap::value_parser!(config::UnifiedImbalance))]
    pub unified_imbalance: Option<config::UnifiedImbalance>,

    /// Rendering style whenever a node's volume actually is split:
    /// "radiating" (a lone simple pair gets one row with two bars growing
    /// from a shared center marker; anything with more channels - extra
    /// singles alongside a pair, more than one pair, or no pair at all -
    /// gets one row per detected pair/channel instead, each pair still
    /// radiating on its own row) or "stacked" (one row per channel, always,
    /// regardless of pairing)
    #[clap(long, value_parser = clap::value_parser!(config::SplitStyle))]
    pub split_style: Option<config::SplitStyle>,

    /// Start with volume keys adjusting every channel of the selected node
    /// together (linked/ganged) - the default
    #[clap(long, conflicts_with = "channel_mode")]
    pub no_channel_mode: bool,

    /// Start with volume keys adjusting only the individually-cursored
    /// channel of the selected node ("Channel mode" - see
    /// ToggleChannelMode)
    #[clap(long, conflicts_with = "no_channel_mode")]
    pub channel_mode: bool,

    /// How a radiating pair row labels which physical pair it's showing,
    /// once more than one row can appear in the same node's split display
    /// (a lone pair needs no label): "verbose" ("F L/R") or "short" ("F")
    #[clap(long, value_parser = clap::value_parser!(config::PairLabelStyle))]
    pub pair_label_style: Option<config::PairLabelStyle>,

    #[cfg(debug_assertions)]
    #[clap(short, long)]
    pub dump_events: bool,
}

impl Opt {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}
