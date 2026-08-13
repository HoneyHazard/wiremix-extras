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
    ChannelDisplay, ChannelState, ChannelView, Config, MeterLayout,
    PairLabelStyle, Peaks, SplitStyle, UnifiedImbalance,
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
    /// Only chosen when a node's *entire* channel layout is exactly one
    /// detected left/right pair and nothing else - the common case
    /// (plain stereo hardware), rendered at the classic fixed height.
    /// Anything with more channels than that (extra unpaired channels
    /// alongside a pair, more than one pair, or no pair at all) uses
    /// `Stacked` instead, so nothing is ever silently left off-screen.
    Radiating {
        left_index: usize,
        right_index: usize,
    },
    /// One row per `display_rows()` group: a detected left/right pair
    /// becomes its own radiating row when split_style = "radiating",
    /// otherwise (or for any unpaired channel) it's one row per channel.
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
        SplitStyle::Radiating => match single_pair(node) {
            Some((left_index, right_index)) => VolumeDisplay::Radiating {
                left_index,
                right_index,
            },
            // Either no pair at all, or more going on than just one
            // simple pair (extra channels, or more than one pair) -
            // `Stacked`'s per-group layout handles both.
            None => VolumeDisplay::Stacked,
        },
    }
}

/// The row layout `Stacked` should use: one entry per row, in render
/// order. A detected left/right pair collapses to a single
/// `ChannelGroup::Pair` row (rendered as a radiating mini-bar) only when
/// split_style = "radiating" and channel_mode is off - Channel mode
/// always wants every raw channel individually addressable (radiating's
/// marker-placement problem for a cursored channel isn't solved), and
/// split_style = "stacked" means exactly what it says. Without
/// `positions` data there's nothing to pair from, so every channel is
/// its own row regardless.
fn display_rows(
    channel_state: ChannelState,
    node: &view::Node,
) -> Vec<ChannelGroup> {
    let per_channel =
        || (0..node.volumes.len()).map(ChannelGroup::Single).collect();

    if channel_state.channel_mode
        || channel_state.split_style == SplitStyle::Stacked
    {
        return per_channel();
    }

    match node.positions.as_ref() {
        Some(positions) => channel_pairing::group_channels(positions),
        None => per_channel(),
    }
}

/// How long each channel is shown before `unified_imbalance = "cycle"`
/// advances to the next one. Not user-configurable yet (see
/// `NOTES-multichannel.md` §10) - a reasonable fixed value for a first
/// version, in the 0.5-2s range floated during design.
const CYCLE_INTERVAL_SECONDS: f32 = 1.5;

/// Splits a bar/meter row into its `volume_area` and `meter_area` for a
/// given view's `MeterLayout`. The volume:meter ratio (`meter_width_percent`,
/// or stock's own `4:4` when unset) is applied first, as a pure two-way
/// `Fill` split with nothing else in the list - so `gap`/`right_margin`
/// can never shrink the volume side's own share, only the meter side's,
/// regardless of how wide either is set to. `gap`/`right_margin` are
/// then carved out of the meter side alone as fixed-`Length` blank
/// columns (leading/trailing respectively), which is also why they're
/// plain `u16` rather than `Option` - there's no "proportional" mode
/// left for either to fall back to.
fn split_meter_row(area: Rect, layout: MeterLayout) -> (Rect, Rect) {
    let (volume_weight, meter_weight) = match layout.meter_width_percent {
        Some(percent) => {
            let meter = percent.round() as u16;
            (100 - meter, meter)
        }
        None => (4, 4), // stock's literal Fill(4)/Fill(4)
    };
    let sides = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(volume_weight),
            Constraint::Fill(meter_weight),
        ])
        .split(area);
    let volume_area = sides[0];
    let meter_side = sides[1];
    let gap = effective_gap(layout.gap, meter_side.width);
    let meter_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(gap),
            Constraint::Min(0),
            Constraint::Length(layout.right_margin),
        ])
        .split(meter_side);
    (volume_area, meter_layout[1])
}

/// `layout.gap`'s configured value is a floor, not a fixed override -
/// the actual gap scales up with the meter side's own available width
/// (1-in-8, matching stock's own historical gap proportion) once
/// there's enough room for that to exceed the floor, rather than
/// staying visually cramped in a wide terminal.
fn effective_gap(min_gap: u16, meter_side_width: u16) -> u16 {
    min_gap.max(meter_side_width.div_ceil(8))
}

