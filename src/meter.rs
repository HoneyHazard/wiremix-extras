//! Peak level meter rendering.

use ratatui::{
    prelude::{Alignment, Buffer, Constraint, Direction, Layout, Rect, Widget},
    style::Style,
    text::{Line, Span},
};

use crate::config::{ChannelView, Config};

/// Sizes (in characters) of each zone of a rendered peak meter, from the
/// live/center marker outward: `active` and `overload` are lit (the
/// current peak has reached them); `inactive` and `inactive_overload` are
/// not (the peak hasn't reached them yet), but `inactive_overload` still
/// falls past the same 0dB boundary `overload` does - a permanent preview
/// of where the overload zone sits on the scale, the way a physical VU
/// meter marks its red zone whether or not the needle is currently there.
/// This split always happens - there's no separate on/off switch for it.
/// `meter_left/right_inactive_overload` (char_set) and
/// `meter_inactive_overload` (theme) are ordinary fields like any other
/// `meter_*` key, resolved the same way `meter_active`/`meter_overload`
/// already are - the "default" char_set/theme ship a real (dim red/hollow)
/// preview out of the box, but nothing stops a config from setting either
/// one back to match plain `inactive` if the flat classic look is
/// preferred instead.
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

/// One side's (or mono gauge's) inactive/active/overload glyphs, resolved
/// to whichever concrete characters should actually be drawn - either the
/// stock `meter_left`/`meter_right` ones (`Unified` view), or the
/// `meter_split_*` fields (`Linked`/`Channels` view), themselves falling
/// back to the stock glyph wherever a theme hasn't set them.
struct MeterChars<'a> {
    inactive: &'a str,
    active: &'a str,
    overload: &'a str,
    inactive_overload: &'a str,
}

/// One side's (or mono gauge's) inactive/active/overload colors, resolved
/// the same way as [`MeterChars`] - stock `meter_*` `Style`s in `Unified`
/// view, or `meter_split_*` in `Linked`/`Channels`, themselves falling
/// back to the stock `Style` wherever a theme hasn't set them.
#[derive(Clone, Copy)]
struct MeterStyle {
    inactive: Style,
    active: Style,
    overload: Style,
    inactive_overload: Style,
}

/// A side's (or mono gauge's) glyphs and colors together - bundled so
/// `render_stereo_core`/`render_mono_core` take one argument per side
/// instead of two.
struct MeterSide<'a> {
    chars: MeterChars<'a>,
    style: MeterStyle,
}

/// The two-char "live" indicator's resolved colors, active and inactive.
#[derive(Clone, Copy)]
struct CenterStyle {
    inactive: Style,
    active: Style,
}

fn render_side(
    area: Rect,
    buf: &mut Buffer,
    peak: f32,
    side: &MeterSide,
    alignment: Alignment,
) {
    let sizes = render_peak(peak, area);

    let inactive = Span::styled(
        side.chars.inactive.repeat(sizes.inactive),
        side.style.inactive,
    );
    let overload = Span::styled(
        side.chars.overload.repeat(sizes.overload),
        side.style.overload,
    );
    let active =
        Span::styled(side.chars.active.repeat(sizes.active), side.style.active);
    let inactive_overload = Span::styled(
        side.chars.inactive_overload.repeat(sizes.inactive_overload),
        side.style.inactive_overload,
    );

    let spans = match alignment {
        // Left side: filled portion sits adjacent to the center marker (the
        // bar's own right edge) and grows outward, away from center. The
        // overload-zone preview sits at the far/outer edge, like a
        // physical VU meter's permanent red-zone marking.
        Alignment::Right => vec![inactive_overload, inactive, overload, active],
        // Right side (and mono): mirror image - filled adjacent to center,
        // growing outward to the right, overload preview at the far edge.
        _ => vec![active, overload, inactive, inactive_overload],
    };

    Line::from(spans).alignment(alignment).render(area, buf);
}

/// The two-char "live" indicator between a stereo gauge's left and right
/// halves, and its resolved colors.
struct StereoCenter<'a> {
    left_inactive: &'a str,
    left_active: &'a str,
    right_inactive: &'a str,
    right_active: &'a str,
    style: CenterStyle,
}

