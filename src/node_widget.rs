//! A Ratatui widget representing a single PipeWire node in an object list.

use std::sync::atomic::Ordering;

use ratatui::{
    layout::Flex,
    prelude::{Alignment, Buffer, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{StatefulWidget, Widget},
};

use crossterm::event::{MouseButton, MouseEventKind};
use smallvec::smallvec;

use crate::app::{Action, MouseArea};
use crate::channel_pairing::{self, ChannelGroup};
use crate::config::{
    ChannelDisplay, ChannelState, Config, Peaks, SplitStyle, UnifiedImbalance,
};
use crate::device_kind::DeviceKind;
use crate::meter;
use crate::object_list::ObjectList;
use crate::view;

fn is_default(node: &view::Node, device_kind: Option<DeviceKind>) -> bool {
    match device_kind {
        Some(DeviceKind::Sink) => node.is_default_sink,
        Some(DeviceKind::Source) => node.is_default_source,
        None => false,
    }
}

/// Whether `node`'s own channels currently all hold the same value.
/// `false` for a node with 0 or 1 channels - nothing to be imbalanced
/// between.
fn is_imbalanced(node: &view::Node) -> bool {
    let mut volumes = node.volumes.iter();
    let Some(&first) = volumes.next() else {
        return false;
    };
    volumes.any(|&volume| volume != first)
}

/// What `NodeWidget` should actually show for a node's volume, resolved
/// from the current channel state and the node's own data. See
/// `NOTES-multichannel.md` §9's config-restructuring writeup for the
/// full reasoning behind this precedence.
#[derive(Debug, Clone, Copy, PartialEq)]
enum VolumeDisplay {
    /// One combined bar/row - today's stock behavior.
    Unified,
    /// Two bars sharing one row, radiating from a shared center marker.
    /// Only ever chosen for a detected 2-channel left/right pair.
    Radiating {
        left_index: usize,
        right_index: usize,
    },
    /// One row per channel.
    Stacked,
}

fn volume_display(
    channel_state: ChannelState,
    node: &view::Node,
) -> VolumeDisplay {
    if node.volumes.len() <= 1 {
        return VolumeDisplay::Unified;
    }

    // Channel mode (individual setting) always wins over the display
    // axis: always stacked, regardless of channel_display/split_style -
    // radiating's marker-placement problem for an individually-cursored
    // channel isn't solved yet (see NOTES-multichannel.md §5/§7.5).
    if channel_state.channel_mode {
        return VolumeDisplay::Stacked;
    }

    let wants_split = match channel_state.channel_display {
        ChannelDisplay::Always => true,
        ChannelDisplay::Unified => {
            is_imbalanced(node)
                && channel_state.unified_imbalance == UnifiedImbalance::Split
        }
    };

    if !wants_split {
        return VolumeDisplay::Unified;
    }

    match channel_state.split_style {
        SplitStyle::Stacked => VolumeDisplay::Stacked,
        SplitStyle::Radiating => match stereo_pair(node) {
            Some((left_index, right_index)) => VolumeDisplay::Radiating {
                left_index,
                right_index,
            },
            // Not a detected pair (odd channel count, or channels with
            // no left/right naming) - nothing for radiating to grow
            // from center, fall back to stacked.
            None => VolumeDisplay::Stacked,
        },
    }
}

/// How long each channel is shown before `unified_imbalance = "cycle"`
/// advances to the next one. Not user-configurable yet (see
/// `NOTES-multichannel.md` §10) - a reasonable fixed value for a first
/// version, in the 0.5-2s range floated during design.
const CYCLE_INTERVAL_SECONDS: f32 = 1.5;

/// Which channel `unified_imbalance = "cycle"` should show right now for
/// an imbalanced node in Unified display, if it applies at all. `None`
/// whenever cycling isn't active for this node (display isn't actually
/// Unified, `unified_imbalance` isn't "cycle", or the node is balanced -
/// nothing to cycle through).
///
/// Deliberately stateless: computed fresh from `elapsed_seconds` (shared
/// across every node, so they all cycle at the same rate) and a
/// per-node phase offset derived from the node's own object ID (so
/// imbalanced nodes don't all flip in lockstep) - no stored "last switch
/// time" field anywhere, matching how `positions`/`volumes` are already
/// read fresh from `view::Node` every render rather than cached. See
/// NOTES-multichannel.md §7.2/§10 for why this shape was chosen over
/// explicit per-node stored state.
fn cycling_channel(
    channel_state: ChannelState,
    node: &view::Node,
    elapsed_seconds: f32,
) -> Option<usize> {
    if channel_state.channel_display != ChannelDisplay::Unified
        || channel_state.unified_imbalance != UnifiedImbalance::Cycle
    {
        return None;
    }
    let channel_count = node.volumes.len();
    if channel_count <= 1 || !is_imbalanced(node) {
        return None;
    }

    // A stable, deterministic value in [0, 1) derived from the node's own
    // ID - gives every imbalanced node the same cycle *rate* while
    // landing at a different *phase*, so a list full of imbalanced nodes
    // doesn't read as one uniform flashing block.
    let phase = (u32::from(node.object_id) % 997) as f32 / 997.0;
    let position =
        elapsed_seconds / CYCLE_INTERVAL_SECONDS + phase * channel_count as f32;
    Some(position as usize % channel_count)
}

pub struct NodeWidget<'a> {
    config: &'a Config,
    device_kind: Option<DeviceKind>,
    node: &'a view::Node,
    selected: bool,
    channel_state: ChannelState,
    /// Which channel of *this* node is cursor-targeted, if any. Only
    /// meaningful when `selected` is also true - another node's channel
    /// index has no bearing on this one's marker.
    selected_channel: Option<usize>,
    /// Seconds elapsed since `App` started - see `cycling_channel`.
    elapsed_seconds: f32,
}

impl<'a> NodeWidget<'a> {
    pub fn new(
        config: &'a Config,
        device_kind: Option<DeviceKind>,
        node: &'a view::Node,
        selected: bool,
        channel_state: ChannelState,
        selected_channel: Option<usize>,
        elapsed_seconds: f32,
    ) -> Self {
        Self {
            config,
            device_kind,
            node,
            selected,
            channel_state,
            selected_channel,
            elapsed_seconds,
        }
    }