/// The `volume_area` for a bar-only row (`peaks = "off"`, nothing to gap
/// or split a meter against) - the whole row, minus `right_margin`
/// carved off its trailing edge. Unlike `split_meter_row`, `right_margin`
/// here necessarily comes out of the volume side, since there's no
/// meter side to carve it from instead.
fn peaks_off_volume_area(area: Rect, layout: MeterLayout) -> Rect {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(layout.right_margin),
        ])
        .split(area)[0]
}

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
    /// header line plus one line per `display_rows()` group (a detected
    /// pair rendered as radiating counts as one row, same as any single
    /// channel); everything else renders at the ordinary fixed
    /// `height()`.
    pub fn node_height(channel_state: ChannelState, node: &view::Node) -> u16 {
        match volume_display(channel_state, node) {
            VolumeDisplay::Stacked => {
                1 + display_rows(channel_state, node).len() as u16
            }
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

    /// Split display: one header line (title/target, same as the ordinary
    /// layout) followed by one line per `display_rows()` group - a single
    /// channel's own row, or (split_style = "radiating", linked setting)
    /// a detected pair's radiating row. See `node_height` for the
    /// matching height calculation.
    fn render_channel_rows(
        &self,
        area: Rect,
        buf: &mut Buffer,
        mouse_areas: &mut Vec<MouseArea>,
        groups: &[ChannelGroup],
    ) {
        let mut constraints = vec![Constraint::Length(1)]; // header_row
        constraints.extend(
            std::iter::repeat(Constraint::Length(1)).take(groups.len()),
        );
        let row_areas = Layout::default()
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

        // In Channel mode (individual setting), the header row deliberately
        // never shows the selector marker - only the individually-targeted
        // channel row does, so there's exactly one place to look for
        // "which channel is this" rather than two markers competing for
        // attention. But when display is split for some other reason
        // (linked mode's split_style = "stacked", a pair-less device like
        // AUX0/AUX1 falling back from "radiating", or a radiating pair
        // row - which is never individually targetable) there is no
        // individually-targeted channel at all - the whole node is still
        // the selection unit, so the marker needs to span *every* row in
        // the block, not just the header - a continuous top/middle/.../
        // bottom bracket, the same visual language the classic 2-line
        // node's own `SelectorWidget` already uses (top cap on its header
        // line, bottom cap on its bar line) generalized to however many
        // rows this block has. A marker on the header row alone left every
        // row below it looking like it belonged to no selection at all.
        let header_split = row_layout(row_areas[0]);
        let header_marked = self.selected && !self.channel_state.channel_mode;
        if header_marked {
            let top_char = if groups.is_empty() {
                &self.config.char_set.selector_middle
            } else {
                &self.config.char_set.selector_top
            };
            Span::styled(top_char, self.config.theme.selector)
                .render(header_split[0], buf);
        }
        HeaderWidget::new(self.config, self.device_kind, self.node).render(
            header_split[1],
            buf,
            mouse_areas,
        );

        // Whenever split_style = "radiating" (and channel_mode is off -
        // display_rows never produces a Pair group otherwise), every row
        // - a detected pair or a lone unpaired channel alike - shares
        // the same column grid: label | left/unpaired % | left/unpaired
        // bar | divider-or-blank | right bar-or-blank | right %-or-
        // blank. An unpaired channel simply occupies the left half of
        // that grid and leaves the right half empty, rather than
        // stretching its own bar across the whole row - so its bar
        // starts and ends at exactly the same columns a paired row's
        // left bar would, and represents the same volume-to-character
        // scale. Channel mode and split_style = "stacked" keep the
        // plain, left-aligned, full-width `ChannelRowWidget` instead -
        // there's no radiating row anywhere in the block to stay
        // consistent with there.
        let radiating_context = !self.channel_state.channel_mode
            && self.channel_state.split_style == SplitStyle::Radiating;
        let bar_width = radiating_context.then(|| {
            let content_width = Self::split_row_content_width(
                self.config,
                self.channel_state.view(),
                row_layout(row_areas[1])[1],
            );
            RadiatingRowWidget::bar_width(content_width)
        });

        for (row_index, group) in groups.iter().enumerate() {
            let split = row_layout(row_areas[row_index + 1]);
            if header_marked {
                let is_last = row_index == groups.len() - 1;
                let selector_char = if is_last {
                    &self.config.char_set.selector_bottom
                } else {
                    &self.config.char_set.selector_middle
                };
                Span::styled(selector_char, self.config.theme.selector)
                    .render(split[0], buf);
            }
            match *group {
                ChannelGroup::Single(channel_index) => {
                    let marked = self.channel_state.channel_mode
                        && self.selected
                        && self.selected_channel == Some(channel_index);
                    if marked {
                        Span::styled(
                            &self.config.char_set.selector_middle,
                            self.config.theme.selector,
                        )
                        .render(split[0], buf);
                    }
                    if radiating_context {
                        RadiatingRowWidget::new(
                            self.config,
                            self.node,
                            channel_index,
                            None,
                            self.channel_state.pair_label_style,
                            bar_width.expect(
                                "radiating_context is exactly the \
                                 condition bar_width was computed from",
                            ),
                            self.channel_state.view(),
                        )
                        .render(
                            split[1],
                            buf,
                            mouse_areas,
                        );
                    } else {
                        ChannelRowWidget::new(
                            self.config,
                            self.node,
                            channel_index,
                            self.channel_state.view(),
                        )
                        .render(
                            split[1],
                            buf,
                            mouse_areas,
                        );
                    }
                }
                ChannelGroup::Pair(left_index, right_index) => {
                    RadiatingRowWidget::new(
                        self.config,
                        self.node,
                        left_index,
                        Some(right_index),
                        self.channel_state.pair_label_style,
                        bar_width.expect(
                            "a pair row's presence implies radiating_context, \
                             the condition bar_width was computed from",
                        ),
                        self.channel_state.view(),
                    )
                    .render(split[1], buf, mouse_areas);
                }
            }
        }
    }

    /// The width available for a split row's own content once the peak
    /// meter (if enabled) has claimed its share - matches the
    /// row/meter split every `Stacked` row widget applies internally, so
    /// a bar_width computed from this figure lines up with what those
    /// widgets will actually do with the same area.
    fn split_row_content_width(
        config: &Config,
        view: ChannelView,
        row_area: Rect,
    ) -> u16 {
        let layout = config.meter_layout(view);
        if config.peaks == Peaks::Off {
            peaks_off_volume_area(row_area, layout).width
        } else {
            split_meter_row(row_area, layout).0.width
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
            let groups = display_rows(self.channel_state, self.node);
            self.render_channel_rows(area, buf, mouse_areas, &groups);
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

        // Every view (including Unified) shares the same generic
        // volume/meter split now - see `split_meter_row`/
        // `peaks_off_volume_area`.
        let view = self.channel_state.view();
        let meter_layout = self.config.meter_layout(view);

        // Render volume bar and (if enabled) peak meter
        if self.config.peaks == Peaks::Off {
            let volume_area = peaks_off_volume_area(bar_area, meter_layout);

            render_volume(
                self.config,
                self.node,
                VolumeContext {
                    channel_state: self.channel_state,
                    display,
                    cycling_channel,
                },
                volume_area,
                buf,
                mouse_areas,
            );
        } else {
            let (volume_area, meter_area) =
                split_meter_row(bar_area, meter_layout);

            render_volume(
                self.config,
                self.node,
                VolumeContext {
                    channel_state: self.channel_state,
                    display,
                    cycling_channel,
                },
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

/// `Some((left, right))` only when `node`'s *entire* channel layout is
/// exactly one detected left/right pair and nothing else - the common
/// case (plain stereo hardware), which fits the classic single-row,
/// fixed-height `VolumeDisplay::Radiating`. `None` for anything else
/// (no positions data, no pair at all, extra unpaired channels alongside
/// a pair, or more than one pair) - those all need `Stacked`'s per-group
/// row layout instead, so nothing ends up silently unrendered.
fn single_pair(node: &view::Node) -> Option<(usize, usize)> {
    let positions = node.positions.as_ref()?;
    let mut groups = channel_pairing::group_channels(positions).into_iter();
    match (groups.next(), groups.next()) {
        (Some(ChannelGroup::Pair(left, right)), None) => Some((left, right)),
        _ => None,
    }
}

/// `RadiatingRowWidget`'s leading label. For a detected pair
/// (`right_index: Some`) this is the pair's `channel_pairing::
/// pair_group_name` - "F" (`PairLabelStyle::Short`) or "F L|R"
/// (`PairLabelStyle::Verbose`) for the FL/FR pair, and so on - never the
/// individual channel's own name, which would be indistinguishable from
/// an actual single-channel row showing that exact channel. For an
/// unpaired channel (`right_index: None`) it's just that channel's own
/// `channel_name`, same as `ChannelRowWidget` uses - there's no pair to
/// name here, only the one real channel.
fn radiating_row_label(
    node: &view::Node,
    left_index: usize,
    right_index: Option<usize>,
    style: PairLabelStyle,
) -> String {
    let Some(right_index) = right_index else {
        return node
            .positions
            .as_ref()
            .and_then(|positions| positions.get(left_index))
            .map(|&position| channel_pairing::channel_name(position))
            .unwrap_or_else(|| left_index.to_string());
    };

    let group = node
        .positions
        .as_ref()
        .and_then(|positions| {
            let left = *positions.get(left_index)?;
            let right = *positions.get(right_index)?;
            channel_pairing::pair_group_name(left, right)
        })
        .unwrap_or("?");

    match style {
        PairLabelStyle::Short => group.to_string(),
        PairLabelStyle::Verbose => format!("{group} L|R"),
    }
}

/// Bundles `render_volume`'s per-render context args (as opposed to
/// `config`/`node`/`area`/`buf`/`mouse_areas`, which every render call
/// needs regardless) into one `Copy` value, keeping the function's own
/// argument count down.
#[derive(Clone, Copy)]
struct VolumeContext {
    channel_state: ChannelState,
    display: VolumeDisplay,
    cycling_channel: Option<usize>,
}

/// Renders a node's volume within a single-row area - the resolved
/// `display` must be `Unified` or `Radiating`; `Stacked` is handled
/// earlier in `NodeWidget::render()`, via `render_channel_rows`, since it
/// needs a taller multi-row area this function doesn't have.
fn render_volume(
    config: &Config,
    node: &view::Node,
    ctx: VolumeContext,
    area: Rect,
    buf: &mut Buffer,
    mouse_areas: &mut Vec<MouseArea>,
) {
    match ctx.display {
        VolumeDisplay::Radiating {
            left_index,
            right_index,
        } => {
            StereoVolumeWidget::new(config, node, left_index, right_index)
                .render(area, buf, mouse_areas);
        }
        VolumeDisplay::Unified | VolumeDisplay::Stacked => {
            VolumeWidget::new(
                config,
                node,
                ctx.channel_state,
                ctx.cycling_channel,
            )
            .render(area, buf, mouse_areas);
        }
    }
}

struct VolumeWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
    /// Whether the overall list is currently in `Unified` view (as
    /// opposed to `Linked`/`Channels`) - decides which of two label
    /// widths this row's bar should line up with: stock's own narrow
    /// one (nothing else in a stock-settings Unified-view list ever
    /// needs to stay aligned with it), or the wider one every Linked/
    /// Channels row shares (see `VOLUME_LABEL_WIDTH`'s doc comment).
    channel_state: ChannelState,
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
        channel_state: ChannelState,
        cycling_channel: Option<usize>,
    ) -> Self {
        Self {
            config,
            node,
            channel_state,
            cycling_channel,
        }
    }
}

impl StatefulWidget for VolumeWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        let max_volume = self.config.max_volume_percent / 100.0;

        // Unified view's label width never changes, cycling or not -
        // every row's bar starts at the same column regardless of
        // balance state. A cycling row's channel label (below) gets its
        // own fixed sub-area at volume_label's own left edge, sized so
        // it can never collide with the percent's own worst-case width
        // - see STOCK_VOLUME_LABEL_WIDTH's doc comment. Linked/Channels
        // always use the wider, alignment-driven VOLUME_LABEL_WIDTH
        // regardless, whether or not cycling applies to them (it never
        // does - see `cycling_channel`) - see `VOLUME_LABEL_WIDTH`'s
        // doc comment for why.
        let view = self.channel_state.view();
        let label_width = if view == ChannelView::Unified {
            STOCK_VOLUME_LABEL_WIDTH
        } else {
            VOLUME_LABEL_WIDTH
        };

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(label_width), // volume_label
                Constraint::Min(0),              // volume_bar
            ])
            .spacing(1)
            .split(area);
        let volume_label = layout[0];
        let volume_bar = layout[1];

        let volumes = &self.node.volumes;
        if !volumes.is_empty() {
            // unified_imbalance = "cycle": show the currently-cycled
            // channel's own value (both label and bar) instead of the
            // mean.
            let (volume, channel_label, percent_text) = if let Some(index) =
                self.cycling_channel
            {
                let raw = volumes.get(index).copied().unwrap_or(0.0);
                let volume = raw.cbrt();
                let percent = (volume * 100.0).round() as u32;
                // Same "simple stereo pair -> L/R, otherwise the
                // channel's own real name, otherwise the raw index"
                // fallback ChannelRowWidget's own per-channel label
                // uses - a short, meaningful label reads better here
                // than a bare index digit whenever one's available.
                let label = match single_pair(self.node) {
                    Some((left, _)) if index == left => "L".to_string(),
                    Some(_) => "R".to_string(),
                    None => self
                        .node
                        .positions
                        .as_ref()
                        .and_then(|positions| positions.get(index))
                        .map(|&position| {
                            channel_pairing::channel_name(position)
                        })
                        .unwrap_or_else(|| index.to_string()),
                };
                (volume, Some(label), format!("{percent}%"))
            } else {
                let mean = volumes.iter().sum::<f32>() / volumes.len() as f32;
                let volume = mean.cbrt();
                let percent = (volume * 100.0).round() as u32;
                (volume, None, format!("{percent}%"))
            };

            // Mute is a single node-level property (SPA_PROP_mute), not
            // per-channel, so it overrides the percent text alone - the
            // channel label (if cycling) and the bar both keep
            // rendering normally either way, matching the convention
            // ChannelRowWidget's percent_col/StereoVolumeWidget's
            // label_l/label_r use.
            let percent_text = if self.node.mute {
                "muted".to_string()
            } else {
                percent_text
            };

            // The channel label (if cycling) gets its own fixed sub-area
            // - UNIFIED_CHANNEL_LABEL_WIDTH columns, anchored to
            // volume_label's own left edge (the same position it's
            // always occupied, right after the selector) - right-aligned
            // within it rather than left-aligned, so a short label ("L",
            // "R") sits closer to the percent instead of hugging the far
            // left. Since STOCK_VOLUME_LABEL_WIDTH reserves enough total
            // width for the label sub-area *and* the percent's own worst
            // case ("muted", 5 columns) side by side with no overlap
            // (see its doc comment), the two never collide - a
            // 4-character label and 5-character percent/mute meet
            // exactly at the boundary with zero columns to spare in the
            // worst case, but never overwrite each other's cells.
            if let Some(channel_label) = channel_label {
                let label_area = Rect::new(
                    volume_label.x,
                    volume_label.y,
                    UNIFIED_CHANNEL_LABEL_WIDTH,
                    volume_label.height,
                );
                Line::from(Span::styled(
                    channel_label,
                    self.config.theme.volume,
                ))
                .alignment(Alignment::Right)
                .render(label_area, buf);
            }
            Line::from(Span::styled(percent_text, self.config.theme.volume))
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
        } else if self.node.mute {
            // No channels to show a percentage for at all (an unusual,
            // likely-transient state) - still surface "muted" somewhere
            // rather than showing nothing.
            Line::from(Span::styled("muted", self.config.theme.volume))
                .alignment(Alignment::Right)
                .render(volume_label, buf);
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

        let max_volume = self.config.max_volume_percent / 100.0;

        let (label_l_width, label_r_width) =
            if self.config.expand_unused_label_space {
                (STEREO_LABEL_WIDTH_EXPANDED, STEREO_LABEL_WIDTH_EXPANDED)
            } else {
                (STEREO_LABEL_L_WIDTH, STEREO_LABEL_R_WIDTH)
            };

        // Two separate Fill(1) constraints can end up with unequal widths
        // when the remaining space is odd (Ratatui's Fill distribution
        // isn't guaranteed symmetric) - identical numerically-equal L/R
        // volumes would then render as visibly different bar lengths just
        // from that width difference. Computing one shared bar_width and
        // giving both bars the same explicit Length(bar_width) makes that
        // impossible by construction; any odd leftover column is simply
        // unused rather than handed to one side.
        let fixed_width = label_l_width + label_r_width + 1; // label_l + center + label_r
        let spacing_width = 4; // 4 gaps between the 5 segments, at 1 each
        let bar_width =
            area.width.saturating_sub(fixed_width + spacing_width) / 2;

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(label_l_width), // label_l
                Constraint::Length(bar_width),     // bar_l
                Constraint::Length(1),             // center
                Constraint::Length(bar_width),     // bar_r
                Constraint::Length(label_r_width), // label_r
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

        // Mute is a single node-level property (SPA_PROP_mute), not
        // per-channel, so it replaces both channels' own percent text
        // here rather than picking one - the bars/divider still render
        // normally either way, only the percent text itself is
        // overridden (same convention ChannelRowWidget's percent_col
        // uses).
        let left_percent = if self.node.mute {
            "muted".to_string()
        } else {
            format!("{}%", (left_volume * 100.0).round() as u32)
        };
        Line::from(Span::styled(left_percent, self.config.theme.volume))
            .alignment(Alignment::Right)
            .render(label_l, buf);

        let right_percent = if self.node.mute {
            "muted".to_string()
        } else {
            format!("{}%", (right_volume * 100.0).round() as u32)
        };
        Line::from(Span::styled(right_percent, self.config.theme.volume))
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

/// A single channel's volume bar within Channel mode's (or split_style =
/// "stacked"'s) stacked-row display - one line, independently
/// addressable, left-aligned, stretching its bar across the whole
/// remaining row width. Labeled with its real channel name (`FL`,
/// `AUX0`, ...) via `channel_pairing::channel_name` when `positions` data
/// is available, falling back to the raw channel index otherwise - unless
/// the *entire* node is a single simple pair (`single_pair` returns
/// `Some`), in which case there's no ambiguity to name away and the
/// label simplifies to just `L`/`R`.
struct ChannelRowWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
    channel_index: usize,
    view: ChannelView,
}

impl<'a> ChannelRowWidget<'a> {
    fn new(
        config: &'a Config,
        node: &'a view::Node,
        channel_index: usize,
        view: ChannelView,
    ) -> Self {
        Self {
            config,
            node,
            channel_index,
            view,
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
        let meter_layout = self.config.meter_layout(self.view);
        let (row_area, meter_area) = if self.config.peaks == Peaks::Off {
            (peaks_off_volume_area(area, meter_layout), None)
        } else {
            let (volume_area, meter_area) = split_meter_row(area, meter_layout);
            (volume_area, Some(meter_area))
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
        let label = match single_pair(self.node) {
            Some((left, _)) if channel_index == left => "L".to_string(),
            Some(_) => "R".to_string(),
            None => self
                .node
                .positions
                .as_ref()
                .and_then(|positions| positions.get(channel_index))
                .map(|&position| channel_pairing::channel_name(position))
                .unwrap_or_else(|| channel_index.to_string()),
        };

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
            meter::render_channel_mono(meter_area, buf, peak, self.config);
            self.node.peaks_dirty.store(false, Ordering::Relaxed);
        }
    }
}

/// The column width for `RadiatingRowWidget`'s leading label - wide
/// enough for the longest possible content regardless of which shows up:
/// a `PairLabelStyle::Verbose` group label (`channel_pairing::
/// MAX_GROUP_NAME_WIDTH` plus a 4-character `" L|R"` suffix), or an
/// unpaired channel's own `channel_name` (up to `"AUX63"`, 5 chars,
/// comfortably under this).
const RADIATING_LABEL_WIDTH: u16 =
    (channel_pairing::MAX_GROUP_NAME_WIDTH + 4) as u16;

/// `RadiatingRowWidget`'s label_l/label_r width - fits `"100%"` or, once
/// muted, `"muted"` (5 characters), matching `ChannelRowWidget`'s own
/// percent_col precedent. 4 would truncate "muted" to "uted".
const RADIATING_PERCENT_WIDTH: u16 = 5;

/// `VolumeWidget`'s label width in `Linked`/`Channels` view - wide
/// enough for `"{index} {percent}%"` (`unified_imbalance = "cycle"`'s
/// label; up to 6 characters, e.g. `"1 100%"`) as well as the plain
/// mean-only `"{percent}%"` label, *plus* 6 extra blank columns folded
/// in on the left. Those 6 columns used to be a separate
/// `Constraint::Length` sitting in front of `volume_area` in the outer
/// bar/meter `Layout` (a `NODE_VOLUME_PADDING` constant) - pure
/// padding, invisible either way, but as a *sibling* constraint it
/// shrank the `Fill` budget the outer layout had to split between
/// `volume_area` and `meter_area` relative to a `Stacked` row's own
/// row/meter split (which has nothing analogous), so the two disagreed
/// on where `meter_area` should start by a column or two even after
/// their *bars* lined up. Folding the same 6 columns into this label
/// instead keeps the bar at the same column as before while making the
/// outer layout's constraint list byte-for-byte identical between the
/// classic path and a `Stacked`/`Radiating` row, which is what actually
/// guarantees their meter columns match too, by construction rather
/// than by coincidence.
///
/// `Unified` view doesn't use this at all - see
/// `STOCK_VOLUME_LABEL_WIDTH`. 13, not 12, to stay aligned with
/// `StereoVolumeWidget`/`RadiatingRowWidget`'s own bar_l start now that
/// `RADIATING_PERCENT_WIDTH` (5, wide enough to fit "muted") grew from
/// the previous 4.
const VOLUME_LABEL_WIDTH: u16 = 13;

/// `VolumeWidget`'s label width in `Unified` view (channel_mode off,
/// channel_display = "unified") - `UNIFIED_CHANNEL_LABEL_WIDTH` (4) for
/// a cycling row's own channel label, plus the percent's own worst case
/// ("muted", 5 columns), side by side with no overlap, `4 + 5 = 9`.
/// This never changes for `unified_imbalance = "cycle"` - every Unified
/// row's bar starts at the same column regardless of balance state; a
/// cycling row's channel label gets its own fixed sub-area instead of
/// widening the row (see `VolumeWidget::render`). Balanced rows (and
/// `Linked`/`Channels`, which use `VOLUME_LABEL_WIDTH` instead) simply
/// carry a bit more blank leading space than they strictly need - the
/// trade-off for keeping every row's bar at a single, predictable
/// column even though only some rows ever show a label.
const STOCK_VOLUME_LABEL_WIDTH: u16 = 9;

/// `VolumeWidget`'s channel-label sub-area width in `Unified` view -
/// anchored to `volume_label`'s own left edge (right after the
/// selector), independent of the percent text's own position. Right-
/// aligned within it, so a short label ("L", "R") sits closer to the
/// percent instead of hugging the far left. Fits a simple pair's
/// `"L"`/`"R"`, most named positions (`"FL"`, `"RR"`, `"LFE"`), and
/// numbered ones up to `"AUX9"` without truncating; wider numbered
/// channels (`"AUX10"`+) truncate, an accepted edge case for a label
/// that's meant to stay short.
const UNIFIED_CHANNEL_LABEL_WIDTH: u16 = 4;

/// `StereoVolumeWidget`'s label_l width - same "closes the gap to a
/// `Stacked` row's bar-start column" role `VOLUME_LABEL_WIDTH` plays,
/// see its doc comment for why this folds in what used to be a separate
/// `NODE_VOLUME_PADDING` constraint. Only `label_l` needs it: it's the
/// first segment in `StereoVolumeWidget`'s own layout, so its width is
/// what determines the bar's absolute start column - `label_r` (below)
/// comes after both bars and never needs to line up with anything
/// external. 13, not 12, to stay aligned with `RadiatingRowWidget`'s own
/// bar_l start now that its `label_l`/`label_r` are
/// `RADIATING_PERCENT_WIDTH` (5, wide enough to fit "muted") rather
/// than the previous 4.
const STEREO_LABEL_L_WIDTH: u16 = 13;

/// `StereoVolumeWidget`'s label_r width. Its content (a plain
/// `"{percent}%"`) never needs all of this width - the leading column
/// or two is just blank padding, kept equal to what `label_l` used to
/// be (before folding padding into it) purely so the two labels still
/// look visually balanced around the bars.
const STEREO_LABEL_R_WIDTH: u16 = 6;

/// `STEREO_LABEL_L_WIDTH`/`STEREO_LABEL_R_WIDTH`, under
/// `expand_unused_label_space` - both labels drop straight down to just
/// what a plain `"{percent}%"` needs, discarding `STEREO_LABEL_L_WIDTH`'s
/// own folded alignment padding entirely rather than trimming it by a
/// token column or two. This deliberately gives up cross-widget column
/// alignment with `RadiatingRowWidget` (a lone stereo pair's row no
/// longer starts its bar at the same column a `Stacked` block's own
/// radiating row would) in exchange for a much more noticeable amount
/// of reclaimed width - the whole point of turning this setting on is
/// "an arrangement that uses most of the space" over "an arrangement of
/// greater symmetry", so a barely-perceptible one-column nudge doesn't
/// actually deliver what was asked for.
const STEREO_LABEL_WIDTH_EXPANDED: u16 = 5;

/// One row within a `Stacked` block, in the shared column grid a
/// `split_style = "radiating"` context uses for every row alike: label |
/// left/unpaired % | left/unpaired bar | divider-or-blank | right
/// bar-or-blank | right %-or-blank. A detected left/right pair
/// (`right_index: Some`) fills every column, the same two-bars-from-
/// center idea as `StereoVolumeWidget`'s classic single-row display, but
/// sized to share space with sibling rows rather than owning the whole
/// bar area, and prefixed with a short group label since - unlike the
/// classic single-pair fast path - there's always at least one sibling
/// row here to tell apart from. An unpaired channel (`right_index:
/// None`) fills only the left half and leaves the divider/right bar/
/// right label columns blank, so its bar starts and ends at exactly the
/// same columns a paired row's left bar would.
struct RadiatingRowWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
    left_index: usize,
    /// `Some(right_index)` for a detected pair, `None` for an unpaired
    /// channel occupying just the left half of the row.
    right_index: Option<usize>,
    pair_label_style: PairLabelStyle,
    /// Shared across every row in the block - see
    /// `NodeWidget::render_channel_rows`.
    bar_width: u16,
    view: ChannelView,
}

impl<'a> RadiatingRowWidget<'a> {
    fn new(
        config: &'a Config,
        node: &'a view::Node,
        left_index: usize,
        right_index: Option<usize>,
        pair_label_style: PairLabelStyle,
        bar_width: u16,
        view: ChannelView,
    ) -> Self {
        Self {
            config,
            node,
            left_index,
            right_index,
            pair_label_style,
            bar_width,
            view,
        }
    }

    /// `bar_width` for a row of this shape given the width available for
    /// the row's own content - see `NodeWidget::split_row_content_width`.
    /// Kept in sync with the exact column layout `render` below actually
    /// uses. The same formula applies whether or not this particular row
    /// ends up paired - an unpaired row still reserves (but leaves
    /// blank) the divider/right-bar/right-label columns, so every row in
    /// the block agrees on where the bar starts and how long it can get.
    fn bar_width(content_width: u16) -> u16 {
        let fixed_width = RADIATING_LABEL_WIDTH
            + RADIATING_PERCENT_WIDTH
            + 1
            + RADIATING_PERCENT_WIDTH; // label + label_l + center + label_r
        let spacing_width = 5; // 5 gaps between the 6 segments, at 1 each
        content_width.saturating_sub(fixed_width + spacing_width) / 2
    }
}

impl StatefulWidget for RadiatingRowWidget<'_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        // Split style rows split their own monitor too, same rule as
        // `ChannelRowWidget` - a paired row shows a real stereo gauge
        // (rather than a mono one) since it's already displaying two
        // channels' worth of volume; an unpaired row's gauge occupies
        // only the left half, mirroring its volume bar.
        let meter_layout = self.config.meter_layout(self.view);
        let (row_area, meter_area) = if self.config.peaks == Peaks::Off {
            (peaks_off_volume_area(area, meter_layout), None)
        } else {
            let (volume_area, meter_area) = split_meter_row(area, meter_layout);
            (volume_area, Some(meter_area))
        };

        let max_volume = self.config.max_volume_percent / 100.0;

        // An unpaired channel's row normally reserves (but leaves
        // blank) the divider/right-bar/right-label columns, so every
        // row in the block agrees on where the bar starts and ends -
        // see the module doc comment on `RadiatingRowWidget`.
        // expand_unpaired_channel_bars trades that cross-row agreement
        // away for a denser look: bar_l stretches across all of the
        // reclaimed space instead, since there's no second channel here
        // that would ever need it.
        let expand_unpaired = self.config.expand_unpaired_channel_bars
            && self.right_index.is_none();

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(if expand_unpaired {
                vec![
                    Constraint::Length(RADIATING_LABEL_WIDTH), // label
                    Constraint::Length(RADIATING_PERCENT_WIDTH), // label_l
                    Constraint::Min(0),                        // bar_l
                ]
            } else {
                vec![
                    Constraint::Length(RADIATING_LABEL_WIDTH), // label
                    Constraint::Length(RADIATING_PERCENT_WIDTH), // label_l
                    Constraint::Length(self.bar_width),        // bar_l
                    Constraint::Length(1),                     // center
                    Constraint::Length(self.bar_width),        // bar_r
                    Constraint::Length(RADIATING_PERCENT_WIDTH), // label_r
                ]
            })
            .spacing(1)
            .split(row_area);
        let label_area = layout[0];
        let label_l = layout[1];
        let bar_l = layout[2];

        Line::from(Span::styled(
            radiating_row_label(
                self.node,
                self.left_index,
                self.right_index,
                self.pair_label_style,
            ),
            self.config.theme.volume,
        ))
        .alignment(Alignment::Right)
        .render(label_area, buf);

        let volumes = &self.node.volumes;
        let left_volume =
            volumes.get(self.left_index).copied().unwrap_or(0.0).cbrt();

        // Mute is a single node-level property (SPA_PROP_mute), not
        // per-channel, so it replaces the percent text alone - the bar
        // still renders normally either way (same convention
        // ChannelRowWidget's percent_col uses).
        let left_percent = if self.node.mute {
            "muted".to_string()
        } else {
            format!("{}%", (left_volume * 100.0).round() as u32)
        };
        Line::from(Span::styled(left_percent, self.config.theme.volume))
            .alignment(Alignment::Right)
            .render(label_l, buf);

        let left_count = ((left_volume.clamp(0.0, max_volume) / max_volume)
            * bar_l.width as f32)
            .round() as usize;
        let blank = self
            .config
            .char_set
            .volume_empty
            .repeat((bar_l.width as usize).saturating_sub(left_count));
        let filled = self.config.char_set.volume_filled.repeat(left_count);
        let spans = if self.right_index.is_none() {
            // An unpaired channel always fills classic left-to-right,
            // like every other non-radiating bar in the app - only a
            // genuine radiating pair grows outward from a shared center
            // marker (there's no pair here to radiate from).
            vec![
                Span::styled(filled, self.config.theme.volume_filled),
                Span::styled(blank, self.config.theme.volume_empty),
            ]
        } else {
            vec![
                Span::styled(blank, self.config.theme.volume_empty),
                Span::styled(filled, self.config.theme.volume_filled),
            ]
        };
        Line::from(spans).render(bar_l, buf);

        mouse_areas.push((
            label_l,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![
                Action::SelectObject(self.node.object_id),
                Action::SelectChannel(self.left_index),
                Action::ToggleMute
            ],
        ));

        // The divider/right-bar/right-label columns simply stay blank
        // for an unpaired channel - reserved above (unless
        // expand_unpaired_channel_bars handed them to bar_l instead) so
        // every row in the block agrees on where the bar starts, but
        // there's no second channel here to show anything in them.
        let Some(right_index) = self.right_index else {
            // Classic left-to-right mapping, matching the fill direction
            // above - the two must agree or clicking a filled cell
            // wouldn't reproduce the value it displays.
            for i in 0..=bar_l.width {
                let x = bar_l.x.saturating_add(i);
                let volume_area = Rect::new(x, bar_l.y, 1, bar_l.height);
                let volume_step = max_volume / bar_l.width as f32;
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
                        Action::SelectChannel(self.left_index),
                        Action::SetChannelAbsoluteVolume(
                            self.left_index,
                            sticky_volume
                        ),
                    ],
                ));
            }
            if let Some(meter_area) = meter_area {
                self.render_meter(meter_area, buf);
            }
            return;
        };
        // right_index is Some, so expand_unpaired was false above and
        // the layout used all 6 constraints.
        let center = layout[3];
        let bar_r = layout[4];
        let label_r = layout[5];

        let right_volume =
            volumes.get(right_index).copied().unwrap_or(0.0).cbrt();

        let right_percent = if self.node.mute {
            "muted".to_string()
        } else {
            format!("{}%", (right_volume * 100.0).round() as u32)
        };
        Line::from(Span::styled(right_percent, self.config.theme.volume))
            .render(label_r, buf);

        Line::from(Span::styled("|", self.config.theme.volume))
            .alignment(Alignment::Center)
            .render(center, buf);

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

        mouse_areas.push((
            label_r,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![
                Action::SelectObject(self.node.object_id),
                Action::SelectChannel(right_index),
                Action::ToggleMute
            ],
        ));

        for (bar, channel_index, from_center) in
            [(bar_l, self.left_index, true), (bar_r, right_index, false)]
        {
            for i in 0..=bar.width {
                let x = if from_center {
                    bar.x.saturating_add(bar.width.saturating_sub(i))
                } else {
                    bar.x.saturating_add(i)
                };
                let volume_area = Rect::new(x, bar.y, 1, bar.height);

                let volume_step = max_volume / bar.width as f32;
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
        }

        if let Some(meter_area) = meter_area {
            self.render_meter(meter_area, buf);
        }
    }
}

