//! A Ratatui widget for an interactable list of PipeWire objects.

use std::cmp;
use std::collections::HashSet;

use ratatui::{
    prelude::{Alignment, Buffer, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{ListState, StatefulWidget, Widget},
};

use crossterm::event::{MouseButton, MouseEventKind};
use smallvec::smallvec;

use crate::app::{Action, MouseArea};
use crate::config::{
    ChannelDisplay, ChannelState, ChannelView, Config, PairLabelStyle,
    SplitStyle, UnifiedImbalance,
};
use crate::device_kind::DeviceKind;
use crate::device_widget::DeviceWidget;
use crate::dropdown_widget::DropdownWidget;
use crate::node_widget::NodeWidget;
use crate::view::{self, ListKind, VolumeAdjustment};
use crate::wirehose::ObjectId;

/// ObjectList stores information for filtering and displaying a subset of
/// objects from a [`View`](`crate::view::View`).
///
/// Control operations pertaining to individual objects are handled here.
#[derive(Default)]
pub struct ObjectList {
    /// Index of the first object in viewport
    top: usize,
    /// ID of the currently selected object
    pub selected: Option<ObjectId>,
    /// Which set of objects to use from the View
    list_kind: ListKind,
    /// Default device type to use for defaults and node rendering
    device_kind: Option<DeviceKind>,
    /// Target dropdown state
    pub dropdown_state: ListState,
    /// Targets
    pub targets: Vec<(view::Target, String)>,
    /// Whether keyboard navigation and volume actions target individual
    /// channels of the selected node instead of the whole node together.
    /// See `selected_channel`.
    pub channel_mode: bool,
    /// Which channel of the selected node is targeted. Only meaningful
    /// while `channel_mode` is on; `None` whenever `channel_mode` is off,
    /// or the selected object has fewer than two channels to cycle
    /// through (nothing to individually target).
    pub selected_channel: Option<usize>,
    /// Whether a node's volume is ever shown as more than one bar/row
    /// when `channel_mode` is off (linked setting). Seeded from
    /// `Config::channel_display` at startup; toggled live via
    /// `Action::CycleChannelDisplay`.
    pub channel_display: ChannelDisplay,
    /// See `Config::unified_imbalance`. Seeded from config; no runtime
    /// toggle yet.
    pub unified_imbalance: UnifiedImbalance,
    /// See `Config::split_style`. Seeded from config; no runtime toggle
    /// yet.
    pub split_style: SplitStyle,
    /// See `Config::pair_label_style`. Seeded from config; no runtime
    /// toggle yet.
    pub pair_label_style: PairLabelStyle,
}

impl ObjectList {
    pub fn new(list_kind: ListKind, device_kind: Option<DeviceKind>) -> Self {
        Self {
            top: 0,
            selected: None,
            list_kind,
            device_kind,
            ..Default::default()
        }
    }

    pub fn down(&mut self, view: &view::View) {
        if self.dropdown_state.selected().is_some() {
            self.dropdown_state.select_next();
            return;
        }

        // In channel mode, step to the selected node's next channel before
        // advancing to the next node - the channel cursor folds into the
        // same up/down navigation stream rather than needing its own keys.
        if let (Some(object_id), Some(channel)) =
            (self.selected, self.selected_channel)
        {
            if channel.saturating_add(1) < self.channel_count(view, object_id) {
                self.selected_channel = Some(channel + 1);
                return;
            }
        }

        let new_selected = view.next_id(self.list_kind, self.selected);
        if new_selected.is_some() {
            self.select(view, new_selected, false);
        }
    }

    pub fn up(&mut self, view: &view::View) {
        if self.dropdown_state.selected().is_some() {
            self.dropdown_state.select_previous();
            return;
        }

        if let Some(channel) = self.selected_channel {
            if channel > 0 {
                self.selected_channel = Some(channel - 1);
                return;
            }
        }

        let new_selected = view.previous_id(self.list_kind, self.selected);
        if new_selected.is_some() {
            // Arriving via `up()` lands on the *last* channel, mirroring
            // how `down()` lands on the first - so up/down are exact
            // reverses of each other rather than both always landing on
            // channel 0.
            self.select(view, new_selected, true);
        }
    }

    /// Toggles whether keyboard navigation and volume actions target
    /// individual channels of the selected node ("Channel mode" - see the
    /// multichannel design notes) instead of the whole node together.
    pub fn toggle_channel_mode(&mut self, view: &view::View) {
        self.channel_mode = !self.channel_mode;
        self.selected_channel =
            self.initial_channel(view, self.selected, false);
    }

    /// Cycles the display axis between showing a node's volume as one
    /// combined bar/row ("unified") and always splitting it ("always") -
    /// see `Config::channel_display`. Independent of `channel_mode`.
    pub fn cycle_channel_display(&mut self) {
        self.channel_display = match self.channel_display {
            ChannelDisplay::Unified => ChannelDisplay::Always,
            ChannelDisplay::Always => ChannelDisplay::Unified,
        };
    }

    /// The current `ChannelView` - see `ChannelState::view`.
    pub fn channel_view(&self) -> ChannelView {
        self.channel_state().view()
    }

    /// Switches directly to the given `ChannelView` - see
    /// `Action::SelectView`. `unified_imbalance`/`split_style`/
    /// `pair_label_style` are untouched; they're orthogonal display
    /// refinements, not part of the view itself.
    pub fn select_view(&mut self, target: ChannelView, view: &view::View) {
        match target {
            ChannelView::Unified => {
                self.channel_mode = false;
                self.channel_display = ChannelDisplay::Unified;
            }
            ChannelView::Linked => {
                self.channel_mode = false;
                self.channel_display = ChannelDisplay::Always;
            }
            ChannelView::Channels => {
                self.channel_mode = true;
            }
        }
        self.selected_channel =
            self.initial_channel(view, self.selected, false);
    }

    /// Advances to the next `ChannelView` in `view_cycle`, wrapping - see
    /// `Action::CycleView`. If the current view isn't in `view_cycle`
    /// (reached via `select_view` while it was excluded), lands on
    /// `view_cycle`'s first entry instead of trying to guess a "next"
    /// relative to a view that isn't part of the cycle.
    pub fn cycle_channel_view(
        &mut self,
        view_cycle: &[ChannelView],
        view: &view::View,
    ) {
        let current = self.channel_view();
        let next = match view_cycle.iter().position(|v| *v == current) {
            Some(i) => view_cycle[(i + 1) % view_cycle.len()],
            None => *view_cycle.first().unwrap_or(&ChannelView::Unified),
        };
        self.select_view(next, view);
    }

    /// Bundles the display/setting axes for passing to `NodeWidget`.
    pub fn channel_state(&self) -> ChannelState {
        ChannelState {
            channel_mode: self.channel_mode,
            channel_display: self.channel_display,
            unified_imbalance: self.unified_imbalance,
            split_style: self.split_style,
            pair_label_style: self.pair_label_style,
        }
    }

    /// Number of channels the given object has to cycle through in channel
    /// mode. Always 0 for devices - device rows don't carry per-channel
    /// volume data the way nodes do.
    fn channel_count(&self, view: &view::View, object_id: ObjectId) -> usize {
        if matches!(self.list_kind, ListKind::Device) {
            return 0;
        }
        view.nodes
            .get(&object_id)
            .map_or(0, |node| node.volumes.len())
    }

    /// The channel a newly-selected object should start on, if channel
    /// mode is active and the object has more than one channel to cycle
    /// through - `None` otherwise (including whenever channel mode is
    /// off). `from_end` picks which end: `false` for the first channel
    /// (landing via `down()`, or any non-directional selection), `true`
    /// for the last (landing via `up()`, so up/down are exact reverses of
    /// each other instead of both always landing on channel 0).
    fn initial_channel(
        &self,
        view: &view::View,
        object_id: Option<ObjectId>,
        from_end: bool,
    ) -> Option<usize> {
        if !self.channel_mode {
            return None;
        }
        let object_id = object_id?;
        let count = self.channel_count(view, object_id);
        if count <= 1 {
            return None;
        }
        Some(if from_end { count - 1 } else { 0 })
    }

    fn dropdown_open(&mut self, view: &view::View) {
        let targets = match self.list_kind {
            ListKind::Node(_) => self
                .selected
                .and_then(|object_id| view.node_targets(object_id)),
            ListKind::Device => self
                .selected
                .and_then(|object_id| view.device_targets(object_id)),
        };
        if let Some((targets, index)) = targets {
            if !targets.is_empty() {
                self.targets = targets;
                self.dropdown_state.select(Some(index));
            }
        }
    }

    fn selected_target(&self) -> Option<&view::Target> {
        self.dropdown_state
            .selected()
            .and_then(|index| self.targets.get(index))
            .map(|(target, _)| target)
    }

    pub fn dropdown_activate(&mut self, view: &view::View) {
        // Just open the dropdown if it's not showing yet.
        if self.dropdown_state.selected().is_none() {
            self.dropdown_open(view);
            return;
        }

        if let (Some(object_id), Some(&target)) =
            (self.selected, self.selected_target())
        {
            view.set_target(object_id, target);
        };

        self.dropdown_state.select(None);
    }

    pub fn dropdown_close(&mut self) {
        self.dropdown_state.select(None);
    }

    pub fn set_target(&mut self, view: &view::View, target: view::Target) {
        self.dropdown_state.select(None);
        if let Some(object_id) = self.selected {
            view.set_target(object_id, target);
        };
    }

    pub fn toggle_mute(&mut self, view: &view::View) {
        if matches!(self.list_kind, ListKind::Device) {
            return;
        }
        if let Some(node_id) = self.selected {
            view.mute(node_id);
        }
    }

    pub fn set_absolute_volume(
        &mut self,
        view: &view::View,
        volume: f32,
        max: Option<f32>,
    ) -> bool {
        if matches!(self.list_kind, ListKind::Device) {
            return false;
        }
        if let Some(node_id) = self.selected {
            return view.volume(
                node_id,
                VolumeAdjustment::Absolute(volume),
                max,
            );
        }
        false
    }

    pub fn set_channel_absolute_volume(
        &mut self,
        view: &view::View,
        channel: usize,
        volume: f32,
        max: Option<f32>,
    ) -> bool {
        if matches!(self.list_kind, ListKind::Device) {
            return false;
        }
        if let Some(node_id) = self.selected {
            return view.channel_volume(
                node_id,
                channel,
                VolumeAdjustment::Absolute(volume),
                max,
            );
        }
        false
    }

    pub fn set_channel_relative_volume(
        &mut self,
        view: &view::View,
        channel: usize,
        volume: f32,
        max: Option<f32>,
    ) -> bool {
        if matches!(self.list_kind, ListKind::Device) {
            return false;
        }
        if let Some(node_id) = self.selected {
            return view.channel_volume(
                node_id,
                channel,
                VolumeAdjustment::Relative(volume),
                max,
            );
        }
        false
    }

    pub fn set_relative_volume(
        &mut self,
        view: &view::View,
        volume: f32,
        max: Option<f32>,
    ) -> bool {
        if matches!(self.list_kind, ListKind::Device) {
            return false;
        }
        if let Some(node_id) = self.selected {
            return view.volume(
                node_id,
                VolumeAdjustment::Relative(volume),
                max,
            );
        }
        false
    }

    pub fn set_default(&mut self, view: &view::View) {
        if matches!(self.list_kind, ListKind::Device) {
            return;
        }
        if let Some(node_id) = self.selected {
            if let Some(device_kind) = self.device_kind {
                view.set_default(node_id, device_kind);
            } else {
                view.set_target(node_id, view::Target::Default);
            }
        }
    }

    fn selected_index(&self, view: &view::View) -> Option<usize> {
        self.selected
            .and_then(|selected| view.position(self.list_kind, selected))
    }

    fn select(
        &mut self,
        view: &view::View,
        object_id: Option<ObjectId>,
        from_end: bool,
    ) {
        self.selected = object_id;
        self.selected_channel = self.initial_channel(view, object_id, from_end);
        // Close the dropdown in case it is open for the previously-selected
        // object. This can happen when the object is removed from PipeWire
        // while the dropdown is open.
        self.dropdown_close();
    }

    /// Returns a set of object IDs of the visible objects. This includes all
    /// dependencies that affect the display of the objects.
    pub fn visible_objects(
        &self,
        area: &Rect,
        view: &view::View,
    ) -> HashSet<ObjectId> {
        let objects = view.object_ids(self.list_kind);

        let last =
            cmp::min(objects.len(), self.top + self.visible_count(view, area));

        // Always include object 0 - the global PipeWire state.
        let mut visible_objects = HashSet::from([ObjectId::from_raw_id(0)]);

        for object_id in objects[self.top..last].iter().cloned() {
            visible_objects.insert(object_id);
            if let Some(node) = view.nodes.get(&object_id) {
                // Add linked client and device.
                visible_objects.extend(node.client_id);
                visible_objects.extend(node.device_info.map(|(id, _, _)| id));

                // Add the target and any linked client and device.
                if let ListKind::Node(node_kind) = self.list_kind {
                    if let Some(target_id) = node
                        .target
                        .and_then(|target| target.resolve(view, node_kind))
                    {
                        visible_objects.insert(target_id);
                        if let Some(target_node) = view.nodes.get(&target_id) {
                            visible_objects.extend(target_node.client_id);
                            visible_objects.extend(
                                target_node.device_info.map(|(id, _, _)| id),
                            );
                        }
                    }
                }
            }
        }

        visible_objects
    }

    /// Returns the number of objects visible.
    fn visible_count(&self, view: &view::View, area: &Rect) -> usize {
        let (_, list_area, _) = self.areas(area);
        let spacing = match self.list_kind {
            ListKind::Node(_) => NodeWidget::spacing(),
            ListKind::Device => DeviceWidget::spacing(),
        };

        let mut used = 0u16;
        let mut count = 0usize;
        for height in self.item_heights(view) {
            let step = height.saturating_add(spacing);
            if used.saturating_add(step) > list_area.height {
                break;
            }
            used = used.saturating_add(step);
            count += 1;
        }
        count
    }

    /// Raw (spacing-excluded) height of every object visible from `top`
    /// onward, in list order. Node heights vary with the current channel
    /// display/setting state and each node's own channel count/values
    /// (see `NodeWidget::node_height`); device heights are always
    /// uniform.
    fn item_heights(&self, view: &view::View) -> Vec<u16> {
        let channel_state = self.channel_state();
        match self.list_kind {
            ListKind::Node(node_kind) => view
                .full_nodes(node_kind)
                .iter()
                .skip(self.top)
                .map(|node| NodeWidget::node_height(channel_state, node))
                .collect(),
            ListKind::Device => {
                let count = view.full_devices().len().saturating_sub(self.top);
                vec![DeviceWidget::height(); count]
            }
        }
    }

    /// Reconciles changes to objects, viewport, and selection.
    pub fn update(&mut self, area: Rect, view: &view::View) {
        let selected_index = self.selected_index(view).or_else(|| {
            // There's nothing selected! Select the first item and try again.
            self.select(view, view.next_id(self.list_kind, None), false);
            self.selected_index(view)
        });

        let objects_len = view.len(self.list_kind);

        let visible_count = self.visible_count(view, &area);

        // If objects were removed and the viewport is now below the visible
        // objects, move the viewport up so that the bottom of the object list
        // is visible.
        if self.top >= objects_len {
            self.top = objects_len.saturating_sub(visible_count);
        }

        // Make sure the selected object is visible and adjust the viewport
        // if necessary.
        if self.selected.is_some() {
            match selected_index {
                Some(selected_index) => {
                    if selected_index >= self.top.saturating_add(visible_count)
                    {
                        // The selection is below the viewport. Reposition the
                        // viewport so that the selected item is at the bottom.
                        let visible_count_except_last =
                            visible_count.saturating_sub(1);
                        self.top = selected_index
                            .saturating_sub(visible_count_except_last);
                    } else if selected_index < self.top {
                        // The selected item is above the viewport. Reposition
                        // so that it's the first visible item.
                        self.top = selected_index;
                    }
                }
                None => self.select(view, None, false), // The selected object is gone!
            }
        }
    }

    fn areas(&self, area: &Rect) -> (Rect, Rect, Rect) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header_area
                Constraint::Min(0),    // list_area
                Constraint::Length(1), // footer_area
            ])
            .split(*area);

        (layout[0], layout[1], layout[2])
    }
}