    /// Height of a full node display in the default (unified/radiating)
    /// layout - both fit in the same fixed height.
    pub fn height() -> u16 {
        3
    }

    /// Height of `node`'s display given the current channel state -
    /// stacked display expands a node with more than one channel into one
    /// header line plus one line per channel; everything else renders at
    /// the ordinary fixed `height()`.
    pub fn node_height(channel_state: ChannelState, node: &view::Node) -> u16 {
        match volume_display(channel_state, node) {
            VolumeDisplay::Stacked => 1 + node.volumes.len() as u16,
            _ => Self::height(),
        }
    }

    /// Spacing between nodes
    pub fn spacing() -> u16 {
        2
    }

    /// Area for the target dropdown
    pub fn dropdown_area(
        object_list: &ObjectList,
        list_area: &Rect,
        object_area: &Rect,
    ) -> Rect {
        // Number of items to show at once
        let max_visible_items = 5;

        let max_target_length = object_list
            .targets
            .iter()
            .map(|(_, title)| title.len())
            .max()
            .unwrap_or(0);

        // Add 2 for vertical borders and 2 for highlight symbol
        let width = max_target_length.saturating_add(4) as u16;
        let height = std::cmp::min(max_visible_items, object_list.targets.len())
            .saturating_add(2) as u16; // Plus 2 for horizontal borders

        // Align to the right of the list area
        let x = list_area.right().saturating_sub(width);
        // Subtract 1 for the top border
        let y = object_area.top().saturating_sub(1);

        Rect::new(x, y, width, height)
    }

    /// Channel mode display: one header line (title/target, same as the
    /// ordinary layout) followed by one line per channel, each its own
    /// independently addressable volume bar. See `node_height` for the
    /// matching height calculation.
    fn render_channel_rows(
        &self,
        area: Rect,
        buf: &mut Buffer,
        mouse_areas: &mut Vec<MouseArea>,
        channel_count: usize,
    ) {
        let mut constraints = vec![Constraint::Length(1)]; // header_row
        constraints.extend(
            std::iter::repeat(Constraint::Length(1)).take(channel_count),
        );
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let row_layout = |row: Rect| {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(1), // marker_area
                    Constraint::Min(0),    // rest_area
                ])
                .split(row)
        };

        // The header row deliberately never shows the selector marker in
        // Channel mode - only the individually-targeted channel row does,
        // so there's exactly one place to look for "which channel is
        // this" rather than two markers competing for attention. The
        // marker_area column is still reserved here (unrendered) purely
        // so the header text lines up with the channel rows below it.
        let header_split = row_layout(rows[0]);
        HeaderWidget::new(self.config, self.device_kind, self.node).render(
            header_split[1],
            buf,
            mouse_areas,
        );

        for channel_index in 0..channel_count {
            let split = row_layout(rows[channel_index + 1]);
            let marked =
                self.selected && self.selected_channel == Some(channel_index);
            if marked {
                Span::styled(
                    &self.config.char_set.selector_middle,
                    self.config.theme.selector,
                )
                .render(split[0], buf);
            }
            ChannelRowWidget::new(self.config, self.node, channel_index)
                .render(split[1], buf, mouse_areas);
        }
    }
}

impl StatefulWidget for NodeWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        mouse_areas.extend([
            (
                area,
                smallvec![MouseEventKind::Down(MouseButton::Left)],
                smallvec![Action::SelectObject(self.node.object_id)],
            ),
            (
                area,
                smallvec![MouseEventKind::Down(MouseButton::Right)],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::SetDefault
                ],
            ),
            (
                area,
                smallvec![MouseEventKind::ScrollLeft],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::SetRelativeVolume(-0.01),
                ],
            ),
            (
                area,
                smallvec![MouseEventKind::ScrollRight],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::SetRelativeVolume(0.01),
                ],
            ),
        ]);

        let display = volume_display(self.channel_state, self.node);
        let cycling_channel = cycling_channel(
            self.channel_state,
            self.node,
            self.elapsed_seconds,
        );
        if display == VolumeDisplay::Stacked {
            self.render_channel_rows(
                area,
                buf,
                mouse_areas,
                self.node.volumes.len(),
            );
            return;
        }

        // Split area into a selection indicator on the left and the main node
        // area on the right
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(1), // selector_area
                Constraint::Min(0),    // node_area
            ])
            .split(area);
        let selector_area = layout[0];
        let node_area = layout[1];

        SelectorWidget::new(self.config, self.selected)
            .render(selector_area, buf);

        // Split the main node area into a header line and a line for the
        // volume bar and peak meter.
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header_area
                Constraint::Length(1), // bar_area
            ])
            .spacing(1)
            .flex(Flex::Legacy)
            .split(node_area);
        let header_area = layout[0];
        let bar_area = layout[1];

        HeaderWidget::new(self.config, self.device_kind, self.node).render(
            header_area,
            buf,
            mouse_areas,
        );

        // Render volume bar and (if enabled) peak meter
        if self.config.peaks == Peaks::Off {
            let layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![
                    Constraint::Length(2), // _padding
                    Constraint::Fill(9),   // volume_area
                    Constraint::Fill(1),   // _padding
                ])
                .split(bar_area);
            // index 0 is _padding
            let volume_area = layout[1];

            render_volume(
                self.config,
                self.node,
                display,
                cycling_channel,
                volume_area,
                buf,
                mouse_areas,
            );
        } else {
            let layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![
                    Constraint::Length(2), // _padding
                    Constraint::Fill(4),   // volume_area
                    Constraint::Fill(1),   // _padding
                    Constraint::Fill(4),   // meter_area
                    Constraint::Fill(1),   // _padding
                ])
                .split(bar_area);
            // index 0 is _padding
            let volume_area = layout[1];
            // index 2 is _padding
            let meter_area = layout[3];

            render_volume(
                self.config,
                self.node,
                display,
                cycling_channel,
                volume_area,
                buf,
                mouse_areas,
            );
            MeterWidget::new(self.config, self.node).render(meter_area, buf);
        }
    }
}

