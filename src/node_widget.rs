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
use crate::config::{Config, Peaks};
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

pub struct NodeWidget<'a> {
    config: &'a Config,
    device_kind: Option<DeviceKind>,
    node: &'a view::Node,
    selected: bool,
    /// Whether keyboard navigation/volume keys are targeting individual
    /// channels ("Channel mode" - see the multichannel design notes,
    /// §7.3/§7.4). Session-wide, so it affects how every node in the list
    /// renders, not just the selected one.
    channel_mode: bool,
    /// Which channel of *this* node is cursor-targeted, if any. Only
    /// meaningful when `selected` is also true - another node's channel
    /// index has no bearing on this one's marker.
    selected_channel: Option<usize>,
}

impl<'a> NodeWidget<'a> {
    pub fn new(
        config: &'a Config,
        device_kind: Option<DeviceKind>,
        node: &'a view::Node,
        selected: bool,
        channel_mode: bool,
        selected_channel: Option<usize>,
    ) -> Self {
        Self {
            config,
            device_kind,
            node,
            selected,
            channel_mode,
            selected_channel,
        }
    }

    /// Height of a full node display in the default (non-Channel-mode)
    /// layout.
    pub fn height() -> u16 {
        3
    }

    /// Height of `node`'s display given the current channel mode -
    /// channel mode expands a node with more than one channel into one
    /// header line plus one line per channel; everything else renders at
    /// the ordinary fixed `height()`.
    pub fn node_height(channel_mode: bool, node: &view::Node) -> u16 {
        let channel_count = node.volumes.len();
        if channel_mode && channel_count > 1 {
            1 + channel_count as u16
        } else {
            Self::height()
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

        let header_split = row_layout(rows[0]);
        if self.selected {
            Span::styled(
                &self.config.char_set.selector_middle,
                self.config.theme.selector,
            )
            .render(header_split[0], buf);
        }
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

        let channel_count = self.node.volumes.len();
        if self.channel_mode && channel_count > 1 {
            self.render_channel_rows(area, buf, mouse_areas, channel_count);
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

/// The first detected left/right channel pair on `node`, if
/// `show_channel_volumes` display should render one for it. `None` if the
/// node has no `positions` data at all, or none of its channels pair (see
/// `channel_pairing`) - callers fall back to the ordinary single-bar
/// `VolumeWidget` in that case.
fn stereo_pair(node: &view::Node) -> Option<(usize, usize)> {
    let positions = node.positions.as_ref()?;
    channel_pairing::group_channels(positions)
        .into_iter()
        .find_map(|group| match group {
            ChannelGroup::Pair(left, right) => Some((left, right)),
            ChannelGroup::Single(_) => None,
        })
}

fn render_volume(
    config: &Config,
    node: &view::Node,
    area: Rect,
    buf: &mut Buffer,
    mouse_areas: &mut Vec<MouseArea>,
) {
    if config.show_channel_volumes {
        if let Some((left, right)) = stereo_pair(node) {
            StereoVolumeWidget::new(config, node, left, right).render(
                area,
                buf,
                mouse_areas,
            );
            return;
        }
    }
    VolumeWidget::new(config, node).render(area, buf, mouse_areas);
}

struct VolumeWidget<'a> {
    config: &'a Config,
    node: &'a view::Node,
}

impl<'a> VolumeWidget<'a> {
    fn new(config: &'a Config, node: &'a view::Node) -> Self {
        Self { config, node }
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
            let mean = volumes.iter().sum::<f32>() / volumes.len() as f32;
            let volume = mean.cbrt();
            let percent = (volume * 100.0).round() as u32;

            Line::from(Span::styled(
                format!("{percent}%"),
                self.config.theme.volume,
            ))
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

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(4), // label_l
                Constraint::Fill(1),   // bar_l
                Constraint::Length(1), // center
                Constraint::Fill(1),   // bar_r
                Constraint::Length(4), // label_r
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

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(10), // volume_label, e.g. "AUX63 100%"
                Constraint::Min(0),     // volume_bar
            ])
            .spacing(1)
            .split(area);
        let volume_label = layout[0];
        let volume_bar = layout[1];

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

        Line::from(Span::styled(
            format!("{label} {percent}%"),
            self.config.theme.volume,
        ))
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

        if self.node.mute {
            Line::from(format!("{label} muted")).render(volume_label, buf);
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
        render_volume(config, node, area, &mut buf, &mut Vec::new());
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
        let height = NodeWidget::node_height(channel_mode, node);
        let area = Rect::new(0, 0, 40, height);
        let mut buf = Buffer::empty(area);
        NodeWidget::new(
            config,
            None,
            node,
            selected,
            channel_mode,
            selected_channel,
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
    fn show_channel_volumes_off_renders_single_averaged_bar() {
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
    fn show_channel_volumes_on_renders_independent_percentages() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config =
            config::Config::from_toml_str("show_channel_volumes = true");

        let rendered = render_to_string(&config, &node);

        // Both channels' real values show up independently - not a 50%
        // average masking the actual 100%/0% split.
        assert!(rendered.contains("100%"));
        assert!(rendered.contains("0%"));
        assert!(!rendered.contains("50%"));
    }

    #[test]
    fn show_channel_volumes_on_without_a_pair_falls_back_to_single_bar() {
        // Mono - nothing to pair, so this must render exactly like the
        // option was off, even though it's enabled.
        let mono = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
        let node = test_node(Some(vec![mono]), vec![0.5_f32.powi(3)]);
        let config =
            config::Config::from_toml_str("show_channel_volumes = true");

        let rendered = render_to_string(&config, &node);

        assert!(rendered.contains("50%"));
    }

    #[test]
    fn node_height_default_is_unaffected_by_channel_count() {
        let node = test_node(None, vec![1.0, 1.0, 1.0]);
        assert_eq!(NodeWidget::node_height(false, &node), NodeWidget::height());
    }

    #[test]
    fn node_height_channel_mode_expands_one_line_per_channel() {
        let node = test_node(None, vec![1.0, 1.0, 1.0]);
        // 1 header line + 3 channel lines
        assert_eq!(NodeWidget::node_height(true, &node), 4);
    }

    #[test]
    fn node_height_channel_mode_single_channel_unaffected() {
        let node = test_node(None, vec![1.0]);
        assert_eq!(NodeWidget::node_height(true, &node), NodeWidget::height());
    }

    #[test]
    fn channel_mode_renders_one_line_per_channel() {
        let fl = libspa_sys::SPA_AUDIO_CHANNEL_FL;
        let fr = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        let node = test_node(Some(vec![fl, fr]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        assert_eq!(lines.len(), 3); // header + 2 channel rows
        assert!(lines[1].contains("FL 100%"));
        assert!(lines[2].contains("FR 0%"));
    }

    #[test]
    fn channel_mode_labels_fall_back_to_index_without_positions() {
        let node = test_node(None, vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        assert!(lines[1].contains("0 100%"));
        assert!(lines[2].contains("1 0%"));
    }

    #[test]
    fn channel_mode_labels_aux_channels_by_number() {
        let aux0 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
        let aux1 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;
        let node = test_node(Some(vec![aux0, aux1]), vec![1.0, 0.0]);
        let config = config::Config::from_toml_str("");

        let lines = render_node_lines(&config, &node, true, false, None);

        assert!(lines[1].contains("AUX0 100%"));
        assert!(lines[2].contains("AUX1 0%"));
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
        assert_eq!(NodeWidget::node_height(false, &node), 3);
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
}
