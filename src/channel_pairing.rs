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
}
