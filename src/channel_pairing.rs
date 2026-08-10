//! Detects stereo (and other named left/right) pairs among a node's audio
//! channel positions, so multi-channel rendering can group channels that
//! are actually a pair and leave everything else independent.
//!
//! Pairing is based on the channel *name* (per `enum spa_audio_channel` in
//! spa/param/audio/raw.h), not proximity in the position array - PipeWire
//! doesn't guarantee adjacent pairs are ordered adjacently, and channels
//! with no left/right semantics at all (LFE, FC, the generic AUX range)
//! must never be paired with anything, since there's no protocol-level
//! signal that they're related.

use std::collections::HashSet;

/// One channel, or a left/right pair of channels, in the order they first
/// appear in the `positions` slice `group_channels()` was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelGroup {
    /// A stereo (or other named left/right) pair: `(left_index,
    /// right_index)` into the original `positions` slice.
    Pair(usize, usize),
    /// A single channel with no detected pair, by index into the original
    /// `positions` slice.
    Single(usize),
}

/// Known left/right channel name pairs, per `enum spa_audio_channel`.
/// `AUX*`/`UNKNOWN`/`NA`/`MONO` are deliberately absent - nothing in the
/// protocol says any of those are paired with anything, so they always
/// come out as `ChannelGroup::Single`.
const LR_PAIRS: &[(u32, u32)] = &[
    (
        libspa_sys::SPA_AUDIO_CHANNEL_FL,
        libspa_sys::SPA_AUDIO_CHANNEL_FR,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_SL,
        libspa_sys::SPA_AUDIO_CHANNEL_SR,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_RL,
        libspa_sys::SPA_AUDIO_CHANNEL_RR,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_FLC,
        libspa_sys::SPA_AUDIO_CHANNEL_FRC,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_TFL,
        libspa_sys::SPA_AUDIO_CHANNEL_TFR,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_TRL,
        libspa_sys::SPA_AUDIO_CHANNEL_TRR,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_RLC,
        libspa_sys::SPA_AUDIO_CHANNEL_RRC,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_FLW,
        libspa_sys::SPA_AUDIO_CHANNEL_FRW,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_FLH,
        libspa_sys::SPA_AUDIO_CHANNEL_FRH,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_TSL,
        libspa_sys::SPA_AUDIO_CHANNEL_TSR,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_LLFE,
        libspa_sys::SPA_AUDIO_CHANNEL_RLFE,
    ),
    (
        libspa_sys::SPA_AUDIO_CHANNEL_BLC,
        libspa_sys::SPA_AUDIO_CHANNEL_BRC,
    ),
];

/// A short, human-readable name for a single `enum spa_audio_channel`
/// value, for labeling individual channel rows in Channel mode display.
/// Named channels (including every `LR_PAIRS` entry) get their real
/// abbreviation straight from the enum's own doc comments in
/// spa/param/audio/raw.h (`FL`, `FR`, `LFE`, ...); the generic
/// `AUX0`..`AUX63` range gets a computed `AUX{n}` since there's no fixed
/// name to look up; anything else (`UNKNOWN`/`NA`, or a future value this
/// list hasn't caught up to) falls back to `?`.
pub fn channel_name(position: u32) -> String {
    match position {
        libspa_sys::SPA_AUDIO_CHANNEL_MONO => "MONO".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FL => "FL".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FR => "FR".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FC => "FC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_LFE => "LFE".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_SL => "SL".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_SR => "SR".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FLC => "FLC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FRC => "FRC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_RC => "RC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_RL => "RL".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_RR => "RR".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TC => "TC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TFL => "TFL".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TFC => "TFC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TFR => "TFR".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TRL => "TRL".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TRC => "TRC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TRR => "TRR".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_RLC => "RLC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_RRC => "RRC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FLW => "FLW".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FRW => "FRW".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_LFE2 => "LFE2".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FLH => "FLH".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FCH => "FCH".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_FRH => "FRH".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TFLC => "TFLC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TFRC => "TFRC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TSL => "TSL".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_TSR => "TSR".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_LLFE => "LLFE".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_RLFE => "RLFE".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_BC => "BC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_BLC => "BLC".to_string(),
        libspa_sys::SPA_AUDIO_CHANNEL_BRC => "BRC".to_string(),
        p if (libspa_sys::SPA_AUDIO_CHANNEL_START_Aux
            ..=libspa_sys::SPA_AUDIO_CHANNEL_LAST_Aux)
            .contains(&p) =>
        {
            format!("AUX{}", p - libspa_sys::SPA_AUDIO_CHANNEL_START_Aux)
        }
        _ => "?".to_string(),
    }
}

fn lr_partner(channel: u32) -> Option<(u32, bool)> {
    LR_PAIRS.iter().find_map(|&(l, r)| {
        if channel == l {
            Some((r, true))
        } else if channel == r {
            Some((l, false))
        } else {
            None
        }
    })
}