struct SelectorWidget<'a> {
    config: &'a Config,
    selected: bool,
}

impl<'a> SelectorWidget<'a> {
    fn new(config: &'a Config, selected: bool) -> Self {
        Self { config, selected }
    }
}

impl Widget for SelectorWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.selected {
            // Render and indication that this is the selected node.
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(area);

            let style = self.config.theme.selector;

            // Render the selected node indicator
            Span::styled(&self.config.char_set.selector_top, style)
                .render(rows[0], buf);
            Span::styled(&self.config.char_set.selector_middle, style)
                .render(rows[1], buf);
            Span::styled(&self.config.char_set.selector_bottom, style)
                .render(rows[2], buf);
        }
    }
}

struct HeaderWidget<'a> {
    config: &'a Config,
    device_kind: Option<DeviceKind>,
    node: &'a view::Node,
}

impl<'a> HeaderWidget<'a> {
    fn new(
        config: &'a Config,
        device_kind: Option<DeviceKind>,
        node: &'a view::Node,
    ) -> Self {
        Self {
            config,
            device_kind,
            node,
        }
    }

    fn target_line(&self) -> Line<'_> {
        match self.node.target {
            Some(view::Target::Default) => {
                // Add the default target indicator
                Line::from(vec![
                    Span::styled(
                        &self.config.char_set.default_stream,
                        self.config.theme.default_stream,
                    ),
                    Span::from(" "),
                    Span::styled(
                        &self.node.target_title,
                        self.config.theme.node_target,
                    ),
                ])
            }
            _ => Line::from(Span::styled(
                &self.node.target_title,
                self.config.theme.node_target,
            )),
        }
    }

    fn title_line(&self) -> Line<'_> {
        let default_span = if is_default(self.node, self.device_kind) {
            Span::styled(
                &self.config.char_set.default_device,
                self.config.theme.default_device,
            )
        } else {
            Span::from(" ")
        };
        Line::from(vec![
            default_span,
            Span::from(" "),
            Span::styled(&self.node.title, self.config.theme.node_title),
        ])
    }
}

impl StatefulWidget for HeaderWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        let target_line = self.target_line();
        let target_width = target_line.width().try_into().unwrap_or(u16::MAX);

        // See if we can fit the whole title on the screen. We'll scrap this
        // layout if it doesn't fit.
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                // Min(1) so we always show the default indicator
                Constraint::Min(1),               // title_area
                Constraint::Length(target_width), // target_area
            ])
            .horizontal_margin(1)
            .spacing(1)
            .split(area);
        let mut title_area = layout[0];
        let mut target_area = layout[1];

        let title_line = self.title_line();
        if title_line.width() > title_area.width as usize {
            // It doesn't fit
            let layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    // Min(1) so we always show the default indicator
                    Constraint::Min(1),    // title_area
                    Constraint::Length(3), // ellipses_area
                    Constraint::Length(1), // _padding
                    Constraint::Length(target_width), // target_area
                ])
                .horizontal_margin(1)
                .split(area);
            title_area = layout[0];
            let ellipses_area = layout[1];
            target_area = layout[3];

            Span::styled("...", self.config.theme.node_title)
                .render(ellipses_area, buf);
        }
        let (title_area, target_area) = (title_area, target_area);

        target_line
            .alignment(Alignment::Right)
            .render(target_area, buf);

        mouse_areas.push((
            target_area,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![
                Action::SelectObject(self.node.object_id),
                Action::ActivateDropdown
            ],
        ));

        title_line.render(title_area, buf);
    }
}

/// The first detected left/right channel pair on `node`, for a radiating
/// display to render. `None` if the node has no `positions` data at all,
/// or none of its channels pair (see `channel_pairing`) - callers fall
/// back to stacked (or the ordinary single-bar `VolumeWidget`) in that
/// case.
fn stereo_pair(node: &view::Node) -> Option<(usize, usize)> {
    let positions = node.positions.as_ref()?;
    channel_pairing::group_channels(positions)
        .into_iter()
        .find_map(|group| match group {
            ChannelGroup::Pair(left, right) => Some((left, right)),
            ChannelGroup::Single(_) => None,
        })
}

/// Renders a node's volume within a single-row area - the resolved
/// `display` must be `Unified` or `Radiating`; `Stacked` is handled
/// earlier in `NodeWidget::render()`, via `render_channel_rows`, since it
/// needs a taller multi-row area this function doesn't have.
fn render_volume(
    config: &Config,
    node: &view::Node,
    display: VolumeDisplay,
    cycling_channel: Option<usize>,
    area: Rect,
    buf: &mut Buffer,
    mouse_areas: &mut Vec<MouseArea>,
) {
    match display {
        VolumeDisplay::Radiating {
            left_index,
            right_index,
        } => {
            StereoVolumeWidget::new(config, node, left_index, right_index)
                .render(area, buf, mouse_areas);
        }
        VolumeDisplay::Unified | VolumeDisplay::Stacked => {
            VolumeWidget::new(config, node, cycling_channel).render(
                area,
                buf,
                mouse_areas,
            );
        }
    }
}

struct VolumeWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
    /// When `Some(index)`, render channel `index`'s own label+percentage
    /// instead of the whole-node mean - `unified_imbalance = "cycle"`'s
    /// per-render channel choice (see `cycling_channel`), resolved by
    /// the caller so this widget doesn't need to know about timing.
    cycling_channel: Option<usize>,
}

impl<'a> VolumeWidget<'a> {
    fn new(
        config: &'a Config,
        node: &'a view::Node,
        cycling_channel: Option<usize>,
    ) -> Self {
        Self {
            config,
            node,
            cycling_channel,
        }
    }
}

