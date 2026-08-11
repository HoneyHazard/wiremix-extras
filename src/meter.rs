//! Peak level meter rendering.

use ratatui::{
    prelude::{Alignment, Buffer, Constraint, Direction, Layout, Rect, Widget},
    text::{Line, Span},
};

use crate::config::Config;

fn render_peak(peak: f32, area: Rect) -> (usize, usize, usize) {
    fn normalize(value: f32) -> f32 {
        let amplitude = 10.0_f32.powf(value / 60.0);
        let min = 10.0_f32.powf(-60.0 / 60.0);
        let max = 10.0_f32.powf(6.0 / 60.0);

        (amplitude - min) / (max - min)
    }

    // Convert to dB between -20 and +3
    let db = 20.0 * (peak + 1e-10).log10();
    let vu_value = db.clamp(-60.0, 6.0);

    let meter = normalize(vu_value);

    let total_chars = area.width as usize;
    let lit = ((meter * total_chars as f32).round() as usize).min(total_chars);

    // Values above 0.0 will be colored differently
    let zero_char = (normalize(0.0) * total_chars as f32).round() as usize;

    // Assign colors
    let active_size = lit.min(zero_char);
    let overload_size = lit.saturating_sub(zero_char);
    let inactive_size = total_chars
        .saturating_sub(active_size)
        .saturating_sub(overload_size);

    (active_size, overload_size, inactive_size)
}

pub fn render_stereo(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
) {
    render_stereo_with_chars(
        meter_area,
        buf,
        peaks,
        config,
        &config.char_set.meter_left_active,
        &config.char_set.meter_left_overload,
        &config.char_set.meter_left_inactive,
        &config.char_set.meter_right_active,
        &config.char_set.meter_right_overload,
        &config.char_set.meter_right_inactive,
    );
}

/// Same as [`render_stereo`], but for a detected pair's own row within a
/// split display (`Stacked`, when `split_style = "radiating"`) - uses
/// `meter_channel_*` for both sides instead of `meter_left_*`/
/// `meter_right_*`, so a paired row's gauge reads as visually
/// consistent with its unpaired siblings in the same block (both use
/// the "channel" glyph), rather than looking like an unrelated
/// whole-node meter that happens to share the block.
pub fn render_stereo_channel(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
) {
    render_stereo_with_chars(
        meter_area,
        buf,
        peaks,
        config,
        &config.char_set.meter_channel_active,
        &config.char_set.meter_channel_overload,
        &config.char_set.meter_channel_inactive,
        &config.char_set.meter_channel_active,
        &config.char_set.meter_channel_overload,
        &config.char_set.meter_channel_inactive,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_stereo_with_chars(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
    left_active: &str,
    left_overload: &str,
    left_inactive: &str,
    right_active: &str,
    right_overload: &str,
    right_inactive: &str,
) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(2),   // meter_left
            Constraint::Length(2), // meter_live
            Constraint::Fill(2),   // meter_right
        ])
        .spacing(1)
        .split(meter_area);
    let meter_left = layout[0];
    let meter_live = layout[1];
    let meter_right = layout[2];

    let (left_peak, right_peak) = peaks.unwrap_or_default();

    let area = meter_left;
    let (active_peak, overload_peak, inactive_peak) =
        render_peak(left_peak, area);
    Line::from(vec![
        Span::styled(
            left_inactive.repeat(inactive_peak),
            config.theme.meter_inactive,
        ),
        Span::styled(
            left_overload.repeat(overload_peak),
            config.theme.meter_overload,
        ),
        Span::styled(
            left_active.repeat(active_peak),
            config.theme.meter_active,
        ),
    ])
    .alignment(Alignment::Right)
    .render(area, buf);

    let area = meter_right;
    let (active_peak, overload_peak, inactive_peak) =
        render_peak(right_peak, area);
    Line::from(vec![
        Span::styled(
            right_active.repeat(active_peak),
            config.theme.meter_active,
        ),
        Span::styled(
            right_overload.repeat(overload_peak),
            config.theme.meter_overload,
        ),
        Span::styled(
            right_inactive.repeat(inactive_peak),
            config.theme.meter_inactive,
        ),
    ])
    .render(area, buf);

    let live_line = if peaks.is_some() {
        Line::from(Span::styled(
            format!(
                "{}{}",
                config.char_set.meter_center_left_active,
                config.char_set.meter_center_right_active,
            ),
            config.theme.meter_center_active,
        ))
    } else {
        Line::from(Span::styled(
            format!(
                "{}{}",
                config.char_set.meter_center_left_inactive,
                config.char_set.meter_center_right_inactive
            ),
            config.theme.meter_center_inactive,
        ))
    };
    live_line.render(meter_live, buf);
}

pub fn render_mono(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
) {
    render_mono_with_chars(
        meter_area,
        buf,
        peak,
        config,
        &config.char_set.meter_right_active,
        &config.char_set.meter_right_overload,
        &config.char_set.meter_right_inactive,
    );
}

/// Same as [`render_mono`], but for a single channel's own row within a
/// split display (Channel mode, or any other multi-row layout) - uses
/// `meter_channel_*` instead of `meter_right_*` so several of these
/// stacked directly above one another read as distinct gauges rather
/// than blending into one solid bar.
pub fn render_mono_channel(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
) {
    render_mono_with_chars(
        meter_area,
        buf,
        peak,
        config,
        &config.char_set.meter_channel_active,
        &config.char_set.meter_channel_overload,
        &config.char_set.meter_channel_inactive,
    );
}

fn render_mono_with_chars(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
    active_char: &str,
    overload_char: &str,
    inactive_char: &str,
) {
    let mono_peak = peak.unwrap_or_default();

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1), // meter_live
            Constraint::Fill(2),   // meter_mono
        ])
        .spacing(1)
        .split(meter_area);
    let meter_live = layout[0];
    let meter_mono = layout[1];

    let area = meter_mono;
    let (active_peak, overload_peak, inactive_peak) =
        render_peak(mono_peak, area);
    Line::from(vec![
        Span::styled(
            active_char.repeat(active_peak),
            config.theme.meter_active,
        ),
        Span::styled(
            overload_char.repeat(overload_peak),
            config.theme.meter_overload,
        ),
        Span::styled(
            inactive_char.repeat(inactive_peak),
            config.theme.meter_inactive,
        ),
    ])
    .render(area, buf);

    let live_line = if peak.is_some() {
        Line::from(Span::styled(
            &config.char_set.meter_center_right_active,
            config.theme.meter_center_active,
        ))
    } else {
        Line::from(Span::styled(
            &config.char_set.meter_center_right_inactive,
            config.theme.meter_center_inactive,
        ))
    };
    live_line.render(meter_live, buf);
}