impl RadiatingRowWidget<'_> {
    fn render_meter(&self, meter_area: Rect, buf: &mut Buffer) {
        let peaks = self.node.peaks.as_deref();
        let left = peaks
            .and_then(|peaks| peaks.get(self.left_index))
            .map(|p| p.load());

        match self.right_index {
            Some(right_index) => {
                let right = peaks
                    .and_then(|peaks| peaks.get(right_index))
                    .map(|p| p.load());
                // Same rule MeterWidget applies for a whole node: honor
                // peaks = "mono" even for a detected pair, collapsing to
                // one combined gauge across the full row instead of a
                // left/right split.
                if self.config.peaks == Peaks::Mono {
                    let mono = match (left, right) {
                        (Some(l), Some(r)) => Some((l + r) / 2.0),
                        (Some(peak), None) | (None, Some(peak)) => Some(peak),
                        (None, None) => None,
                    };
                    meter::render_channel_mono(
                        meter_area,
                        buf,
                        mono,
                        self.config,
                    );
                } else {
                    meter::render_channel_stereo(
                        meter_area,
                        buf,
                        left.zip(right),
                        self.config,
                    );
                }
            }
            None => {
                // Only the left half gets a gauge - the right half stays
                // blank, mirroring the volume bar's own left/right split
                // above.
                let half = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Fill(1), Constraint::Fill(1)])
                    .split(meter_area);
                meter::render_channel_mono(half[0], buf, left, self.config);
            }
        }

        self.node.peaks_dirty.store(false, Ordering::Relaxed);
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

    #[test]
    fn split_meter_row_unset_ratio_splits_evenly() {
        let area = Rect::new(0, 0, 100, 1);
        let layout = MeterLayout {
            meter_width_percent: None,
            gap: 0,
            right_margin: 0,
        };
        let (volume_area, meter_area) = split_meter_row(area, layout);
        assert_eq!(volume_area.width, 50);
        // gap = 0 is a floor, not an exact override - it still scales
        // up with the 50-wide meter side (see effective_gap), landing
        // at 50.div_ceil(8) = 7 here even though 0 was configured.
        assert_eq!(meter_area.width, 43);
        assert_eq!(meter_area.x, 57);
    }

    #[test]
    fn split_meter_row_custom_ratio_never_shrinks_the_volume_side() {
        // Regression guard: gap/right_margin must come out of the meter
        // side alone - the volume side's own width must depend only on
        // the ratio, never on how wide the gap/margin happen to be.
        let area = Rect::new(0, 0, 100, 1);
        let narrow_gap = MeterLayout {
            meter_width_percent: Some(20.0),
            gap: 1,
            right_margin: 1,
        };
        let wide_gap = MeterLayout {
            meter_width_percent: Some(20.0),
            gap: 20,
            right_margin: 20,
        };
        assert_eq!(
            split_meter_row(area, narrow_gap).0.width,
            split_meter_row(area, wide_gap).0.width,
            "volume_area's width must be identical regardless of gap/right_margin"
        );
        assert_eq!(split_meter_row(area, narrow_gap).0.width, 80);
    }

    #[test]
    fn split_meter_row_gap_and_margin_shrink_only_the_meter_area() {
        let area = Rect::new(0, 0, 100, 1);
        let layout = MeterLayout {
            meter_width_percent: None,
            gap: 3,
            right_margin: 7,
        };
        let (volume_area, meter_area) = split_meter_row(area, layout);
        assert_eq!(volume_area.width, 50);
        // meter_side is 50 wide; the configured gap floor (3) is below
        // what 50.div_ceil(8) = 7 scales up to, so the effective gap is
        // 7, not 3 - leaving 50 - 7 - 7 = 36 for the meter itself.
        assert_eq!(meter_area.width, 36);
        assert_eq!(meter_area.x, 50 + 7);
    }

    #[test]
    fn effective_gap_scales_up_with_available_width_above_the_floor() {
        // Narrow meter side: the configured floor wins.
        assert_eq!(effective_gap(2, 8), 2);
        // Wide meter side: scaling (1-in-8 of the meter side's own
        // width) wins over a small floor.
        assert_eq!(effective_gap(2, 80), 10);
        // A larger configured floor still wins over scaling when it's
        // the bigger of the two.
        assert_eq!(effective_gap(5, 8), 5);
    }

    #[test]
    fn peaks_off_volume_area_reserves_only_right_margin() {
        let area = Rect::new(0, 0, 100, 1);
        let layout = MeterLayout {
            meter_width_percent: None,
            gap: 0,
            right_margin: 0,
        };
        assert_eq!(peaks_off_volume_area(area, layout).width, 100);

        let layout = MeterLayout {
            meter_width_percent: None,
            gap: 0,
            right_margin: 5,
        };
        assert_eq!(peaks_off_volume_area(area, layout).width, 95);
    }

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
            pair_label_style: config.pair_label_style,
        };
        let display = volume_display(channel_state, node);
        let cycling_channel = cycling_channel(channel_state, node, 0.0);
        render_volume(
            config,
            node,
            VolumeContext {
                channel_state,
                display,
                cycling_channel,
            },
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
            pair_label_style: config.pair_label_style,
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
        // after fixed segments (40 - 17 = 23) is odd, exactly the case
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
    fn stereo_volume_mute_overrides_only_the_percent_labels() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let mut node = test_node(Some(vec![fl, fr]), vec![1.0, 1.0]);
        node.mute = true;
        let config =
            config::Config::from_toml_str("channel_display = \"always\"");

        let rendered = render_to_string(&config, &node);

        // Both channels' percent text becomes "muted", but the row
        // structure (divider) still renders - a lone stereo pair's
        // muted row must not collapse into one big centered "muted"
        // spanning the whole row, which used to blank out the divider
        // and both bars entirely.
        assert_eq!(rendered.matches("muted").count(), 2);
        assert!(
            rendered.contains('|'),
            "the center divider must still render while muted"
        );
    }

    #[test]
    fn expand_unused_label_space_widens_a_lone_stereo_pairs_bars() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 1.0]);

        let narrow_config = config::Config::from_toml_str(
            "channel_display = \"always\"\nmax_volume_percent = 100.0\n\
             expand_unused_label_space = false",
        );
        let expanded_config = config::Config::from_toml_str(
            "channel_display = \"always\"\nmax_volume_percent = 100.0\n\
             expand_unused_label_space = true",
        );

        let count_filled = |config: &Config| -> (usize, usize) {
            let rendered = render_to_string(config, &node);
            let filled = config.char_set.volume_filled.as_str();
            let center_index =
                rendered.find('|').expect("center marker present");
            (
                rendered[..center_index].matches(filled).count(),
                rendered[center_index + 1..].matches(filled).count(),
            )
        };

        let (narrow_left, narrow_right) = count_filled(&narrow_config);
        let (wide_left, wide_right) = count_filled(&expanded_config);

        // label_l+label_r drop from 12+6=18 down to 5+5=10 - an 8-column
        // reclaim, split evenly (4 each) between bar_l/bar_r, not just a
        // token column or two.
        assert_eq!(
            wide_left,
            narrow_left + 4,
            "expand_unused_label_space should give bar_l a real width \
             increase, not just a token column"
        );
        assert_eq!(
            wide_right,
            narrow_right + 4,
            "bar_r gains the same amount as bar_l so they stay equal-width"
        );
    }

    #[test]
    fn single_pair_finds_a_lone_named_lr_pair() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        assert_eq!(single_pair(&node), Some((0, 1)));
    }

    #[test]
    fn single_pair_none_without_positions() {
        let node = test_node(None, vec![1.0, 0.0]);
        assert_eq!(single_pair(&node), None);
    }

    #[test]
    fn single_pair_none_for_generic_aux_channels() {
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;
        let node = test_node(Some(vec![aux0, aux1]), vec![1.0, 0.0]);
        assert_eq!(single_pair(&node), None);
    }

    #[test]
    fn single_pair_none_when_more_than_one_pair() {
        // The user's real 5.1 hardware: "M-Audio Sonica Theater Analog
        // Surround 5.1" -> FL,FR,RL,RR,FC,LFE. Radiating's classic
        // single-row fast path must not claim this - two pairs plus two
        // singles need `Stacked`'s multi-row layout, or four of the six
        // channels would never be rendered or clickable at all.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let rl = libspa_sys::SPA_AUDIO_CHANNEL_RL;
        let rr = libspa_sys::SPA_AUDIO_CHANNEL_RR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let lfe = libspa_sys::SPA_AUDIO_CHANNEL_LFE;
        let node = test_node(Some(vec![fl, fr, rl, rr, fc, lfe]), vec![0.5; 6]);
        assert_eq!(single_pair(&node), None);
    }

    #[test]
    fn single_pair_none_with_extra_channel_alongside_a_pair() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let node = test_node(Some(vec![fl, fr, fc]), vec![0.5; 3]);
        assert_eq!(single_pair(&node), None);
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
    fn unified_view_bar_start_never_moves_for_cycling() {
        // Default config (channel_display = "unified", unified_imbalance
        // = "none") now carries small non-`None` gap/right_margin
        // defaults (see default_gap/default_right_margin in config.rs),
        // so it no longer takes the literal-stock fast path - the bar
        // starts right after the customized label (9) + spacing(1) =
        // column 10.
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let balanced_node = test_node(Some(vec![mono]), vec![1.0]);

        let default_config = config::Config::from_toml_str("");
        let default_rendered =
            render_to_string(&default_config, &balanced_node);
        let bar_start = default_rendered
            .find(default_config.char_set.volume_filled.as_str())
            .expect("a filled bar somewhere");
        assert_eq!(
            bar_start, 10,
            "default Unified view's bar should start at column 10 \
             (9-wide customized label + 1 spacing)"
        );

        let cycle_config =
            config::Config::from_toml_str("unified_imbalance = \"cycle\"");

        // Every Unified row's bar starts at the same column regardless
        // of balance state - a balanced node...
        let balanced_cycle_rendered =
            render_to_string(&cycle_config, &balanced_node);
        let balanced_cycle_bar_start = balanced_cycle_rendered
            .find(cycle_config.char_set.volume_filled.as_str())
            .expect("a filled bar somewhere");
        assert_eq!(balanced_cycle_bar_start, 10);

        // ...and a genuinely imbalanced FL/FR pair, showing a channel
        // label alongside its own value, both land on column 10 - the
        // label has its own fixed sub-area within the row's total
        // width instead of widening the row to fit it.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let imbalanced_node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let imbalanced_rendered =
            render_to_string(&cycle_config, &imbalanced_node);
        let imbalanced_bar_start = imbalanced_rendered
            .find(cycle_config.char_set.volume_filled.as_str())
            .expect("a filled bar somewhere");
        assert_eq!(
            imbalanced_bar_start, 10,
            "a cycling row's bar must start at the same column a \
             balanced row's does, not a wider one"
        );
    }

    #[test]
    fn unified_view_and_linked_view_use_different_bar_start_columns() {
        // A single-channel node renders via the same VolumeWidget in
        // both views (nothing to split), but Unified view's column must
        // stay independent of Linked/Channels' wider, alignment-driven
        // one - they're deliberately different layouts now, not the
        // same one with different content.
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let node = test_node(Some(vec![mono]), vec![1.0]);

        let unified_config = config::Config::from_toml_str("");
        let linked_config =
            config::Config::from_toml_str("channel_display = \"always\"");

        let unified_lines =
            render_node_lines(&unified_config, &node, false, false, None);
        let linked_lines =
            render_node_lines(&linked_config, &node, false, false, None);

        let bar_start = |lines: &[String], filled: &str| -> usize {
            lines
                .iter()
                .find_map(|line| line.find(filled))
                .expect("a filled bar somewhere in the rendered node")
        };

        let unified_start = bar_start(
            &unified_lines,
            unified_config.char_set.volume_filled.as_str(),
        );
        let linked_start = bar_start(
            &linked_lines,
            linked_config.char_set.volume_filled.as_str(),
        );

        assert_ne!(
            unified_start, linked_start,
            "Unified view's stock-matching column and Linked view's \
             wider, alignment-driven column must not coincide"
        );
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
        // the unified_imbalance = "none" test above) - labeled "L" since
        // this node is a simple FL/FR pair (see single_pair), not the
        // raw index. One space: "L" lands in the last column of its own
        // 4-wide, right-aligned sub-area, immediately followed by the
        // one blank column left over before "100%" (4 wide) fills the
        // rest of the 9-wide volume_label.
        assert!(rendered.contains("L 100%"));
        assert!(!rendered.contains("79%"));
    }

    #[test]
    fn unified_imbalance_cycle_bar_immediately_follows_the_percent() {
        // Regression test: a bug in how channel_col/percent_col were
        // split out of volume_label left 2 blank columns stranded
        // between "%" and the bar (ratatui's default flex packs Length
        // constraints at the *start* of the area they're given and
        // leaves any shortfall trailing at the end, rather than at the
        // front where the alignment fold is meant to sit). The bar must
        // start exactly 1 column (the normal spacing) after "%", not 3.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str(
            "channel_display = \"unified\"\nunified_imbalance = \"cycle\"",
        );

        let rendered = render_to_string(&config, &node);
        let percent_end = rendered.find('%').expect("a percent sign") + 1;
        let bar_start = rendered
            .find(config.char_set.volume_filled.as_str())
            .expect("a filled bar somewhere");

        assert_eq!(
            bar_start - percent_end,
            1,
            "exactly 1 spacing column must separate \"%\" from the bar, \
             not 3"
        );
    }

    #[test]
    fn unified_imbalance_cycle_mute_overrides_only_the_percent() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let mut node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        node.mute = true;
        let config = config::Config::from_toml_str(
            "channel_display = \"unified\"\nunified_imbalance = \"cycle\"",
        );

        let rendered = render_to_string(&config, &node);

        // The channel label ("L") must still show even while muted -
        // only the percent text becomes "muted", not the whole label
        // area.
        assert!(
            rendered.contains("L muted") || rendered.contains("Lmuted"),
            "channel label must survive muting, only the percent \
             becomes \"muted\": {rendered:?}"
        );
    }

    #[test]
    fn unified_view_mute_overrides_only_the_percent_not_the_bar() {
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let mut node = test_node(Some(vec![mono]), vec![1.0]);
        node.mute = true;
        let config = config::Config::from_toml_str("");

        let rendered = render_to_string(&config, &node);
        assert!(rendered.contains("muted"));
    }

    #[test]
    fn unified_imbalance_cycle_channel_label_holds_position_regardless_of_percent_width(
    ) {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let config = config::Config::from_toml_str(
            "channel_display = \"unified\"\nunified_imbalance = \"cycle\"",
        );

        // Channel 0's own value at differing percent widths - 64% (two
        // digits) vs 100% (three) - previously shifted the index digit's
        // column when it was rendered as one right-aligned string
        // together with the percentage; label_col/percent_col are
        // independently right-aligned specifically so this can't happen.
        let two_digit = test_node(Some(vec![fl, fr]), vec![0.262144, 0.0]);
        let three_digit = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);

        let two_digit_rendered = render_to_string(&config, &two_digit);
        let three_digit_rendered = render_to_string(&config, &three_digit);

        // Channel 0 of a simple FL/FR pair labels as "L" (see
        // single_pair) - its column must hold steady regardless of the
        // percentage's own digit count.
        let label_col = |s: &str| s.find('L').expect("channel label present");
        assert_eq!(
            label_col(&two_digit_rendered),
            label_col(&three_digit_rendered),
        );
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
            pair_label_style: Default::default(),
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
    fn volume_display_stacked_for_multi_pair_device_even_with_radiating() {
        // The Sonica Theater case: split_style asks for radiating, but
        // there's more than a single simple pair here, so this must NOT
        // take the classic single-row Radiating fast path (which would
        // silently drop four of the six channels).
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let rl = libspa_sys::SPA_AUDIO_CHANNEL_RL;
        let rr = libspa_sys::SPA_AUDIO_CHANNEL_RR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let lfe = libspa_sys::SPA_AUDIO_CHANNEL_LFE;
        let node = test_node(Some(vec![fl, fr, rl, rr, fc, lfe]), vec![0.5; 6]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        state.split_style = SplitStyle::Radiating;
        assert_eq!(volume_display(state, &node), VolumeDisplay::Stacked);
    }

    #[test]
    fn display_rows_radiating_collapses_pairs_and_keeps_singles() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let rl = libspa_sys::SPA_AUDIO_CHANNEL_RL;
        let rr = libspa_sys::SPA_AUDIO_CHANNEL_RR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let lfe = libspa_sys::SPA_AUDIO_CHANNEL_LFE;
        let node = test_node(Some(vec![fl, fr, rl, rr, fc, lfe]), vec![0.5; 6]);
        let mut state = channel_state(false);
        state.split_style = SplitStyle::Radiating;

        assert_eq!(
            display_rows(state, &node),
            vec![
                ChannelGroup::Pair(0, 1),
                ChannelGroup::Pair(2, 3),
                ChannelGroup::Single(4),
                ChannelGroup::Single(5),
            ]
        );
    }

    #[test]
    fn display_rows_stacked_style_ignores_pairing() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);
        let mut state = channel_state(false);
        state.split_style = SplitStyle::Stacked;

        assert_eq!(
            display_rows(state, &node),
            vec![ChannelGroup::Single(0), ChannelGroup::Single(1)]
        );
    }

    #[test]
    fn display_rows_channel_mode_ignores_pairing_even_with_radiating() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![0.5, 0.5]);
        let mut state = channel_state(true);
        state.split_style = SplitStyle::Radiating;

        assert_eq!(
            display_rows(state, &node),
            vec![ChannelGroup::Single(0), ChannelGroup::Single(1)]
        );
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
    fn node_height_radiating_counts_groups_not_raw_channels() {
        // Sonica Theater: 6 raw channels, but only 4 display_rows groups
        // (2 pairs + 2 singles) - height must follow the groups, or the
        // rendered rows would run past the area NodeWidget was given.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let rl = libspa_sys::SPA_AUDIO_CHANNEL_RL;
        let rr = libspa_sys::SPA_AUDIO_CHANNEL_RR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let lfe = libspa_sys::SPA_AUDIO_CHANNEL_LFE;
        let node = test_node(Some(vec![fl, fr, rl, rr, fc, lfe]), vec![0.5; 6]);
        let mut state = channel_state(false);
        state.channel_display = ChannelDisplay::Always;
        state.split_style = SplitStyle::Radiating;

        // 1 header line + 4 group rows
        assert_eq!(NodeWidget::node_height(state, &node), 5);
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
                                    // "{label} {percent}%" string right-aligned as one unit. The
                                    // node is a simple stereo pair and nothing else, so the label
                                    // simplifies to "L"/"R" rather than "FL"/"FR".
        assert!(lines[1].contains("L  100%"));
        assert!(lines[2].contains("R    0%"));
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

        // A simple stereo pair and nothing else, so the label
        // simplifies to "L"/"R" rather than "FL"/"FR".
        let l_index = lines[1].find('L').expect("L label present");
        let r_index = lines[2].find('R').expect("R label present");
        assert_eq!(
            l_index, r_index,
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

    #[test]
    fn linked_mode_split_marks_every_row_since_the_whole_node_is_selected() {
        // AUX0/AUX1 never pair, so with split_style = "stacked" (or
        // radiating falling back, same thing) the node still renders
        // split even in linked mode (channel_mode = false). There's no
        // individually-targeted channel here - the whole node is the
        // selection unit - so the marker must span every row (a
        // top/middle/.../bottom bracket, same as the classic 2-line
        // node's own `SelectorWidget`), not just the header - otherwise
        // the rows below the header look like they belong to no
        // selection at all. Distinct top/middle/bottom glyphs (the
        // built-in char_sets all happen to use the same glyph for top
        // and bottom) so each row's marker can be checked precisely.
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;
        let node = test_node(Some(vec![aux0, aux1]), vec![0.5, 0.5]);
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"stacked\"\n\
             char_set = \"test\"\n\
             [char_sets.test]\n\
             inherit = \"default\"\n\
             selector_top = \"^\"\n\
             selector_middle = \"*\"\n\
             selector_bottom = \"_\"",
        );

        let selected = render_node_lines(&config, &node, false, true, None);
        assert!(
            selected[0].contains('^'),
            "the header row must carry the top cap"
        );
        assert!(
            selected[1].contains('*'),
            "an interior row must carry the middle glyph"
        );
        assert!(
            selected[2].contains('_'),
            "the last row must carry the bottom cap"
        );

        let not_selected =
            render_node_lines(&config, &node, false, false, None);
        assert!(!not_selected[0].contains('^'));
        assert!(!not_selected[1].contains('*'));
        assert!(!not_selected[2].contains('_'));
    }

    #[test]
    fn radiating_multi_pair_renders_every_channel() {
        // Sonica Theater: verifies the multi-pair fix actually renders
        // all six channels (not just the first pair), each pair gets a
        // short disambiguating label, and singles keep their own
        // channel_name label - nothing from the user's real hardware
        // layout goes missing or unclickable.
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let rl = libspa_sys::SPA_AUDIO_CHANNEL_RL;
        let rr = libspa_sys::SPA_AUDIO_CHANNEL_RR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let lfe = libspa_sys::SPA_AUDIO_CHANNEL_LFE;
        let node = test_node(
            Some(vec![fl, fr, rl, rr, fc, lfe]),
            vec![1.0, 0.0, 0.0, 1.0, 1.0, 0.0],
        );
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"",
        );

        let lines = render_node_lines(&config, &node, false, false, None);

        assert_eq!(lines.len(), 5); // header + 2 pair rows + 2 single rows
                                    // Pair rows are labeled with their group name, not either
                                    // individual channel's own name - "F"/"R" (default
                                    // PairLabelStyle::Verbose spells it "F L|R"/"R L|R"), never
                                    // "FL" or "RL" (which would be indistinguishable from a real
                                    // single-channel row showing that exact channel).
        assert!(lines[1].contains("F L|R"));
        assert!(lines[1].contains("100%"));
        assert!(lines[2].contains("R L|R"));
        assert!(lines[2].contains("100%"));
        assert!(lines[3].contains("FC"));
        assert!(lines[3].contains("100%"));
        assert!(lines[4].contains("LFE"));
    }

    #[test]
    fn radiating_single_row_bar_matches_pair_row_bar_length() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        // FL maxed (one side of a pair row), FC maxed (a lone single row
        // alongside it) - both must fill the *same* number of
        // characters, since render_channel_rows now shares one
        // bar_width across the whole block instead of letting the
        // single row stretch to fill all its own leftover width.
        let node = test_node(Some(vec![fl, fr, fc]), vec![1.0, 0.0, 1.0]);
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"",
        );

        let lines = render_node_lines(&config, &node, false, false, None);
        assert_eq!(lines.len(), 3); // header + 1 pair row + 1 single row

        let filled = config.char_set.volume_filled.as_str();
        let pair_row_filled = lines[1].matches(filled).count();
        let single_row_filled = lines[2].matches(filled).count();
        assert!(pair_row_filled > 0);
        assert_eq!(
            pair_row_filled, single_row_filled,
            "a maxed single channel row must fill the same number of \
             characters as a maxed side of a radiating pair row"
        );
    }

    #[test]
    fn radiating_pair_and_unpaired_rows_start_their_bars_at_the_same_column() {
        // The whole point of sharing one column grid: an unpaired
        // channel's bar must start at exactly the same x position a
        // paired row's left bar would, not just the same length.
        // max_volume_percent = 100 so FL's bar is fully (not just
        // partially) filled - otherwise its "grow from center" partial
        // fill would leave a genuine empty prefix before the fill
        // starts, and "first filled character" would land inside bar_l
        // rather than at its true left edge (FC's bar, filling classic
        // left-to-right since it's unpaired, has no such prefix, so the
        // two would silently disagree on what "bar start" means
        // without this).
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let node = test_node(Some(vec![fl, fr, fc]), vec![1.0, 0.0, 1.0]);
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"\nmax_volume_percent = 100.0",
        );

        let lines = render_node_lines(&config, &node, false, false, None);
        assert_eq!(lines.len(), 3); // header + 1 pair row + 1 single row

        let filled = config.char_set.volume_filled.as_str();
        let pair_row_bar_start =
            lines[1].find(filled).expect("FL's bar is fully filled");
        let single_row_bar_start =
            lines[2].find(filled).expect("FC's bar is fully filled");
        assert_eq!(
            pair_row_bar_start, single_row_bar_start,
            "an unpaired row's bar must start at the same column as a \
             paired row's left bar"
        );
    }

    #[test]
    fn expand_unpaired_channel_bars_stretches_into_the_freed_space() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let node = test_node(Some(vec![fl, fr, fc]), vec![1.0, 1.0, 1.0]);

        let narrow_config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"\nmax_volume_percent = 100.0",
        );
        let expanded_config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"\nmax_volume_percent = 100.0\n\
             expand_unpaired_channel_bars = true",
        );
        let filled = narrow_config.char_set.volume_filled.as_str();

        let narrow_lines =
            render_node_lines(&narrow_config, &node, false, false, None);
        let expanded_lines =
            render_node_lines(&expanded_config, &node, false, false, None);

        let unpaired_bar_end = |lines: &[String]| -> usize {
            lines[2].rfind(filled).expect("FC's bar is fully filled")
        };
        assert!(
            unpaired_bar_end(&expanded_lines) > unpaired_bar_end(&narrow_lines),
            "expand_unpaired_channel_bars should let the unpaired row's \
             bar reach further right than the paired-row-matching half \
             width"
        );

        let paired_bar_end = |lines: &[String]| -> usize {
            lines[1]
                .rfind(filled)
                .expect("FL/FR's bars are fully filled")
        };
        assert_eq!(
            paired_bar_end(&narrow_lines),
            paired_bar_end(&expanded_lines),
            "expand_unpaired_channel_bars must not affect a paired row's \
             own width"
        );
    }

    #[test]
    fn radiating_pair_label_short_style_omits_the_lr_suffix() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let node = test_node(Some(vec![fl, fr, fc]), vec![0.5, 0.5, 0.5]);
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             pair_label_style = \"short\"\npeaks = \"off\"",
        );

        let lines = render_node_lines(&config, &node, false, false, None);

        assert!(lines[1].contains('F'));
        assert!(!lines[1].contains("L|R"));
    }

    #[test]
    fn radiating_row_mute_overrides_only_the_percent_labels() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let mut node = test_node(Some(vec![fl, fr, fc]), vec![1.0, 1.0, 1.0]);
        node.mute = true;
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"",
        );

        let lines = render_node_lines(&config, &node, false, false, None);
        assert_eq!(lines.len(), 3); // header + pair row + single row

        // The pair row's divider must still render (not collapsed into
        // one centered "muted" spanning the whole row), and both rows'
        // percent text becomes "muted".
        assert!(
            lines[1].contains('|'),
            "the paired row's center divider must still render while muted"
        );
        assert_eq!(lines[1].matches("muted").count(), 2);
        assert_eq!(lines[2].matches("muted").count(), 1);
    }

    #[test]
    fn radiating_pair_row_honors_forced_mono_peaks() {
        use crate::atomic_f32::AtomicF32;
        use std::sync::Arc;

        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let mut node = test_node(Some(vec![fl, fr, fc]), vec![1.0, 0.0, 1.0]);
        node.peaks = Some(Arc::from([
            AtomicF32::new(1.0),
            AtomicF32::new(0.0),
            AtomicF32::new(1.0),
        ]));
        // extracompat's meter_center_left_active ('[') only ever appears
        // in a render_stereo call - render_mono only ever shows the
        // right-side center char (']'). Checking for '[' distinguishes
        // "rendered as a stereo split" from "rendered as one mono gauge"
        // even though meter_left_active/meter_right_active/mono's own
        // active glyph are all '#' in extracompat.
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             char_set = \"extracompat\"\npeaks = \"mono\"",
        );

        let lines = render_node_lines(&config, &node, false, false, None);

        assert!(
            !lines[1].contains('['),
            "peaks = \"mono\" must not render a stereo split even for a \
             detected pair"
        );
        assert!(
            lines[1].contains('#'),
            "a forced-mono pair row still needs some kind of gauge"
        );
    }

    #[test]
    fn paired_row_meter_uses_the_same_stock_glyphs_as_a_whole_node_meter() {
        // Per-row monitors reuse the project's existing stock meter
        // glyphs (meter_left/right/center_*) verbatim - no distinct
        // "channel" glyph. A pair row's gauge is a real stereo split
        // (render_stereo, same as MeterWidget's own whole-node stereo
        // meter); an unpaired row's is a mono gauge (render_mono).
        use crate::atomic_f32::AtomicF32;
        use std::sync::Arc;

        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let mut node = test_node(Some(vec![fl, fr, fc]), vec![1.0, 1.0, 1.0]);
        node.peaks = Some(Arc::from([
            AtomicF32::new(1.0),
            AtomicF32::new(1.0),
            AtomicF32::new(1.0),
        ]));
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"auto\"",
        );

        let lines = render_node_lines(&config, &node, false, false, None);
        assert_eq!(lines.len(), 3); // header + 1 pair row + 1 single row

        let meter_active = config.char_set.meter_left_active.as_str();
        assert!(
            lines[1].contains(meter_active),
            "a pair row's monitor gauge should use the stock \
             meter_left/right glyph"
        );
        assert!(
            lines[2].contains(meter_active),
            "an unpaired row's monitor gauge should use the same stock \
             glyph too"
        );
    }

    #[test]
    fn meter_channel_overrides_apply_to_split_rows_but_not_the_classic_meter() {
        // A theme that *does* configure meter_channel_* should see it
        // only on per-row split monitors (ChannelRowWidget/
        // RadiatingRowWidget) - the classic single-row `MeterWidget`
        // (used here by the plain stereo node) must keep using the
        // stock meter_left/right/center glyphs regardless, since it
        // never consults meter_channel_* at all.
        use crate::atomic_f32::AtomicF32;
        use std::sync::Arc;

        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let fc = libspa_sys::SPA_AUDIO_CHANNEL_FC;
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"auto\"\n\
             char_set = \"test\"\n\
             [char_sets.test]\n\
             inherit = \"default\"\n\
             meter_channel_left_active = \"\u{2759}\"\n\
             meter_channel_right_active = \"\u{2759}\"",
        );
        let channel_glyph = "\u{2759}";
        let stock_glyph = config.char_set.meter_left_active.as_str();
        assert_ne!(
            channel_glyph, stock_glyph,
            "the test needs a channel glyph genuinely distinct from stock"
        );

        // Classic single-row node (plain FL/FR pair, no third channel) -
        // MeterWidget, never split, must render with the stock glyph.
        let mut classic_node = test_node(Some(vec![fl, fr]), vec![1.0, 1.0]);
        classic_node.peaks =
            Some(Arc::from([AtomicF32::new(1.0), AtomicF32::new(1.0)]));
        let classic_lines =
            render_node_lines(&config, &classic_node, false, false, None);
        // [header, blank spacer, bar+meter] - the classic path leaves a
        // blank spacer line between its header and bar rows.
        assert!(classic_lines[2].contains(stock_glyph));
        assert!(!classic_lines[2].contains(channel_glyph));

        // Split node (FL/FR/FC forces a Stacked/Radiating block) - both
        // the pair row and the unpaired row must use the configured
        // channel glyph instead.
        let mut split_node =
            test_node(Some(vec![fl, fr, fc]), vec![1.0, 1.0, 1.0]);
        split_node.peaks = Some(Arc::from([
            AtomicF32::new(1.0),
            AtomicF32::new(1.0),
            AtomicF32::new(1.0),
        ]));
        let split_lines =
            render_node_lines(&config, &split_node, false, false, None);
        assert_eq!(split_lines.len(), 3); // header + pair row + single row
        assert!(split_lines[1].contains(channel_glyph));
        assert!(split_lines[2].contains(channel_glyph));
    }

    #[test]
    fn node_volume_padding_and_radiating_rows_share_a_bar_start_column() {
        // Not just alignment *within* one node's own split block - every
        // node's volume bar starts at the same column, whether it's a
        // plain Unified single bar, the classic single-pair Radiating
        // fast path, or a row inside a Stacked block.
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;

        // channel_display = "always" so the pair and AUX nodes actually
        // split; a 1-channel node is always Unified regardless, so the
        // same config works for all three. max_volume_percent = 100 so a
        // raw volume of 1.0 fully saturates every bar (the default 150
        // would leave StereoVolumeWidget/RadiatingRowWidget's "filled
        // adjacent to center" bars only 2/3 full, leaving a genuine empty
        // prefix before the fill - "first filled character" wouldn't
        // reliably locate the bar's true start column in that case).
        // expand_unused_label_space explicitly off - this test verifies
        // the baseline cross-widget alignment invariant, which the
        // setting deliberately trades away when on (see its own doc
        // comment).
        let config = config::Config::from_toml_str(
            "channel_display = \"always\"\nsplit_style = \"radiating\"\n\
             peaks = \"off\"\nmax_volume_percent = 100.0\n\
             expand_unused_label_space = false",
        );
        let filled = config.char_set.volume_filled.as_str();
        let bar_start = |lines: &[String]| -> usize {
            lines
                .iter()
                .find_map(|line| line.find(filled))
                .expect("a fully-filled bar somewhere in the rendered node")
        };

        let unified_node = test_node(Some(vec![mono]), vec![1.0]);
        let unified_lines =
            render_node_lines(&config, &unified_node, false, false, None);

        let radiating_node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let radiating_lines =
            render_node_lines(&config, &radiating_node, false, false, None);

        let stacked_node = test_node(Some(vec![aux0, aux1]), vec![1.0, 0.0]);
        let stacked_lines =
            render_node_lines(&config, &stacked_node, false, false, None);

        let unified_start = bar_start(&unified_lines);
        let radiating_start = bar_start(&radiating_lines);
        let stacked_start = bar_start(&stacked_lines);

        assert_eq!(
            unified_start, radiating_start,
            "a Unified node's bar must start at the same column as the \
             classic single-pair Radiating fast path's does"
        );
        assert_eq!(
            radiating_start, stacked_start,
            "the classic single-pair Radiating fast path must start its \
             bar at the same column a Stacked block's radiating row \
             does - bars line up across every node in the list, not \
             just within one node's own split block"
        );
    }

    #[test]
    fn meter_width_percent_moves_the_meter_column_for_classic_and_split_rows_alike(
    ) {
        // A narrower meter_width_percent gives more room to the volume
        // side, pushing the meter's start column further right - for
        // both the classic Unified path and a Stacked block's row alike,
        // since both derive their split from the same config setting.
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;

        let default_config = config::Config::from_toml_str(
            "channel_display = \"always\"\npeaks = \"auto\"\n\
             char_set = \"compat\"",
        );
        let narrow_meter_config = config::Config::from_toml_str(
            "channel_display = \"always\"\npeaks = \"auto\"\n\
             char_set = \"compat\"\n\
             [linked_meter_layout]\nmeter_width_percent = 25.0",
        );
        let meter_glyph = default_config.char_set.meter_left_active.as_str();
        let meter_start = |lines: &[String]| -> usize {
            lines
                .iter()
                .find_map(|line| line.find(meter_glyph))
                .expect("a meter somewhere in the rendered node")
        };

        let unified_node = test_node(Some(vec![mono]), vec![1.0]);
        let stacked_node = test_node(Some(vec![aux0, aux1]), vec![0.5, 0.5]);

        let default_unified_start = meter_start(&render_node_lines(
            &default_config,
            &unified_node,
            false,
            false,
            None,
        ));
        let narrow_unified_start = meter_start(&render_node_lines(
            &narrow_meter_config,
            &unified_node,
            false,
            false,
            None,
        ));
        assert!(
            narrow_unified_start > default_unified_start,
            "a narrower meter_width_percent should push the classic \
             path's meter further right"
        );

        let default_stacked_start = meter_start(&render_node_lines(
            &default_config,
            &stacked_node,
            false,
            false,
            None,
        ));
        let narrow_stacked_start = meter_start(&render_node_lines(
            &narrow_meter_config,
            &stacked_node,
            false,
            false,
            None,
        ));
        assert!(
            narrow_stacked_start > default_stacked_start,
            "a narrower meter_width_percent should push a Stacked row's \
             meter further right too"
        );
    }

    #[test]
    fn right_margin_reclaims_or_reserves_trailing_columns() {
        // An explicit right_margin = 0 override lets content reach
        // further right than a larger explicit right_margin does -
        // checked via the last non-space column used by a fully-filled
        // classic bar+meter row. Uses a Linked-view node, but every view
        // (Unified included) shares the same generic mechanism now.
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let node = test_node(Some(vec![mono]), vec![1.0]);

        let last_used_column = |lines: &[String]| -> usize {
            lines
                .iter()
                .map(|line| line.trim_end().chars().count())
                .max()
                .expect("at least one rendered line")
        };

        let no_margin_config = config::Config::from_toml_str(
            "channel_display = \"always\"\npeaks = \"auto\"\n\
             [linked_meter_layout]\nright_margin = 0",
        );
        let wide_margin_config = config::Config::from_toml_str(
            "channel_display = \"always\"\npeaks = \"auto\"\n\
             [linked_meter_layout]\nright_margin = 10",
        );

        let no_margin_lines =
            render_node_lines(&no_margin_config, &node, false, false, None);
        let wide_margin_lines =
            render_node_lines(&wide_margin_config, &node, false, false, None);

        assert!(
            last_used_column(&no_margin_lines)
                > last_used_column(&wide_margin_lines),
            "right_margin = 0 should let content reach further right \
             than right_margin = 10 does"
        );
    }
}