/// Groups `positions` (raw `enum spa_audio_channel` values, in the order
/// PipeWire reported them) into named left/right pairs and leftover
/// singles. A channel only pairs with its documented counterpart (see
/// `LR_PAIRS`), and only if that counterpart is also present in
/// `positions` - adjacency in the array is not required, matching
/// PipeWire's own lack of an ordering guarantee here. Every index appears
/// in exactly one output group, in the order its *first* channel of the
/// group appears in `positions`.
pub fn group_channels(positions: &[u32]) -> Vec<ChannelGroup> {
    let mut paired: HashSet<usize> = HashSet::new();
    let mut groups = Vec::with_capacity(positions.len());

    for (i, &channel) in positions.iter().enumerate() {
        if paired.contains(&i) {
            continue;
        }

        let partner =
            lr_partner(channel).and_then(|(partner_channel, is_left)| {
                positions
                    .iter()
                    .enumerate()
                    .find(|&(j, &c)| {
                        j != i && !paired.contains(&j) && c == partner_channel
                    })
                    .map(|(j, _)| (j, is_left))
            });

        match partner {
            Some((j, is_left)) => {
                paired.insert(i);
                paired.insert(j);
                if is_left {
                    groups.push(ChannelGroup::Pair(i, j));
                } else {
                    groups.push(ChannelGroup::Pair(j, i));
                }
            }
            None => groups.push(ChannelGroup::Single(i)),
        }
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    const FL: u32 = libspa_sys::SPA_AUDIO_CHANNEL_FL;
    const FR: u32 = libspa_sys::SPA_AUDIO_CHANNEL_FR;
    const FC: u32 = libspa_sys::SPA_AUDIO_CHANNEL_FC;
    const LFE: u32 = libspa_sys::SPA_AUDIO_CHANNEL_LFE;
    const LFE2: u32 = libspa_sys::SPA_AUDIO_CHANNEL_LFE2;
    const RL: u32 = libspa_sys::SPA_AUDIO_CHANNEL_RL;
    const RR: u32 = libspa_sys::SPA_AUDIO_CHANNEL_RR;
    const MONO: u32 = libspa_sys::SPA_AUDIO_CHANNEL_MONO;
    const AUX0: u32 = libspa_sys::SPA_AUDIO_CHANNEL_AUX0;
    const AUX1: u32 = libspa_sys::SPA_AUDIO_CHANNEL_AUX1;

    #[test]
    fn empty_positions_produce_no_groups() {
        assert_eq!(group_channels(&[]), vec![]);
    }

    #[test]
    fn mono_is_a_single() {
        assert_eq!(group_channels(&[MONO]), vec![ChannelGroup::Single(0)]);
    }

    #[test]
    fn simple_stereo_pair() {
        assert_eq!(group_channels(&[FL, FR]), vec![ChannelGroup::Pair(0, 1)]);
    }

    #[test]
    fn reversed_order_still_identifies_left_and_right_correctly() {
        // FR appears first in the array, but the pair's left/right indices
        // must still reflect which channel is actually FL vs FR, not
        // array order.
        assert_eq!(group_channels(&[FR, FL]), vec![ChannelGroup::Pair(1, 0)]);
    }

    #[test]
    fn real_5_1_device_pairs_fronts_and_rears_leaves_center_and_lfe_single() {
        // Real audio.position reported by hardware on this machine:
        // "M-Audio Sonica Theater Analog Surround 5.1" -> FL,FR,RL,RR,FC,LFE
        assert_eq!(
            group_channels(&[FL, FR, RL, RR, FC, LFE]),
            vec![
                ChannelGroup::Pair(0, 1),
                ChannelGroup::Pair(2, 3),
                ChannelGroup::Single(4),
                ChannelGroup::Single(5),
            ]
        );
    }

    #[test]
    fn generic_aux_channels_never_pair() {
        // Real audio.position reported by hardware on this machine:
        // "Built-in Audio Pro" -> AUX0,AUX1. Nothing in the protocol says
        // these are a stereo pair - could just as easily be two unrelated
        // mono paths.
        assert_eq!(
            group_channels(&[AUX0, AUX1]),
            vec![ChannelGroup::Single(0), ChannelGroup::Single(1)]
        );
    }

    #[test]
    fn lfe_and_lfe2_are_not_a_pair() {
        // Trap case: same stem, similar name, but both are low-frequency-
        // effects channels, not a left/right pair.
        assert_eq!(
            group_channels(&[LFE, LFE2]),
            vec![ChannelGroup::Single(0), ChannelGroup::Single(1)]
        );
    }

    #[test]
    fn unpaired_channel_missing_its_partner_stays_single() {
        // FR with no FL anywhere in positions - can't pair with nothing.
        assert_eq!(
            group_channels(&[FR, FC]),
            vec![ChannelGroup::Single(0), ChannelGroup::Single(1),]
        );
    }

    #[test]
    fn duplicate_channel_falls_back_to_single_when_no_partner_left() {
        // Two FLs, one FR: first FL claims the only FR, second FL has
        // nothing left to pair with.
        assert_eq!(
            group_channels(&[FL, FL, FR]),
            vec![ChannelGroup::Pair(0, 2), ChannelGroup::Single(1)]
        );
    }

    #[test]
    fn channel_name_covers_named_channels() {
        assert_eq!(channel_name(FL), "FL");
        assert_eq!(channel_name(FR), "FR");
        assert_eq!(channel_name(MONO), "MONO");
        assert_eq!(channel_name(FC), "FC");
        assert_eq!(channel_name(LFE), "LFE");
        assert_eq!(channel_name(LFE2), "LFE2");
        assert_eq!(channel_name(RL), "RL");
        assert_eq!(channel_name(RR), "RR");
    }

    #[test]
    fn channel_name_formats_aux_range_with_computed_offset() {
        assert_eq!(channel_name(AUX0), "AUX0");
        assert_eq!(channel_name(AUX1), "AUX1");
        assert_eq!(channel_name(libspa_sys::SPA_AUDIO_CHANNEL_AUX63), "AUX63");
    }

    #[test]
    fn channel_name_falls_back_for_unknown_values() {
        assert_eq!(channel_name(libspa_sys::SPA_AUDIO_CHANNEL_UNKNOWN), "?");
        assert_eq!(channel_name(libspa_sys::SPA_AUDIO_CHANNEL_NA), "?");
    }
}