fn render_stereo_core(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    left: MeterSide,
    right: MeterSide,
    center: StereoCenter,
) {
    // Two separate Fill(2) constraints can end up with unequal widths when
    // the remaining space is odd (Ratatui's Fill distribution isn't
    // guaranteed symmetric) - identical L/R peaks would then render as a
    // visibly different number of characters just from that width
    // difference. Computing one shared bar_width and giving both sides the
    // same explicit Length(bar_width) makes that impossible by
    // construction; any odd leftover column is simply unused rather than
    // handed to one side. Same fix as StereoVolumeWidget's own bar_width
    // above.
    let center_width = 2;
    let spacing_width = 2; // 2 gaps between the 3 segments, at 1 each
    let bar_width = meter_area
        .width
        .saturating_sub(center_width + spacing_width)
        / 2;

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(bar_width),    // meter_left
            Constraint::Length(center_width), // meter_live
            Constraint::Length(bar_width),    // meter_right
        ])
        .spacing(1)
        .split(meter_area);
    let meter_left = layout[0];
    let meter_live = layout[1];
    let meter_right = layout[2];

    let (left_peak, right_peak) = peaks.unwrap_or_default();

    render_side(meter_left, buf, left_peak, &left, Alignment::Right);
    render_side(meter_right, buf, right_peak, &right, Alignment::Left);

    let live_line = if peaks.is_some() {
        Line::from(Span::styled(
            format!("{}{}", center.left_active, center.right_active),
            center.style.active,
        ))
    } else {
        Line::from(Span::styled(
            format!("{}{}", center.left_inactive, center.right_inactive),
            center.style.inactive,
        ))
    };
    live_line.render(meter_live, buf);
}

/// The single-char "live" indicator for a mono gauge, and its resolved
/// colors.
struct MonoCenter<'a> {
    inactive: &'a str,
    active: &'a str,
    style: CenterStyle,
}

fn render_mono_core(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    mono: MeterSide,
    center: MonoCenter,
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

    render_side(meter_mono, buf, mono_peak, &mono, Alignment::Left);

    let live_line = if peak.is_some() {
        Line::from(Span::styled(center.active, center.style.active))
    } else {
        Line::from(Span::styled(center.inactive, center.style.inactive))
    };
    live_line.render(meter_live, buf);
}

/// Renders a stereo (left/right) peak meter. `view` selects which glyphs/
/// colors apply: `Unified` always uses the stock `meter_left`/
/// `meter_right`/`meter_center_*` fields; `Linked`/`Channels` use the
/// `meter_split_*` fields instead, each falling back to its stock
/// counterpart wherever a theme hasn't set it - so a theme that hasn't
/// opted in to a distinct split-view look renders identically to
/// `Unified`. The `inactive_overload` zone-preview glyph/color (see
/// [`PeakSizes`]) is an ordinary zone like active/inactive/overload, not
/// a special case - `meter_split_left/right_inactive_overload` (glyph)
/// and `meter_split_inactive_overload` (color) follow the same
/// split/fallback rule as everything else here.
pub fn render_stereo(
    meter_area: Rect,
    buf: &mut Buffer,
    peaks: Option<(f32, f32)>,
    config: &Config,
    view: ChannelView,
) {
    let cs = &config.char_set;
    let theme = &config.theme;
    let split = view != ChannelView::Unified;

    let style = if split {
        MeterStyle {
            inactive: theme
                .meter_split_inactive
                .unwrap_or(theme.meter_inactive),
            active: theme.meter_split_active.unwrap_or(theme.meter_active),
            overload: theme
                .meter_split_overload
                .unwrap_or(theme.meter_overload),
            inactive_overload: theme
                .meter_split_inactive_overload
                .unwrap_or(theme.meter_inactive_overload),
        }
    } else {
        MeterStyle {
            inactive: theme.meter_inactive,
            active: theme.meter_active,
            overload: theme.meter_overload,
            inactive_overload: theme.meter_inactive_overload,
        }
    };

    let left = if split {
        MeterChars {
            inactive: cs
                .meter_split_left_inactive
                .as_deref()
                .unwrap_or(&cs.meter_left_inactive),
            active: cs
                .meter_split_left_active
                .as_deref()
                .unwrap_or(&cs.meter_left_active),
            overload: cs
                .meter_split_left_overload
                .as_deref()
                .unwrap_or(&cs.meter_left_overload),
            inactive_overload: cs
                .meter_split_left_inactive_overload
                .as_deref()
                .unwrap_or(&cs.meter_left_inactive_overload),
        }
    } else {
        MeterChars {
            inactive: &cs.meter_left_inactive,
            active: &cs.meter_left_active,
            overload: &cs.meter_left_overload,
            inactive_overload: &cs.meter_left_inactive_overload,
        }
    };

    let right = if split {
        MeterChars {
            inactive: cs
                .meter_split_right_inactive
                .as_deref()
                .unwrap_or(&cs.meter_right_inactive),
            active: cs
                .meter_split_right_active
                .as_deref()
                .unwrap_or(&cs.meter_right_active),
            overload: cs
                .meter_split_right_overload
                .as_deref()
                .unwrap_or(&cs.meter_right_overload),
            inactive_overload: cs
                .meter_split_right_inactive_overload
                .as_deref()
                .unwrap_or(&cs.meter_right_inactive_overload),
        }
    } else {
        MeterChars {
            inactive: &cs.meter_right_inactive,
            active: &cs.meter_right_active,
            overload: &cs.meter_right_overload,
            inactive_overload: &cs.meter_right_inactive_overload,
        }
    };

    let center = if split {
        StereoCenter {
            left_inactive: cs
                .meter_split_center_left_inactive
                .as_deref()
                .unwrap_or(&cs.meter_center_left_inactive),
            left_active: cs
                .meter_split_center_left_active
                .as_deref()
                .unwrap_or(&cs.meter_center_left_active),
            right_inactive: cs
                .meter_split_center_right_inactive
                .as_deref()
                .unwrap_or(&cs.meter_center_right_inactive),
            right_active: cs
                .meter_split_center_right_active
                .as_deref()
                .unwrap_or(&cs.meter_center_right_active),
            style: CenterStyle {
                inactive: theme
                    .meter_split_center_inactive
                    .unwrap_or(theme.meter_center_inactive),
                active: theme
                    .meter_split_center_active
                    .unwrap_or(theme.meter_center_active),
            },
        }
    } else {
        StereoCenter {
            left_inactive: &cs.meter_center_left_inactive,
            left_active: &cs.meter_center_left_active,
            right_inactive: &cs.meter_center_right_inactive,
            right_active: &cs.meter_center_right_active,
            style: CenterStyle {
                inactive: theme.meter_center_inactive,
                active: theme.meter_center_active,
            },
        }
    };

    render_stereo_core(
        meter_area,
        buf,
        peaks,
        MeterSide { chars: left, style },
        MeterSide {
            chars: right,
            style,
        },
        center,
    );
}