impl StatefulWidget for VolumeWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        let max_volume = self.config.max_volume_percent / 100.0;

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(5), // volume_label
                Constraint::Min(0),    // volume_bar
            ])
            .spacing(1)
            .split(area);
        let volume_label = layout[0];
        let volume_bar = layout[1];

        let volumes = &self.node.volumes;
        if !volumes.is_empty() {
            // unified_imbalance = "cycle": show the currently-cycled
            // channel's own value (both label and bar) instead of the
            // mean. The label packs the channel index and percentage
            // into the same 5-column budget the mean-only label already
            // used ("0 79%" / "1100%") - no extra width, matching the
            // "no width cost" goal from the original design discussion.
            let (volume, label) = if let Some(index) = self.cycling_channel {
                let raw = volumes.get(index).copied().unwrap_or(0.0);
                let volume = raw.cbrt();
                let percent = (volume * 100.0).round() as u32;
                let label = if percent >= 100 {
                    format!("{index}{percent}%")
                } else {
                    format!("{index} {percent}%")
                };
                (volume, label)
            } else {
                let mean = volumes.iter().sum::<f32>() / volumes.len() as f32;
                let volume = mean.cbrt();
                let percent = (volume * 100.0).round() as u32;
                (volume, format!("{percent}%"))
            };

            Line::from(Span::styled(label, self.config.theme.volume))
                .alignment(Alignment::Right)
                .render(volume_label, buf);

            let count = ((volume.clamp(0.0, max_volume) / max_volume)
                * volume_bar.width as f32)
                .round() as usize;

            let filled = self.config.char_set.volume_filled.repeat(count);
            let blank = self
                .config
                .char_set
                .volume_empty
                .repeat((volume_bar.width as usize).saturating_sub(count));
            Line::from(vec![
                Span::styled(filled, self.config.theme.volume_filled),
                Span::styled(blank, self.config.theme.volume_empty),
            ])
            .render(volume_bar, buf);
        }
        if self.node.mute {
            Line::from("muted").render(volume_label, buf);
        }

        mouse_areas.push((
            volume_label,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![
                Action::SelectObject(self.node.object_id),
                Action::ToggleMute
            ],
        ));

        // Add mouse areas for setting volume
        for i in 0..=volume_bar.width {
            let volume_area = Rect::new(
                volume_bar.x.saturating_add(i),
                volume_bar.y,
                1,
                volume_bar.height,
            );

            let volume_step = max_volume / volume_bar.width as f32;
            let volume = volume_step * i as f32;
            // Make the volume sticky around 100%. Otherwise it's often not
            // possible to select by mouse.
            let sticky_volume = if (1.0 - volume).abs() <= volume_step {
                1.0
            } else {
                volume
            };

            mouse_areas.push((
                volume_area,
                smallvec![
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Drag(MouseButton::Left),
                ],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::SetAbsoluteVolume(sticky_volume),
                ],
            ));
        }
    }
}

/// Two independent, radiating volume bars for a node's detected left/right
/// channel pair - one per channel, each growing outward from a shared
/// center marker, instead of `VolumeWidget`'s single bar averaging every
/// channel together. `left_index`/`right_index` index into the node's own
/// `volumes` (and correspond to its `positions`, per `channel_pairing`).
struct StereoVolumeWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
    left_index: usize,
    right_index: usize,
}

impl<'a> StereoVolumeWidget<'a> {
    fn new(
        config: &'a Config,
        node: &'a view::Node,
        left_index: usize,
        right_index: usize,
    ) -> Self {
        Self {
            config,
            node,
            left_index,
            right_index,
        }
    }
}

impl StatefulWidget for StereoVolumeWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        if self.node.mute {
            Line::from("muted")
                .alignment(Alignment::Center)
                .render(area, buf);
            mouse_areas.push((
                area,
                smallvec![MouseEventKind::Down(MouseButton::Left)],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::ToggleMute
                ],
            ));
            return;
        }

        let max_volume = self.config.max_volume_percent / 100.0;

        // Two separate Fill(1) constraints can end up with unequal widths
        // when the remaining space is odd (Ratatui's Fill distribution
        // isn't guaranteed symmetric) - identical numerically-equal L/R
        // volumes would then render as visibly different bar lengths just
        // from that width difference. Computing one shared bar_width and
        // giving both bars the same explicit Length(bar_width) makes that
        // impossible by construction; any odd leftover column is simply
        // unused rather than handed to one side.
        let fixed_width = 4 + 1 + 4; // label_l + center + label_r
        let spacing_width = 4; // 4 gaps between the 5 segments, at 1 each
        let bar_width =
            area.width.saturating_sub(fixed_width + spacing_width) / 2;

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(4),         // label_l
                Constraint::Length(bar_width), // bar_l
                Constraint::Length(1),         // center
                Constraint::Length(bar_width), // bar_r
                Constraint::Length(4),         // label_r
            ])
            .spacing(1)
            .split(area);
        let label_l = layout[0];
        let bar_l = layout[1];
        let center = layout[2];
        let bar_r = layout[3];
        let label_r = layout[4];

        let volumes = &self.node.volumes;
        let left_volume =
            volumes.get(self.left_index).copied().unwrap_or(0.0).cbrt();
        let right_volume =
            volumes.get(self.right_index).copied().unwrap_or(0.0).cbrt();

        Line::from(Span::styled(
            format!("{}%", (left_volume * 100.0).round() as u32),
            self.config.theme.volume,
        ))
        .alignment(Alignment::Right)
        .render(label_l, buf);

        Line::from(Span::styled(
            format!("{}%", (right_volume * 100.0).round() as u32),
            self.config.theme.volume,
        ))
        .render(label_r, buf);

        Line::from(Span::styled("|", self.config.theme.volume))
            .alignment(Alignment::Center)
            .render(center, buf);

        // Left bar: the filled portion sits adjacent to the center marker
        // (the bar's own right edge) and grows outward, away from center,
        // as volume increases.
        let left_count = ((left_volume.clamp(0.0, max_volume) / max_volume)
            * bar_l.width as f32)
            .round() as usize;
        Line::from(vec![
            Span::styled(
                self.config
                    .char_set
                    .volume_empty
                    .repeat((bar_l.width as usize).saturating_sub(left_count)),
                self.config.theme.volume_empty,
            ),
            Span::styled(
                self.config.char_set.volume_filled.repeat(left_count),
                self.config.theme.volume_filled,
            ),
        ])
        .render(bar_l, buf);

        // Right bar: mirror image of the left one - filled adjacent to
        // center, growing outward to the right.
        let right_count = ((right_volume.clamp(0.0, max_volume) / max_volume)
            * bar_r.width as f32)
            .round() as usize;
        Line::from(vec![
            Span::styled(
                self.config.char_set.volume_filled.repeat(right_count),
                self.config.theme.volume_filled,
            ),
            Span::styled(
                self.config
                    .char_set
                    .volume_empty
                    .repeat((bar_r.width as usize).saturating_sub(right_count)),
                self.config.theme.volume_empty,
            ),
        ])
        .render(bar_r, buf);

        for label_area in [label_l, label_r] {
            mouse_areas.push((
                label_area,
                smallvec![MouseEventKind::Down(MouseButton::Left)],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::ToggleMute
                ],
            ));
        }

        // Click-to-set mouse areas, per channel, each measured outward
        // from the center marker (column 0 = right at center) - mirrors
        // VolumeWidget's own per-column mouse area loop, just split
        // across two independently-clickable halves instead of one.
        for (bar, channel_index, from_center) in [
            (bar_l, self.left_index, true),
            (bar_r, self.right_index, false),
        ] {
            for i in 0..=bar.width {
                let x = if from_center {
                    bar.x.saturating_add(bar.width.saturating_sub(i))
                } else {
                    bar.x.saturating_add(i)
                };
                let volume_area = Rect::new(x, bar.y, 1, bar.height);

                let volume_step = max_volume / bar.width as f32;
                let volume = volume_step * i as f32;
                // Make the volume sticky around 100%. Otherwise it's often
                // not possible to select by mouse.
                let sticky_volume = if (1.0 - volume).abs() <= volume_step {
                    1.0
                } else {
                    volume
                };

                mouse_areas.push((
                    volume_area,
                    smallvec![
                        MouseEventKind::Down(MouseButton::Left),
                        MouseEventKind::Drag(MouseButton::Left),
                    ],
                    smallvec![
                        Action::SelectObject(self.node.object_id),
                        Action::SetChannelAbsoluteVolume(
                            channel_index,
                            sticky_volume
                        ),
                    ],
                ));
            }
        }
    }
}