pub struct ObjectListWidget<'a, 'b> {
    pub object_list: &'a mut ObjectList,
    pub view: &'a view::View<'b>,
    pub config: &'a Config,
    /// Seconds elapsed since `App` started - the shared time reference
    /// for `unified_imbalance = "cycle"`'s stateless phase-offset
    /// rendering. See `NodeWidget::node_height`/`render` for how it's
    /// used; irrelevant (and unused) for device rows.
    pub elapsed_seconds: f32,
}

struct ObjectListRenderContext<'a> {
    list_area: Rect,
    objects_layout: &'a [Rect],
    objects_visible: usize,
}

impl ObjectListWidget<'_, '_> {
    fn render_node_list(
        &mut self,
        node_kind: view::NodeKind,
        context: ObjectListRenderContext,
        area: Rect,
        buf: &mut Buffer,
        mouse_areas: &mut Vec<MouseArea>,
    ) {
        let all_objects = self.view.full_nodes(node_kind);
        let objects = all_objects
            .iter()
            .skip(self.object_list.top)
            // Take one extra so we can render a partial node at the bottom of
            // the area.
            .take(context.objects_visible.saturating_add(1));

        let objects_and_areas: Vec<(&&view::Node, &Rect)> =
            objects.zip(context.objects_layout.iter()).collect();
        for (object, &object_area) in &objects_and_areas {
            let selected = self
                .object_list
                .selected
                .map(|id| id == object.object_id)
                .unwrap_or_default();
            NodeWidget::new(
                self.config,
                self.object_list.device_kind,
                object,
                selected,
                self.object_list.channel_state(),
                self.object_list.selected_channel,
                self.elapsed_seconds,
            )
            .render(object_area, buf, mouse_areas);
        }

        // Show the target dropdown?
        if self.object_list.dropdown_state.selected().is_some() {
            // Get the area for the selected object
            if let Some((_, object_area)) =
                objects_and_areas.iter().find(|(object, _)| {
                    self.object_list
                        .selected
                        .map(|id| id == object.object_id)
                        .unwrap_or_default()
                })
            {
                DropdownWidget::new(
                    self.object_list,
                    &NodeWidget::dropdown_area(
                        self.object_list,
                        &context.list_area,
                        object_area,
                    ),
                    self.config,
                )
                .render(area, buf, mouse_areas);
            }
        }
    }

