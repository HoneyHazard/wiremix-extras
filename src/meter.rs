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

/// One side's (or mono gauge's) inactive/active/overload glyphs, resolved
/// to whichever concrete characters should actually be drawn - either the
/// stock `meter_left`/`meter_right` ones, or (for a channel-row gauge) a
/// `meter_channel_*` override falling back to the stock glyph wherever the
/// override isn't set.
struct MeterChars<'a> {
    inactive: &'a str,
    active: &'a str,
    overload: &'a str,
}

fn render_side(
    area: Rect,
    buf: &mut Buffer,
    peak: f32,
    chars: &MeterChars,
    theme: &crate::config::Theme,
    alignment: Alignment,
) {
    let (active_peak, overload_peak, inactive_peak) = render_peak(peak, area);

    let inactive = Span::styled(
        chars.inactive.repeat(inactive_peak),
        theme.meter_inactive,
    );
    let overload = Span::styled(
        chars.overload.repeat(overload_peak),
        theme.meter_overload,
    );
    let active =
        Span::styled(chars.active.repeat(active_peak), theme.meter_active);

    let spans = match alignment {
        // Left side: filled portion sits adjacent to the center marker (the
        // bar's own right edge) and grows outward, away from center.
        Alignment::Right => vec![inactive, overload, active],
        // Right side (and mono): mirror image - filled adjacent to center,
        // growing outward to the right.
        _ => vec![active, overload, inactive],
    };

    Line::from(spans).alignment(alignment).render(area, buf);
}

/// The two-char "live" indicator between a stereo gauge's left and right
/// halves.
struct CenterChars<'a> {
    left_inactive: &'a str,
    left_active: &'a str,
    right_inactive: &'a str,
    right_active: &'a str,
}

fn render_stereo_core(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
    left: MeterChars,
    right: MeterChars,
    center: CenterChars,
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

    render_side(
        meter_left,
        buf,
        left_peak,
        &left,
        &config.theme,
        Alignment::Right,
    );
    render_side(
        meter_right,
        buf,
        right_peak,
        &right,
        &config.theme,
        Alignment::Left,
    );

    let live_line = if peaks.is_some() {
        Line::from(Span::styled(
            format!("{}{}", center.left_active, center.right_active),
            config.theme.meter_center_active,
        ))
    } else {
        Line::from(Span::styled(
            format!("{}{}", center.left_inactive, center.right_inactive),
            config.theme.meter_center_inactive,
        ))
    };
    live_line.render(meter_live, buf);
}

fn render_mono_core(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
    mono: MeterChars,
    center_inactive: &str,
    center_active: &str,
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

    render_side(
        meter_mono,
        buf,
        mono_peak,
        &mono,
        &config.theme,
        Alignment::Left,
    );

    let live_line = if peak.is_some() {
        Line::from(Span::styled(
            center_active,
            config.theme.meter_center_active,
        ))
    } else {
        Line::from(Span::styled(
            center_inactive,
            config.theme.meter_center_inactive,
        ))
    };
    live_line.render(meter_live, buf);
}

pub fn render_stereo(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
) {
    let cs = &config.char_set;
    render_stereo_core(
        meter_area,
        buf,
        peaks,
        config,
        MeterChars {
            inactive: &cs.meter_left_inactive,
            active: &cs.meter_left_active,
            overload: &cs.meter_left_overload,
        },
        MeterChars {
            inactive: &cs.meter_right_inactive,
            active: &cs.meter_right_active,
            overload: &cs.meter_right_overload,
        },
        CenterChars {
            left_inactive: &cs.meter_center_left_inactive,
            left_active: &cs.meter_center_left_active,
            right_inactive: &cs.meter_center_right_inactive,
            right_active: &cs.meter_center_right_active,
        },
    );
}

pub fn render_mono(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
) {
    let cs = &config.char_set;
    render_mono_core(
        meter_area,
        buf,
        peak,
        config,
        MeterChars {
            inactive: &cs.meter_right_inactive,
            active: &cs.meter_right_active,
            overload: &cs.meter_right_overload,
        },
        &cs.meter_center_right_inactive,
        &cs.meter_center_right_active,
    );
}

/// Same as [`render_stereo`], but for a single channel row within a split
/// block (`ChannelRowWidget`/`RadiatingRowWidget`) - each `meter_channel_*`
/// glyph is used if the active char_set sets it, otherwise falls back to
/// the same stock glyph [`render_stereo`] itself would use, so a theme that
/// hasn't opted in to a distinct split-row look renders identically to a
/// whole node's own meter.
pub fn render_channel_stereo(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
) {
    let cs = &config.char_set;
    render_stereo_core(
        meter_area,
        buf,
        peaks,
        config,
        MeterChars {
            inactive: cs
                .meter_channel_left_inactive
                .as_deref()
                .unwrap_or(&cs.meter_left_inactive),
            active: cs
                .meter_channel_left_active
                .as_deref()
                .unwrap_or(&cs.meter_left_active),
            overload: cs
                .meter_channel_left_overload
                .as_deref()
                .unwrap_or(&cs.meter_left_overload),
        },
        MeterChars {
            inactive: cs
                .meter_channel_right_inactive
                .as_deref()
                .unwrap_or(&cs.meter_right_inactive),
            active: cs
                .meter_channel_right_active
                .as_deref()
                .unwrap_or(&cs.meter_right_active),
            overload: cs
                .meter_channel_right_overload
                .as_deref()
                .unwrap_or(&cs.meter_right_overload),
        },
        CenterChars {
            left_inactive: cs
                .meter_channel_center_left_inactive
                .as_deref()
                .unwrap_or(&cs.meter_center_left_inactive),
            left_active: cs
                .meter_channel_center_left_active
                .as_deref()
                .unwrap_or(&cs.meter_center_left_active),
            right_inactive: cs
                .meter_channel_center_right_inactive
                .as_deref()
                .unwrap_or(&cs.meter_center_right_inactive),
            right_active: cs
                .meter_channel_center_right_active
                .as_deref()
                .unwrap_or(&cs.meter_center_right_active),
        },
    );
}

/// Same as [`render_mono`], but for a single channel row within a split
/// block - see [`render_channel_stereo`] for the fallback rule.
pub fn render_channel_mono(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
) {
    let cs = &config.char_set;
    render_mono_core(
        meter_area,
        buf,
        peak,
        config,
        MeterChars {
            inactive: cs
                .meter_channel_right_inactive
                .as_deref()
                .unwrap_or(&cs.meter_right_inactive),
            active: cs
                .meter_channel_right_active
                .as_deref()
                .unwrap_or(&cs.meter_right_active),
            overload: cs
                .meter_channel_right_overload
                .as_deref()
                .unwrap_or(&cs.meter_right_overload),
        },
        cs.meter_channel_center_right_inactive
            .as_deref()
            .unwrap_or(&cs.meter_center_right_inactive),
        cs.meter_channel_center_right_active
            .as_deref()
            .unwrap_or(&cs.meter_center_right_active),
    );
}