/// A single channel's volume bar within Channel mode's stacked-row
/// display - one line, independently addressable, labeled with its real
/// channel name (`FL`, `AUX0`, ...) via `channel_pairing::channel_name`
/// when `positions` data is available, falling back to the raw channel
/// index otherwise.
struct ChannelRowWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
    channel_index: usize,
}

impl<'a> ChannelRowWidget<'a> {
    fn new(
        config: &'a Config,
        node: &'a view::Node,
        channel_index: usize,
    ) -> Self {
        Self {
            config,
            node,
            channel_index,
        }
    }
}

impl StatefulWidget for ChannelRowWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        let max_volume = self.config.max_volume_percent / 100.0;

        // Whenever a node's volume display is split, its monitor splits
        // too - one mono-style gauge per channel row, honoring whatever
        // the peaks config already says (off stays off; auto/mono both
        // render as a single mono gauge here regardless, since one
        // channel has nothing to show a left/right split of).
        let (row_area, meter_area) = if self.config.peaks == Peaks::Off {
            (area, None)
        } else {
            let layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![
                    Constraint::Fill(4), // row_area
                    Constraint::Fill(1), // _padding
                    Constraint::Fill(4), // meter_area
                ])
                .spacing(1)
                .split(area);
            (layout[0], Some(layout[2]))
        };

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(11), // volume_label: label_col + percent_col
                Constraint::Min(0),     // volume_bar
            ])
            .spacing(1)
            .split(row_area);
        let volume_label = layout[0];
        let volume_bar = layout[1];

        // label_col and percent_col are independently right-aligned, so a
        // channel's label always lands in the same column regardless of
        // how many digits its own (or any other row's) percentage has -
        // "FL 100%" / "FL  50%" / "FL   3%", not "FL 100%" / " FL 50%" /
        // "  FL 3%".
        let label_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(5), // label_col, e.g. "AUX63"
                Constraint::Length(5), // percent_col, e.g. "100%"/"muted"
            ])
            .spacing(1)
            .split(volume_label);
        let label_col = label_split[0];
        let percent_col = label_split[1];

        let volume = self
            .node
            .volumes
            .get(self.channel_index)
            .copied()
            .unwrap_or(0.0)
            .cbrt();
        let percent = (volume * 100.0).round() as u32;
        let channel_index = self.channel_index;
        let label = self
            .node
            .positions
            .as_ref()
            .and_then(|positions| positions.get(channel_index))
            .map(|&position| channel_pairing::channel_name(position))
            .unwrap_or_else(|| channel_index.to_string());

        Line::from(Span::styled(&label, self.config.theme.volume))
            .alignment(Alignment::Right)
            .render(label_col, buf);

        Line::from(Span::styled(
            format!("{percent}%"),
            self.config.theme.volume,
        ))
        .alignment(Alignment::Right)
        .render(percent_col, buf);

        let count = ((volume.clamp(0.0, max_volume) / max_volume)
            * volume_bar.width as f32)
            .round() as usize;
        let filled = self.config.char_set.volume_filled.repeat(count);
        let blank = self
            .config
            .char_set
            .volume_empty
            .repeat((volume_bar.width as usize).saturating_sub(count));
        Line::from(vec![
            Span::styled(filled, self.config.theme.volume_filled),
            Span::styled(blank, self.config.theme.volume_empty),
        ])
        .render(volume_bar, buf);

        if self.node.mute {
            Line::from("muted")
                .alignment(Alignment::Right)
                .render(percent_col, buf);
        }

        mouse_areas.push((
            volume_label,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![
                Action::SelectObject(self.node.object_id),
                Action::SelectChannel(channel_index),
                Action::ToggleMute
            ],
        ));

        // Click-to-set mouse areas, mirroring VolumeWidget's own
        // per-column loop, but dispatching to just this channel.
        for i in 0..=volume_bar.width {
            let x = volume_bar.x.saturating_add(i);
            let volume_area = Rect::new(x, volume_bar.y, 1, volume_bar.height);

            let volume_step = max_volume / volume_bar.width as f32;
            let volume = volume_step * i as f32;
            let sticky_volume = if (1.0 - volume).abs() <= volume_step {
                1.0
            } else {
                volume
            };

            mouse_areas.push((
                volume_area,
                smallvec![
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Drag(MouseButton::Left),
                ],
                smallvec![
                    Action::SelectObject(self.node.object_id),
                    Action::SelectChannel(channel_index),
                    Action::SetChannelAbsoluteVolume(
                        channel_index,
                        sticky_volume
                    ),
                ],
            ));
        }

        if let Some(meter_area) = meter_area {
            let peak = self
                .node
                .peaks
                .as_deref()
                .and_then(|peaks| peaks.get(channel_index))
                .map(|peak| peak.load());
            meter::render_mono(meter_area, buf, peak, self.config);
            self.node.peaks_dirty.store(false, Ordering::Relaxed);
        }
    }
}