    fn render_device_list(
        &mut self,
        context: ObjectListRenderContext,
        area: Rect,
        buf: &mut Buffer,
        mouse_areas: &mut Vec<MouseArea>,
    ) {
        let all_objects = self.view.full_devices();
        let objects = all_objects
            .iter()
            .skip(self.object_list.top)
            // Take one extra so we can render a partial node at the bottom of
            // the area.
            .take(context.objects_visible.saturating_add(1));

        let objects_and_areas: Vec<(&&view::Device, &Rect)> =
            objects.zip(context.objects_layout.iter()).collect();
        for (object, &object_area) in &objects_and_areas {
            let selected = self
                .object_list
                .selected
                .map(|id| id == object.object_id)
                .unwrap_or_default();
            DeviceWidget::new(object, selected, self.config).render(
                object_area,
                buf,
                mouse_areas,
            );
        }

        // Show the target dropdown?
        if self.object_list.dropdown_state.selected().is_some() {
            // Get the area for the selected object
            if let Some((_, object_area)) =
                objects_and_areas.iter().find(|(object, _)| {
                    self.object_list
                        .selected
                        .map(|id| id == object.object_id)
                        .unwrap_or_default()
                })
            {
                DropdownWidget::new(
                    self.object_list,
                    &DeviceWidget::dropdown_area(
                        self.object_list,
                        &context.list_area,
                        object_area,
                    ),
                    self.config,
                )
                .render(area, buf, mouse_areas);
            }
        }
    }
}

