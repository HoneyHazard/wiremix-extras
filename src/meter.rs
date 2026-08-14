//! Peak level meter rendering.

use ratatui::{
    prelude::{Alignment, Buffer, Constraint, Direction, Layout, Rect, Widget},
    text::{Line, Span},
};

use crate::config::Config;

/// Sizes (in characters) of each zone of a rendered peak meter, from the
/// live/center marker outward: `active` and `overload` are lit (the
/// current peak has reached them); `inactive` and `inactive_overload` are
/// not (the peak hasn't reached them yet), but `inactive_overload` still
/// falls past the same 0dB boundary `overload` does - a permanent preview
/// of where the overload zone sits on the scale, the way a physical VU
/// meter marks its red zone whether or not the needle is currently there.
/// This split always happens - there's no separate on/off switch for it.
/// What makes it visually inert by default is that `meter_left/right_
/// inactive_overload` (char_set) and `meter_inactive_overload` (theme)
/// default to the exact same glyph/color as their plain `inactive`
/// counterparts, so classic wiremix's single flat unlit color is what you
/// get until you actually write a different value for one of those keys
/// into your own config - at which point exactly that zone's preview
/// changes, and nothing else does.
struct PeakSizes {
    active: usize,
    overload: usize,
    inactive: usize,
    inactive_overload: usize,
}

fn render_peak(peak: f32, area: Rect) -> PeakSizes {
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
    let active = lit.min(zero_char);
    let overload = lit.saturating_sub(zero_char);
    let inactive = total_chars.saturating_sub(active).saturating_sub(overload);

    // Of the not-yet-lit remainder, however much of it falls past the same
    // 0dB boundary the lit `overload` zone uses is `inactive_overload`
    // instead of plain `inactive`.
    let inactive_overload =
        inactive.saturating_sub(zero_char.saturating_sub(lit));
    let inactive = inactive - inactive_overload;

    PeakSizes {
        active,
        overload,
        inactive,
        inactive_overload,
    }
}

pub fn render_stereo(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
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
    let sizes = render_peak(left_peak, area);
    Line::from(vec![
        Span::styled(
            config
                .char_set
                .meter_left_inactive_overload
                .repeat(sizes.inactive_overload),
            config.theme.meter_inactive_overload,
        ),
        Span::styled(
            config.char_set.meter_left_inactive.repeat(sizes.inactive),
            config.theme.meter_inactive,
        ),
        Span::styled(
            config.char_set.meter_left_overload.repeat(sizes.overload),
            config.theme.meter_overload,
        ),
        Span::styled(
            config.char_set.meter_left_active.repeat(sizes.active),
            config.theme.meter_active,
        ),
    ])
    .alignment(Alignment::Right)
    .render(area, buf);

    let area = meter_right;
    let sizes = render_peak(right_peak, area);
    Line::from(vec![
        Span::styled(
            config.char_set.meter_right_active.repeat(sizes.active),
            config.theme.meter_active,
        ),
        Span::styled(
            config.char_set.meter_right_overload.repeat(sizes.overload),
            config.theme.meter_overload,
        ),
        Span::styled(
            config.char_set.meter_right_inactive.repeat(sizes.inactive),
            config.theme.meter_inactive,
        ),
        Span::styled(
            config
                .char_set
                .meter_right_inactive_overload
                .repeat(sizes.inactive_overload),
            config.theme.meter_inactive_overload,
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
    let sizes = render_peak(mono_peak, area);
    Line::from(vec![
        Span::styled(
            config.char_set.meter_right_active.repeat(sizes.active),
            config.theme.meter_active,
        ),
        Span::styled(
            config.char_set.meter_right_overload.repeat(sizes.overload),
            config.theme.meter_overload,
        ),
        Span::styled(
            config.char_set.meter_right_inactive.repeat(sizes.inactive),
            config.theme.meter_inactive,
        ),
        Span::styled(
            config
                .char_set
                .meter_right_inactive_overload
                .repeat(sizes.inactive_overload),
            config.theme.meter_inactive_overload,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16) -> Rect {
        Rect::new(0, 0, width, 1)
    }

    const PEAKS: [f32; 6] = [0.0, 0.1, 0.5, 1.0, 2.0, 10.0];

    #[test]
    fn sizes_always_sum_to_total_width() {
        for peak in PEAKS {
            let sizes = render_peak(peak, area(40));
            assert_eq!(
                sizes.active
                    + sizes.overload
                    + sizes.inactive
                    + sizes.inactive_overload,
                40,
                "peak={peak}"
            );
        }
    }

    #[test]
    fn silence_previews_the_entire_overload_zone_as_unlit() {
        // At silence (nothing lit), the classic overload zone itself must
        // be empty, and its whole width should preview as
        // inactive_overload instead.
        let sizes = render_peak(0.0, area(40));
        assert_eq!(sizes.active, 0);
        assert_eq!(sizes.overload, 0);
        assert!(sizes.inactive_overload > 0);
    }

    #[test]
    fn deep_overload_leaves_nothing_left_to_preview() {
        // A peak far above 0dB lights up the whole overload zone - there's
        // nothing left unlit to preview.
        let sizes = render_peak(100.0, area(40));
        assert_eq!(sizes.inactive, 0);
        assert_eq!(sizes.inactive_overload, 0);
    }

    #[test]
    fn inactive_and_inactive_overload_are_a_clean_partition_of_the_unlit_remainder(
    ) {
        // Every character not lit by active/overload must land in exactly
        // one of inactive/inactive_overload - this is always computed, not
        // gated behind any option.
        for peak in PEAKS {
            let sizes = render_peak(peak, area(40));
            let unlit = 40 - sizes.active - sizes.overload;
            assert_eq!(
                sizes.inactive + sizes.inactive_overload,
                unlit,
                "peak={peak}"
            );
        }
    }
}