struct MeterWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
}

impl<'a> MeterWidget<'a> {
    fn new(config: &'a Config, node: &'a view::Node) -> Self {
        Self { config, node }
    }
}

impl Widget for MeterWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self.node.peaks.as_deref() {
            Some([left, right]) if self.config.peaks != Peaks::Mono => {
                meter::render_stereo(
                    area,
                    buf,
                    Some((left.load(), right.load())),
                    self.config,
                )
            }
            Some(peaks @ [..]) => {
                let peaks = (!peaks.is_empty()).then_some(
                    peaks.iter().map(|peak| peak.load()).sum::<f32>()
                        / peaks.len() as f32,
                );
                meter::render_mono(area, buf, peaks, self.config)
            }
            _ => match self
                .node
                .positions
                .as_ref()
                .map(|positions| positions.len())
            {
                Some(2) if self.config.peaks != Peaks::Mono => {
                    meter::render_stereo(area, buf, None, self.config)
                }
                _ => meter::render_mono(area, buf, None, self.config),
            },
        }

        self.node.peaks_dirty.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::wirehose::ObjectId;
    use std::sync::atomic::AtomicBool;

    fn test_node(positions: Option<Vec<u32>>, volumes: Vec<f32>) -> view::Node {
        view::Node {
            object_id: ObjectId::from_raw_id(1),
            object_serial: 1,
            name: String::from("Test node"),
            title: String::from("Test node"),
            media_class: String::from("Stream/Output/Audio"),
            routes: None,
            target_title: String::new(),
            target: None,
            volumes,
            mute: false,
            peaks: None,
            peaks_dirty: std::sync::Arc::new(AtomicBool::new(false)),
            positions,
            device_info: None,
            is_default_sink: false,
            is_default_source: false,
            client_id: None,
        }
    }

    fn render_to_string(config: &Config, node: &view::Node) -> String {
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let channel_state = ChannelState {
            channel_mode: false,
            channel_display: config.channel_display,
            unified_imbalance: config.unified_imbalance,
            split_style: config.split_style,
        };
        let display = volume_display(channel_state, node);
        let cycling_channel = cycling_channel(channel_state, node, 0.0);
        render_volume(
            config,
            node,
            display,
            cycling_channel,
            area,
            &mut buf,
            &mut Vec::new(),
        );
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "));
        }
        line
    }

    /// Renders a full `NodeWidget` (not just its volume bar) and returns
    /// every line, sized exactly to `NodeWidget::node_height` for the
    /// given mode/node so a channel-mode node's stacked rows are all
    /// captured.
    fn render_node_lines(
        config: &Config,
        node: &view::Node,
        channel_mode: bool,
        selected: bool,
        selected_channel: Option<usize>,
    ) -> Vec<String> {
        let channel_state = ChannelState {
            channel_mode,
            channel_display: config.channel_display,
            unified_imbalance: config.unified_imbalance,
            split_style: config.split_style,
        };
        let height = NodeWidget::node_height(channel_state, node);
        let area = Rect::new(0, 0, 40, height);
        let mut buf = Buffer::empty(area);
        NodeWidget::new(
            config,
            None,
            node,
            selected,
            channel_state,
            selected_channel,
            0.0,
        )
        .render(area, &mut buf, &mut Vec::new());

        (0..height)
            .map(|y| {
                let mut line = String::new();
                for x in 0..area.width {
                    line.push_str(
                        buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "),
                    );
                }
                line
            })
            .collect()
    }

    #[test]
    fn stereo_radiating_bars_have_symmetric_width_for_equal_volumes() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(
            Some(vec![fl, fr]),
            vec![0.5_f32.powi(3), 0.5_f32.powi(3)],
        );
        let config =
            config::Config::from_toml_str("channel_display = \"always\"");

        // render_to_string uses a 40-wide area - the remaining bar space
        // after fixed segments (40 - 13 = 27) is odd, exactly the case
        // that used to give the two Fill(1) bars unequal widths.
        let rendered = render_to_string(&config, &node);

        let filled = config.char_set.volume_filled.as_str();
        let center_index = rendered.find('|').expect("center marker present");
        let left_filled = rendered[..center_index].matches(filled).count();
        let right_filled = rendered[center_index + 1..].matches(filled).count();
        assert_eq!(
            left_filled, right_filled,
            "equal L/R volumes must fill an equal number of characters on each side"
        );
    }

    #[test]
    fn stereo_pair_finds_the_first_named_lr_pair() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        assert_eq!(stereo_pair(&node), Some((0, 1)));
    }

    #[test]
    fn stereo_pair_none_without_positions() {
        let node = test_node(None, vec![1.0, 0.0]);
        assert_eq!(stereo_pair(&node), None);
    }

    #[test]
    fn stereo_pair_none_for_generic_aux_channels() {
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;
        let node = test_node(Some(vec![aux0, aux1]), vec![1.0, 0.0]);
        assert_eq!(stereo_pair(&node), None);
    }

    #[test]
    fn channel_display_unified_renders_single_averaged_bar() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        // Left at 100%, right at 0% (raw linear volumes, i.e. the cubes of
        // the displayed percentages) - VolumeWidget's existing behavior
        // (untouched by this feature) averages the *raw* values first
        // (mean 0.5) and only then takes the cube root for display:
        // cbrt(0.5) * 100 ~= 79%, not a 50/50 average of the displayed
        // percentages themselves.
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let rendered = render_to_string(&config, &node);

        // Single combined label, not two independent ones.
        assert!(rendered.contains("79%"));
        assert!(!rendered.contains("100%"));
        assert!(!rendered.contains("0%"));
    }

    #[test]
    fn unified_imbalance_cycle_shows_one_channels_own_value() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        // test_node() always uses object_id 1, which (per the
        // hand-verified phase_offset_differs_by_object_id test) lands on
        // channel 0 at elapsed_seconds = 0.0 - render_to_string always
        // renders at 0.0.
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str(
            "channel_display = \"unified\"\nunified_imbalance = \"cycle\"",
        );

        let rendered = render_to_string(&config, &node);

        // Channel 0's own 100% (not the 79% mean the same node showed in
        // the unified_imbalance = "none" test above).
        assert!(rendered.contains("0100%"));
        assert!(!rendered.contains("79%"));
    }

    #[test]
    fn channel_display_always_renders_independent_percentages() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config =
            config::Config::from_toml_str("channel_display = \"always\"");

        let rendered = render_to_string(&config, &node);

        // Both channels' real values show up independently - not a 50%
        // average masking the actual 100%/0% split.
        assert!(rendered.contains("100%"));
        assert!(rendered.contains("0%"));
        assert!(!rendered.contains("50%"));
    }

    #[test]
    fn channel_display_always_without_a_pair_falls_back_to_single_bar() {
        // Mono - nothing to pair, so this must render exactly like
        // channel_display was left "unified", even though it's "always".
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let node = test_node(Some(vec![mono]), vec![0.5_f32.powi(3)]);
        let config =
            config::Config::from_toml_str("channel_display = \"always\"");

        let rendered = render_to_string(&config, &node);

        assert!(rendered.contains("50%"));
    }

    fn channel_state(channel_mode: bool) -> ChannelState {
        ChannelState {
            channel_mode,
            channel_display: Default::default(),
            unified_imbalance: Default::default(),
            split_style: Default::default(),
        }
    }

    #[test]
    fn volume_display_defaults_to_unified() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]);
        assert_eq!(
            volume_display(channel_state(false), &node),
            VolumeDisplay::Unified
        );
    }

    #[test]
    fn volume_display_channel_mode_always_wins() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);
        let mut state = channel_state(true);
        state.channel_display = ChannelDisplay::Always;
        state.split_style = SplitStyle::Radiating;
        // Even with a detected pair and split_style = radiating,
        // channel_mode forces Stacked.
        assert_eq!(volume_display(state, &node), VolumeDisplay::Stacked);
    }

    #[test]
    fn volume_display_always_radiates_for_a_detected_pair() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        state.split_style = SplitStyle::Radiating;
        assert_eq!(
            volume_display(state, &node),
            VolumeDisplay::Radiating {
                left_index: 0,
                right_index: 1
            }
        );
    }

    #[test]
    fn volume_display_always_falls_back_to_stacked_without_a_pair() {
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;
        let node = test_node(Some(vec![aux0, aux1]), vec![0.5, 0.5]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        state.split_style = SplitStyle::Radiating;
        // AUX0/AUX1 never pair, so radiating has nothing to grow from
        // center - falls back to stacked even though split_style asked
        // for radiating.
        assert_eq!(volume_display(state, &node), VolumeDisplay::Stacked);
    }

    #[test]
    fn volume_display_always_stacked_when_configured() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        state.split_style = SplitStyle::Stacked;
        assert_eq!(volume_display(state, &node), VolumeDisplay::Stacked);
    }

    #[test]
    fn volume_display_unified_imbalance_none_stays_unified() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Unified;
        state.unified_imbalance = UnifiedImbalance::None;
        assert_eq!(volume_display(state, &node), VolumeDisplay::Unified);
    }

    #[test]
    fn volume_display_unified_imbalance_split_only_splits_when_imbalanced() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Unified;
        state.unified_imbalance = UnifiedImbalance::Split;
        state.split_style = SplitStyle::Radiating;

        let balanced = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);
        assert_eq!(
            volume_display(state, &balanced),
            VolumeDisplay::Unified,
            "balanced node should stay collapsed"
        );

        let imbalanced = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]);
        assert_eq!(
            volume_display(state, &imbalanced),
            VolumeDisplay::Radiating {
                left_index: 0,
                right_index: 1
            },
            "imbalanced node should split"
        );
    }

    #[test]
    fn volume_display_single_channel_always_unified() {
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let node = test_node(Some(vec![mono]), vec![0.5]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        // Nothing to split - a single channel is unified regardless of
        // every other axis.
        assert_eq!(volume_display(state, &node), VolumeDisplay::Unified);
    }

    #[test]
    fn cycling_channel_requires_unified_display() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]); // imbalanced

        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        state.unified_imbalance = UnifiedImbalance::Cycle;
        assert_eq!(cycling_channel(state, &node, 0.0), None);
    }

    #[test]
    fn cycling_channel_requires_unified_imbalance_cycle() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]); // imbalanced

        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Unified;
        state.unified_imbalance = UnifiedImbalance::None;
        assert_eq!(cycling_channel(state, &node, 0.0), None);

        state.unified_imbalance = UnifiedImbalance::Split;
        assert_eq!(cycling_channel(state, &node, 0.0), None);
    }

    #[test]
    fn cycling_channel_none_when_balanced() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);

        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Unified;
        state.unified_imbalance = UnifiedImbalance::Cycle;
        assert_eq!(cycling_channel(state, &node, 0.0), None);
    }

    #[test]
    fn cycling_channel_advances_after_one_full_interval() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]);

        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Unified;
        state.unified_imbalance = UnifiedImbalance::Cycle;

        let first =
            cycling_channel(state, &node, 0.0).expect("imbalanced node");
        let later = cycling_channel(state, &node, CYCLE_INTERVAL_SECONDS)
            .expect("still imbalanced");
        assert_ne!(
            first, later,
            "one full interval later should show the other channel"
        );
    }

    #[test]
    fn cycling_channel_phase_offset_differs_by_object_id() {
        // Hand-verified against the phase formula: phase = (id % 997) /
        // 997, position = elapsed/interval + phase * channel_count.
        // id=1 -> phase ~0.001 -> position ~0.002 -> channel 0.
        // id=500 -> phase ~0.502 -> position ~1.004 -> channel 1.
        // Two nodes with the same imbalance, observed at the same
        // instant, landing on different channels - proving they aren't
        // locked to one shared cursor.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let mut node_a = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]);
        node_a.object_id = ObjectId::from_raw_id(1);
        let mut node_b = test_node(Some(vec![fl, fr]), vec![0.5, 0.3]);
        node_b.object_id = ObjectId::from_raw_id(500);

        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Unified;
        state.unified_imbalance = UnifiedImbalance::Cycle;

        assert_eq!(cycling_channel(state, &node_a, 0.0), Some(0));
        assert_eq!(cycling_channel(state, &node_b, 0.0), Some(1));
    }

    #[test]
    fn node_height_default_is_unaffected_by_channel_count() {
        let node = test_node(None, vec![1.0, 1.0, 1.0]);
        assert_eq!(
            NodeWidget::node_height(channel_state(false), &node),
            NodeWidget::height()
        );
    }

    #[test]
    fn node_height_channel_mode_expands_one_line_per_channel() {
        let node = test_node(None, vec![1.0, 1.0, 1.0]);
        // 1 header line + 3 channel lines
        assert_eq!(NodeWidget::node_height(channel_state(true), &node), 4);
    }

    #[test]
    fn node_height_channel_mode_single_channel_unaffected() {
        let node = test_node(None, vec![1.0]);
        assert_eq!(
            NodeWidget::node_height(channel_state(true), &node),
            NodeWidget::height()
        );
    }

    #[test]
    fn channel_mode_renders_one_line_per_channel() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        assert_eq!(lines.len(), 3); // header + 2 channel rows
                                    // label_col and percent_col are independent fixed-width columns
                                    // (5 + 1 spacing + 5), so the gap between label and percent
                                    // varies with the percentage's own digit count - not a single
                                    // "{label} {percent}%" string right-aligned as one unit.
        assert!(lines[1].contains("FL  100%"));
        assert!(lines[2].contains("FR    0%"));
    }

    #[test]
    fn channel_mode_meter_shows_each_channels_own_peak() {
        use crate::atomic_f32::AtomicF32;
        use std::sync::Arc;

        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let mut node = test_node(Some(vec![fl, fr]), vec![1.0, 1.0]);
        // Channel 0 loud, channel 1 silent - if each row read a shared or
        // averaged peak instead of its own channel's, both rows would
        // show the same fill.
        node.peaks =
            Some(Arc::from([AtomicF32::new(1.0), AtomicF32::new(0.0)]));
        // extracompat uses distinct glyphs for active ('#') vs inactive
        // ('=') meter cells - the default char_set uses the same glyph
        // for both (styled by color only), which a plain-text render
        // can't distinguish.
        let config = config::Config::from_toml_str(
            "char_set = \"extracompat\"\npeaks = \"auto\"",
        );

        let lines = render_node_lines(&config, &node, true, false, None);

        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains('#'), "loud channel should show fill");
        assert!(
            !lines[2].contains('#'),
            "silent channel should show no fill, proving it read its own \
             peak rather than channel 0's"
        );
    }

    #[test]
    fn channel_mode_label_and_percent_are_independently_aligned_columns() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        // Very different percent widths (100% vs 3%) - a shared
        // right-aligned "{label} {percent}%" string would shift the
        // labels relative to each other; independent columns keep them
        // in the same place regardless.
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.03_f32.powi(3)]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        let fl_index = lines[1].find("FL").expect("FL label present");
        let fr_index = lines[2].find("FR").expect("FR label present");
        assert_eq!(
            fl_index, fr_index,
            "labels should start at the same column regardless of percent width"
        );
    }

    #[test]
    fn channel_mode_labels_fall_back_to_index_without_positions() {
        let node = test_node(None, vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        assert!(lines[1].contains("0  100%"));
        assert!(lines[2].contains("1    0%"));
    }

    #[test]
    fn channel_mode_labels_aux_channels_by_number() {
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;
        let node = test_node(Some(vec![aux0, aux1]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        assert!(lines[1].contains("AUX0  100%"));
        assert!(lines[2].contains("AUX1    0%"));
    }

    #[test]
    fn channel_mode_off_ignores_selected_channel() {
        // Regression guard: node_height/rendering must fall back to the
        // ordinary single-bar layout when channel_mode is off, even if a
        // stale selected_channel is still set from a previous mode.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, false, true, Some(1));

        assert_eq!(lines.len(), 3);
        assert_eq!(NodeWidget::node_height(channel_state(false), &node), 3);
    }

    #[test]
    fn channel_mode_marks_only_the_selected_channel_row() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");
        let marker = config.char_set.selector_middle.as_str();

        let channel_0_marked =
            render_node_lines(&config, &node, true, true, Some(0));
        assert!(channel_0_marked[1].contains(marker));
        assert!(!channel_0_marked[2].contains(marker));

        let channel_1_marked =
            render_node_lines(&config, &node, true, true, Some(1));
        assert!(!channel_1_marked[1].contains(marker));
        assert!(channel_1_marked[2].contains(marker));

        let not_selected =
            render_node_lines(&config, &node, true, false, Some(0));
        assert!(!not_selected[1].contains(marker));
        assert!(!not_selected[2].contains(marker));
    }

    #[test]
    fn channel_mode_never_marks_the_header_row() {
        // The header row never shows the selector marker in Channel mode,
        // even when the node is selected - only the individually-targeted
        // channel row should, so there's exactly one marker to look for.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");
        let marker = config.char_set.selector_middle.as_str();

        let lines = render_node_lines(&config, &node, true, true, Some(0));
        assert!(!lines[0].contains(marker));
    }
}