impl StatefulWidget for &mut ObjectListWidget<'_, '_> {
    type State = Vec<MouseArea>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mouse_areas = state;

        let (header_area, list_area, footer_area) =
            self.object_list.areas(&area);

        mouse_areas.push((
            header_area,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![Action::MoveUp],
        ));

        mouse_areas.push((
            footer_area,
            smallvec![MouseEventKind::Down(MouseButton::Left)],
            smallvec![Action::MoveDown],
        ));

        mouse_areas.push((
            list_area,
            smallvec![MouseEventKind::ScrollUp],
            smallvec![Action::MoveUp],
        ));

        mouse_areas.push((
            list_area,
            smallvec![MouseEventKind::ScrollDown],
            smallvec![Action::MoveDown],
        ));

        let spacing = match self.object_list.list_kind {
            ListKind::Node(_) => NodeWidget::spacing(),
            ListKind::Device => DeviceWidget::spacing(),
        };

        // Real, possibly heterogeneous per-object heights (a node in
        // channel mode is taller than one that isn't) - walked from `top`
        // to find how many whole objects fit, mirroring
        // `ObjectList::visible_count`'s own walk.
        let item_heights = self.object_list.item_heights(self.view);
        let mut used = 0u16;
        let mut objects_visible = 0usize;
        for &item_height in &item_heights {
            let step = item_height.saturating_add(spacing);
            if used.saturating_add(step) > list_area.height {
                break;
            }
            used = used.saturating_add(step);
            objects_visible += 1;
        }

        let len = self.view.len(self.object_list.list_kind);

        // Indicate we can scroll up if there are objects above the viewport.
        if self.object_list.top > 0 {
            Line::from(Span::styled(
                &self.config.char_set.list_more,
                self.config.theme.list_more,
            ))
            .alignment(Alignment::Center)
            .render(header_area, buf);
        }

        // Indicate we can scroll down if there are objects below the
        // viewport, with an exception for when the last row is partially
        // rendered but still has all the important parts rendered,
        // excluding margins, etc.
        let is_bottom_last =
            self.object_list.top.saturating_add(objects_visible)
                == len.saturating_sub(1);
        let is_bottom_enough = item_heights
            .get(objects_visible)
            .map_or(true, |&h| list_area.height.saturating_sub(used) >= h);
        if self.object_list.top.saturating_add(objects_visible) < len
            && !(is_bottom_last && is_bottom_enough)
        {
            Line::from(Span::styled(
                &self.config.char_set.list_more,
                self.config.theme.list_more,
            ))
            .alignment(Alignment::Center)
            .render(footer_area, buf);
        }

        let objects_layout = {
            let mut constraints: Vec<Constraint> = item_heights
                .iter()
                .take(objects_visible)
                .map(|&h| Constraint::Length(h))
                .collect();
            // A variable-length constraint for a partial last object
            let partial_height =
                item_heights.get(objects_visible).copied().unwrap_or(0);
            constraints.push(Constraint::Max(partial_height));

            Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .spacing(spacing)
                .split(list_area)
        };