/// Renders a mono (single-channel) peak meter - see [`render_stereo`] for
/// how `view` selects between stock and `meter_split_*` glyphs/colors.
pub fn render_mono(
    meter_area: Rect,
    buf: &mut Buffer,
    peak: Option<f32>,
    config: &Config,
    view: ChannelView,
) {
    let cs = &config.char_set;
    let theme = &config.theme;
    let split = view != ChannelView::Unified;

    let mono = if split {
        MeterSide {
            chars: MeterChars {
                inactive: cs
                    .meter_split_right_inactive
                    .as_deref()
                    .unwrap_or(&cs.meter_right_inactive),
                active: cs
                    .meter_split_right_active
                    .as_deref()
                    .unwrap_or(&cs.meter_right_active),
                overload: cs
                    .meter_split_right_overload
                    .as_deref()
                    .unwrap_or(&cs.meter_right_overload),
                inactive_overload: cs
                    .meter_split_right_inactive_overload
                    .as_deref()
                    .unwrap_or(&cs.meter_right_inactive_overload),
            },
            style: MeterStyle {
                inactive: theme
                    .meter_split_inactive
                    .unwrap_or(theme.meter_inactive),
                active: theme.meter_split_active.unwrap_or(theme.meter_active),
                overload: theme
                    .meter_split_overload
                    .unwrap_or(theme.meter_overload),
                inactive_overload: theme
                    .meter_split_inactive_overload
                    .unwrap_or(theme.meter_inactive_overload),
            },
        }
    } else {
        MeterSide {
            chars: MeterChars {
                inactive: &cs.meter_right_inactive,
                active: &cs.meter_right_active,
                overload: &cs.meter_right_overload,
                inactive_overload: &cs.meter_right_inactive_overload,
            },
            style: MeterStyle {
                inactive: theme.meter_inactive,
                active: theme.meter_active,
                overload: theme.meter_overload,
                inactive_overload: theme.meter_inactive_overload,
            },
        }
    };

    let center = if split {
        MonoCenter {
            inactive: cs
                .meter_split_center_right_inactive
                .as_deref()
                .unwrap_or(&cs.meter_center_right_inactive),
            active: cs
                .meter_split_center_right_active
                .as_deref()
                .unwrap_or(&cs.meter_center_right_active),
            style: CenterStyle {
                inactive: theme
                    .meter_split_center_inactive
                    .unwrap_or(theme.meter_center_inactive),
                active: theme
                    .meter_split_center_active
                    .unwrap_or(theme.meter_center_active),
            },
        }
    } else {
        MonoCenter {
            inactive: &cs.meter_center_right_inactive,
            active: &cs.meter_center_right_active,
            style: CenterStyle {
                inactive: theme.meter_center_inactive,
                active: theme.meter_center_active,
            },
        }
    };

    render_mono_core(meter_area, buf, peak, mono, center);
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