        match self.object_list.list_kind {
            ListKind::Node(node_kind) => {
                self.render_node_list(
                    node_kind,
                    ObjectListRenderContext {
                        list_area,
                        objects_layout: &objects_layout,
                        objects_visible,
                    },
                    area,
                    buf,
                    mouse_areas,
                );
            }
            ListKind::Device => {
                self.render_device_list(
                    ObjectListRenderContext {
                        list_area,
                        objects_layout: &objects_layout,
                        objects_visible,
                    },
                    area,
                    buf,
                    mouse_areas,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::mock;
    use crate::view::{ListKind, NodeKind, View};
    use crate::wirehose::{state::State, PropertyStore, StateEvent};
    use std::sync::Arc;

    fn init() -> (State, mock::WirehoseHandle<'static>) {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        for i in 1..11 {
            let object_id = ObjectId::from_raw_id(i);
            let mut props = PropertyStore::default();
            props.set_node_description(String::from("Test node"));
            props.set_media_class(String::from("Stream/Output/Audio"));
            props.set_media_name(String::from("Media name"));
            props.set_node_name(String::from("Node name"));
            props.set_object_serial(i as u64);
            let props = props;

            let events = vec![
                StateEvent::NodeProperties { object_id, props },
                StateEvent::NodePositions {
                    object_id,
                    positions: vec![0, 1],
                },
                StateEvent::NodeStreamStarted {
                    object_id,
                    rate: 44100,
                    peaks: Arc::new([0.0.into(), 0.0.into()]),
                },
                StateEvent::NodeVolumes {
                    object_id,
                    volumes: vec![0.0, 0.0],
                },
                StateEvent::NodeMute {
                    object_id,
                    mute: false,
                },
            ];
            for event in events {
                state.update(event);
            }
        }

        (state, wirehose)
    }

    /// Helper to create a minimal node with the given media class.
    fn create_node(
        state: &mut State,
        object_id: ObjectId,
        media_class: &str,
        node_name: &str,
    ) {
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Test node"));
        props.set_media_class(String::from(media_class));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from(node_name));
        props.set_object_serial(u32::from(object_id) as u64);

        state.update(StateEvent::NodeProperties { object_id, props });
        state.update(StateEvent::NodeVolumes {
            object_id,
            volumes: vec![1.0, 1.0],
        });
        state.update(StateEvent::NodeMute {
            object_id,
            mute: false,
        });
    }

    #[test]
    fn object_list_up_overflow() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        // + 2 for header and footer
        let rect = Rect::new(0, 0, 80, height * 3 + 2);
        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        // Select first object
        object_list.down(&view);
        assert_eq!(object_list.top, 0);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(1)));

        object_list.up(&view);
        object_list.update(rect, &view);
        assert_eq!(object_list.top, 0);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(1)));
    }

    #[test]
    fn object_list_down_overflow() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        // + 2 for header and footer
        let rect = Rect::new(0, 0, 80, height * 3 + 2);
        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        // Select first object
        object_list.down(&view);
        assert_eq!(object_list.top, 0);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(1)));

        let nodes_len = view.nodes.len();

        for _ in 0..(nodes_len * 2) {
            object_list.down(&view);
        }

        object_list.update(rect, &view);
        assert_eq!(object_list.top, 7);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(10)));
    }

    #[test]
    fn channel_mode_down_cycles_channels_before_advancing_node() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.channel_mode = true;

        // Selecting the first object lands on its first channel - every
        // `init()` node has 2 channels.
        object_list.down(&view);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(1)));
        assert_eq!(object_list.selected_channel, Some(0));

        // Down again steps to channel 1 of the same node.
        object_list.down(&view);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(1)));
        assert_eq!(object_list.selected_channel, Some(1));

        // Down again, past the last channel, advances to the next node and
        // resets to its first channel.
        object_list.down(&view);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(2)));
        assert_eq!(object_list.selected_channel, Some(0));
    }

    #[test]
    fn channel_mode_up_cycles_channels_before_receding_node() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.channel_mode = true;

        // Walk down to node 2, channel 1.
        object_list.down(&view);
        object_list.down(&view);
        object_list.down(&view);
        object_list.down(&view);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(2)));
        assert_eq!(object_list.selected_channel, Some(1));

        // Up steps back to channel 0 of the same node.
        object_list.up(&view);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(2)));
        assert_eq!(object_list.selected_channel, Some(0));

        // Up again recedes to the previous node, landing on its *last*
        // channel - up/down are exact reverses of each other.
        object_list.up(&view);
        assert_eq!(object_list.selected, Some(ObjectId::from_raw_id(1)));
        assert_eq!(object_list.selected_channel, Some(1));
    }

    #[test]
    fn channel_mode_up_and_down_are_exact_reverses() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.channel_mode = true;

        // Walk down 5 steps, recording (selected, selected_channel) after
        // each one.
        let mut visited = Vec::new();
        for _ in 0..5 {
            object_list.down(&view);
            visited.push((object_list.selected, object_list.selected_channel));
        }

        // Walking back up the same number of steps should retrace exactly
        // the same sequence of (node, channel) pairs, in reverse, ending
        // one step before the final down() landed.
        for expected in visited[..4].iter().rev() {
            object_list.up(&view);
            assert_eq!(
                (object_list.selected, object_list.selected_channel),
                *expected
            );
        }
    }

    #[test]
    fn channel_mode_skips_cycling_for_single_channel_node() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        let mono_id = ObjectId::from_raw_id(1);
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Mono node"));
        props.set_media_class(String::from("Stream/Output/Audio"));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from("mono"));
        props.set_object_serial(1);
        state.update(StateEvent::NodeProperties {
            object_id: mono_id,
            props,
        });
        state.update(StateEvent::NodeVolumes {
            object_id: mono_id,
            volumes: vec![0.0],
        });
        state.update(StateEvent::NodeMute {
            object_id: mono_id,
            mute: false,
        });

        let stereo_id = ObjectId::from_raw_id(2);
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Stereo node"));
        props.set_media_class(String::from("Stream/Output/Audio"));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from("stereo"));
        props.set_object_serial(2);
        state.update(StateEvent::NodeProperties {
            object_id: stereo_id,
            props,
        });
        state.update(StateEvent::NodeVolumes {
            object_id: stereo_id,
            volumes: vec![0.0, 0.0],
        });
        state.update(StateEvent::NodeMute {
            object_id: stereo_id,
            mute: false,
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.channel_mode = true;

        // The mono node has nothing to cycle through.
        object_list.down(&view);
        assert_eq!(object_list.selected, Some(mono_id));
        assert_eq!(object_list.selected_channel, None);

        // Down again moves straight to the next (stereo) node, which does.
        object_list.down(&view);
        assert_eq!(object_list.selected, Some(stereo_id));
        assert_eq!(object_list.selected_channel, Some(0));
    }

    #[test]
    fn toggle_channel_mode_sets_initial_channel_for_current_selection() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.down(&view);
        assert_eq!(object_list.selected_channel, None);

        object_list.toggle_channel_mode(&view);
        assert!(object_list.channel_mode);
        assert_eq!(object_list.selected_channel, Some(0));

        object_list.toggle_channel_mode(&view);
        assert!(!object_list.channel_mode);
        assert_eq!(object_list.selected_channel, None);
    }

    #[test]
    fn channel_view_derives_from_channel_mode_and_channel_display() {
        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        assert_eq!(object_list.channel_view(), ChannelView::Unified);

        object_list.channel_display = ChannelDisplay::Always;
        assert_eq!(object_list.channel_view(), ChannelView::Linked);

        // channel_mode wins regardless of channel_display - a lone
        // channel_mode = true node with channel_display left "unified"
        // still renders split (see NodeWidget), so it must report as
        // Channels here too.
        object_list.channel_mode = true;
        assert_eq!(object_list.channel_view(), ChannelView::Channels);
        object_list.channel_display = ChannelDisplay::Unified;
        assert_eq!(object_list.channel_view(), ChannelView::Channels);
    }

    #[test]
    fn select_view_switches_channel_mode_and_channel_display() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.down(&view);

        object_list.select_view(ChannelView::Linked, &view);
        assert!(!object_list.channel_mode);
        assert_eq!(object_list.channel_display, ChannelDisplay::Always);
        assert_eq!(object_list.selected_channel, None);

        object_list.select_view(ChannelView::Channels, &view);
        assert!(object_list.channel_mode);
        assert_eq!(object_list.selected_channel, Some(0));

        object_list.select_view(ChannelView::Unified, &view);
        assert!(!object_list.channel_mode);
        assert_eq!(object_list.channel_display, ChannelDisplay::Unified);
        assert_eq!(object_list.selected_channel, None);
    }

    #[test]
    fn cycle_channel_view_advances_through_view_cycle_and_wraps() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );
        let cycle = [
            ChannelView::Unified,
            ChannelView::Linked,
            ChannelView::Channels,
        ];

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.down(&view);
        assert_eq!(object_list.channel_view(), ChannelView::Unified);

        object_list.cycle_channel_view(&cycle, &view);
        assert_eq!(object_list.channel_view(), ChannelView::Linked);

        object_list.cycle_channel_view(&cycle, &view);
        assert_eq!(object_list.channel_view(), ChannelView::Channels);

        // Wraps back to the first entry after the last.
        object_list.cycle_channel_view(&cycle, &view);
        assert_eq!(object_list.channel_view(), ChannelView::Unified);
    }

    #[test]
    fn cycle_channel_view_lands_on_first_entry_when_current_view_is_excluded() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );
        // Unified excluded from the cycle - reachable via select_view, but
        // cycling from it should land on the cycle's first entry rather
        // than trying to compute a "next" relative to a view that isn't
        // part of the cycle at all.
        let cycle = [ChannelView::Linked, ChannelView::Channels];

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        object_list.down(&view);
        assert_eq!(object_list.channel_view(), ChannelView::Unified);

        object_list.cycle_channel_view(&cycle, &view);
        assert_eq!(object_list.channel_view(), ChannelView::Linked);
    }

    #[test]
    fn channel_mode_accounts_for_taller_nodes_in_visible_count() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        // 1 header line + 4 channel rows = 5 lines tall in channel mode,
        // vs. the ordinary fixed height of 3.
        let object_id = ObjectId::from_raw_id(1);
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Multichannel node"));
        props.set_media_class(String::from("Stream/Output/Audio"));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from("multi"));
        props.set_object_serial(1);
        state.update(StateEvent::NodeProperties { object_id, props });
        state.update(StateEvent::NodeVolumes {
            object_id,
            volumes: vec![0.0, 0.0, 0.0, 0.0],
        });
        state.update(StateEvent::NodeMute {
            object_id,
            mute: false,
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let spacing = NodeWidget::spacing();
        // Room for exactly one node at the default height, but not once it
        // expands to 5 lines tall in channel mode.
        let full_default_height = NodeWidget::height().saturating_add(spacing);
        // + 2 for header and footer
        let rect = Rect::new(0, 0, 80, full_default_height + 2);

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        assert_eq!(object_list.visible_count(&view, &rect), 1);

        object_list.channel_mode = true;
        assert_eq!(object_list.visible_count(&view, &rect), 0);
    }

    #[test]
    fn channel_mode_shrinks_visible_objects_for_lazy_capture() {
        // lazy_capture (App::update_capturing) starts/stops peak-level
        // capture based on ObjectList::visible_objects() - this proves
        // that set correctly shrinks when channel mode makes a node too
        // tall to fit, exactly mirroring visible_count() above but at the
        // API lazy_capture actually consumes. Capture is inherently
        // per-node (ObjectId), never per-channel-row, so no new plumbing
        // is needed here - this is a regression guard confirming that
        // stays true, not new production behavior.
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        let object_id = ObjectId::from_raw_id(1);
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Multichannel node"));
        props.set_media_class(String::from("Stream/Output/Audio"));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from("multi"));
        props.set_object_serial(1);
        state.update(StateEvent::NodeProperties { object_id, props });
        state.update(StateEvent::NodeVolumes {
            object_id,
            volumes: vec![0.0, 0.0, 0.0, 0.0],
        });
        state.update(StateEvent::NodeMute {
            object_id,
            mute: false,
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let full_default_height =
            NodeWidget::height().saturating_add(NodeWidget::spacing());
        let rect = Rect::new(0, 0, 80, full_default_height + 2);

        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);
        let visible = object_list.visible_objects(&rect, &view);
        assert!(visible.contains(&object_id));

        object_list.channel_mode = true;
        let visible = object_list.visible_objects(&rect, &view);
        assert!(!visible.contains(&object_id));
    }

    #[test]
    fn visible_objects_changes_with_scroll() {
        let (state, wirehose) = init();
        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        // 3 nodes + 2 lines for header and footer
        let rect = Rect::new(0, 0, 80, height * 3 + 2);
        let mut object_list =
            ObjectList::new(ListKind::Node(NodeKind::All), None);

        // Start at top
        let visible = object_list.visible_objects(&rect, &view);
        assert_eq!(visible.len(), 4);
        assert!(visible.contains(&ObjectId::from_raw_id(0)));
        assert!(visible.contains(&ObjectId::from_raw_id(1)));
        assert!(visible.contains(&ObjectId::from_raw_id(2)));
        assert!(visible.contains(&ObjectId::from_raw_id(3)));

        // Scroll down
        object_list.top = 5;
        let visible = object_list.visible_objects(&rect, &view);
        assert_eq!(visible.len(), 4);
        assert!(visible.contains(&ObjectId::from_raw_id(0)));
        assert!(visible.contains(&ObjectId::from_raw_id(6)));
        assert!(visible.contains(&ObjectId::from_raw_id(7)));
        assert!(visible.contains(&ObjectId::from_raw_id(8)));

        // Scroll up
        object_list.top = 4;
        let visible = object_list.visible_objects(&rect, &view);
        assert_eq!(visible.len(), 4);
        assert!(visible.contains(&ObjectId::from_raw_id(0)));
        assert!(visible.contains(&ObjectId::from_raw_id(5)));
        assert!(visible.contains(&ObjectId::from_raw_id(6)));
        assert!(visible.contains(&ObjectId::from_raw_id(7)));
    }

    #[test]
    fn visible_objects_includes_linked_clients() {
        let (mut state, wirehose) = init();

        // Set client_id on node 1
        let mut props = state
            .nodes
            .get(&ObjectId::from_raw_id(1))
            .unwrap()
            .props
            .clone();
        props.set_client_id(ObjectId::from_raw_id(101));
        state.update(StateEvent::NodeProperties {
            object_id: ObjectId::from_raw_id(1),
            props,
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        // 1 node + 2 lines for header and footer
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list = ObjectList::new(ListKind::Node(NodeKind::All), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert_eq!(visible.len(), 3);
        assert!(visible.contains(&ObjectId::from_raw_id(0)));
        assert!(visible.contains(&ObjectId::from_raw_id(1)));
        assert!(visible.contains(&ObjectId::from_raw_id(101)));
    }

    #[test]
    fn visible_objects_includes_linked_devices() {
        let (mut state, wirehose) = init();

        // Set device_id on node 1
        let mut props = state
            .nodes
            .get(&ObjectId::from_raw_id(1))
            .unwrap()
            .props
            .clone();
        props.set_device_id(ObjectId::from_raw_id(101));
        let card_profile_device = 0;
        props.set_card_profile_device(card_profile_device);
        state.update(StateEvent::NodeProperties {
            object_id: ObjectId::from_raw_id(1),
            props,
        });

        // Create a test device with everything needed to populate device_info
        // on the node in the view.
        state.update(StateEvent::DeviceProperties {
            object_id: ObjectId::from_raw_id(101),
            props: PropertyStore::default(),
        });
        state.update(StateEvent::DeviceProfile {
            object_id: ObjectId::from_raw_id(101),
            index: 1,
        });
        state.update(StateEvent::DeviceRoute {
            object_id: ObjectId::from_raw_id(101),
            index: 0,
            device: card_profile_device,
            profiles: vec![1],
            description: String::new(),
            available: true,
            channel_volumes: vec![1.0],
            mute: false,
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        // 1 node + 2 lines for header and footer
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list = ObjectList::new(ListKind::Node(NodeKind::All), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert_eq!(visible.len(), 3);
        assert!(visible.contains(&ObjectId::from_raw_id(0)));
        assert!(visible.contains(&ObjectId::from_raw_id(1)));
        assert!(visible.contains(&ObjectId::from_raw_id(101)));
    }

    #[test]
    fn visible_objects_includes_target() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        // Create a playback stream (sink input)
        let stream_id = ObjectId::from_raw_id(0);
        create_node(&mut state, stream_id, "Stream/Output/Audio", "stream");

        // Create a sink as the target
        let sink_id = ObjectId::from_raw_id(100);
        create_node(&mut state, sink_id, "Audio/Sink", "sink");

        // Create a link from stream to sink
        state.update(StateEvent::Link {
            object_id: ObjectId::from_raw_id(200),
            output_id: stream_id,
            input_id: sink_id,
        });

        // Set up metadata
        let metadata_id = ObjectId::from_raw_id(300);
        state.update(StateEvent::MetadataMetadataName {
            object_id: metadata_id,
            metadata_name: String::from("default"),
        });
        state.update(StateEvent::MetadataProperty {
            object_id: metadata_id,
            subject: u32::from(stream_id),
            key: Some(String::from("target.node")),
            value: Some(String::from("100")),
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list =
            ObjectList::new(ListKind::Node(NodeKind::Playback), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert!(visible.contains(&stream_id));
        assert!(visible.contains(&sink_id));
    }

    #[test]
    fn visible_objects_includes_target_client() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        // Create a playback stream
        let stream_id = ObjectId::from_raw_id(0);
        create_node(&mut state, stream_id, "Stream/Output/Audio", "stream");

        // Create a sink with a client_id
        let sink_id = ObjectId::from_raw_id(100);
        let sink_client_id = ObjectId::from_raw_id(101);
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Test sink"));
        props.set_media_class(String::from("Audio/Sink"));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from("sink"));
        props.set_object_serial(100);
        props.set_client_id(sink_client_id);
        state.update(StateEvent::NodeProperties {
            object_id: sink_id,
            props,
        });
        state.update(StateEvent::NodeVolumes {
            object_id: sink_id,
            volumes: vec![1.0, 1.0],
        });
        state.update(StateEvent::NodeMute {
            object_id: sink_id,
            mute: false,
        });

        // Create a link from stream to sink
        state.update(StateEvent::Link {
            object_id: ObjectId::from_raw_id(200),
            output_id: stream_id,
            input_id: sink_id,
        });

        // Set up metadata
        let metadata_id = ObjectId::from_raw_id(300);
        state.update(StateEvent::MetadataMetadataName {
            object_id: metadata_id,
            metadata_name: String::from("default"),
        });
        state.update(StateEvent::MetadataProperty {
            object_id: metadata_id,
            subject: u32::from(stream_id),
            key: Some(String::from("target.node")),
            value: Some(String::from("100")),
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list =
            ObjectList::new(ListKind::Node(NodeKind::Playback), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert!(visible.contains(&stream_id));
        assert!(visible.contains(&sink_id));
        assert!(visible.contains(&sink_client_id));
    }

    #[test]
    fn visible_objects_includes_target_device() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        // Create a playback stream
        let stream_id = ObjectId::from_raw_id(0);
        create_node(&mut state, stream_id, "Stream/Output/Audio", "stream");

        // Create a sink with device_info
        let sink_id = ObjectId::from_raw_id(100);
        let sink_device_id = ObjectId::from_raw_id(101);
        let card_profile_device = 0;
        let mut props = PropertyStore::default();
        props.set_node_description(String::from("Test sink"));
        props.set_media_class(String::from("Audio/Sink"));
        props.set_media_name(String::from("Media name"));
        props.set_node_name(String::from("sink"));
        props.set_object_serial(100);
        props.set_device_id(sink_device_id);
        props.set_card_profile_device(card_profile_device);
        state.update(StateEvent::NodeProperties {
            object_id: sink_id,
            props,
        });
        state.update(StateEvent::NodeVolumes {
            object_id: sink_id,
            volumes: vec![1.0, 1.0],
        });
        state.update(StateEvent::NodeMute {
            object_id: sink_id,
            mute: false,
        });

        // Create the device with route
        state.update(StateEvent::DeviceProperties {
            object_id: sink_device_id,
            props: PropertyStore::default(),
        });
        state.update(StateEvent::DeviceProfile {
            object_id: sink_device_id,
            index: 1,
        });
        state.update(StateEvent::DeviceRoute {
            object_id: sink_device_id,
            index: 0,
            device: card_profile_device,
            profiles: vec![1],
            description: String::new(),
            available: true,
            channel_volumes: vec![1.0],
            mute: false,
        });

        // Create a link from stream to sink
        state.update(StateEvent::Link {
            object_id: ObjectId::from_raw_id(200),
            output_id: stream_id,
            input_id: sink_id,
        });

        // Set up metadata
        let metadata_id = ObjectId::from_raw_id(300);
        state.update(StateEvent::MetadataMetadataName {
            object_id: metadata_id,
            metadata_name: String::from("default"),
        });
        state.update(StateEvent::MetadataProperty {
            object_id: metadata_id,
            subject: u32::from(stream_id),
            key: Some(String::from("target.node")),
            value: Some(String::from("100")),
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        let height = NodeWidget::height() + NodeWidget::spacing();
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list =
            ObjectList::new(ListKind::Node(NodeKind::Playback), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert!(visible.contains(&stream_id));
        assert!(visible.contains(&sink_id));
        assert!(visible.contains(&sink_device_id));
    }

    #[test]
    fn visible_objects_includes_default_sink() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        // Create a playback stream (no explicit link - will use default)
        let stream_id = ObjectId::from_raw_id(0);
        create_node(&mut state, stream_id, "Stream/Output/Audio", "stream");

        // Create a sink
        let sink_id = ObjectId::from_raw_id(100);
        create_node(&mut state, sink_id, "Audio/Sink", "default_sink");

        // Set up metadata for the default sink
        let metadata_id = ObjectId::from_raw_id(300);
        state.update(StateEvent::MetadataMetadataName {
            object_id: metadata_id,
            metadata_name: String::from("default"),
        });
        state.update(StateEvent::MetadataProperty {
            object_id: metadata_id,
            subject: 0,
            key: Some(String::from("default.audio.sink")),
            value: Some(String::from("{\"name\":\"default_sink\"}")),
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        assert!(view.default_sink.is_some());

        let height = NodeWidget::height() + NodeWidget::spacing();
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list =
            ObjectList::new(ListKind::Node(NodeKind::Playback), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert!(visible.contains(&stream_id));
        assert!(visible.contains(&sink_id));
    }

    #[test]
    fn visible_objects_includes_default_source() {
        let mut state = State::default();
        let wirehose = mock::WirehoseHandle::default();

        // Create a recording stream (no explicit link - will use default)
        let stream_id = ObjectId::from_raw_id(0);
        create_node(&mut state, stream_id, "Stream/Input/Audio", "stream");

        // Create a source
        let source_id = ObjectId::from_raw_id(100);
        create_node(&mut state, source_id, "Audio/Source", "default_source");

        // Set up metadata for the default source
        let metadata_id = ObjectId::from_raw_id(300);
        state.update(StateEvent::MetadataMetadataName {
            object_id: metadata_id,
            metadata_name: String::from("default"),
        });
        state.update(StateEvent::MetadataProperty {
            object_id: metadata_id,
            subject: 0,
            key: Some(String::from("default.audio.source")),
            value: Some(String::from("{\"name\":\"default_source\"}")),
        });

        let view = View::from(
            &wirehose,
            &state,
            &config::Names::default(),
            &Vec::new(),
        );

        assert!(view.default_source.is_some());

        let height = NodeWidget::height() + NodeWidget::spacing();
        let rect = Rect::new(0, 0, 80, height + 2);
        let object_list =
            ObjectList::new(ListKind::Node(NodeKind::Recording), None);

        let visible = object_list.visible_objects(&rect, &view);
        assert!(visible.contains(&stream_id));
        assert!(visible.contains(&source_id));
    }
}
