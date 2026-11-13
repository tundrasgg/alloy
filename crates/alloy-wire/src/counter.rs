//! Reconstruction of a transport packet's full counter from the 32 bits on the
//! wire (ARFC 0003 section 3.4).

use crate::types::{PacketCounter, WireCounter};

/// 2^32, the number of values a [`WireCounter`] distinguishes.
const WIRE_SPAN: i128 = 1 << 32;

/// 2^31, half of [`WIRE_SPAN`].
const HALF_SPAN: i128 = 1 << 31;

/// Reconstructs the full counter of a transport packet from the `counter`
/// field of its header and the receiver's replay window on its queue.
///
/// `highest` is `H`, the highest counter accepted on the packet's queue in the
/// session, or `None` when nothing has been accepted there. With `e` equal to
/// `H + 1`, or to 0 for `None`, the result is the one integer `c` such that `c`
/// equals `wire` modulo 2^32 and `e - 2^31 < c <= e + 2^31` (ARFC 0003 section
/// 3.4).
///
/// Returns `None` when that `c` is negative, which the receiver must drop, or
/// does not fit in 64 bits, which no window below `u64::MAX` produces. The
/// result is not checked against the window: dropping a counter more than
/// 2,047 below `H` or already accepted is the receiver's replay check.
///
/// The result is exact: for every true counter below
/// [`COUNTER_LIMIT`](crate::COUNTER_LIMIT) and not more than 2,047 below `H`,
/// it returns that counter. Any other result names a nonce the packet was not
/// sealed with, so the packet does not open; reconstruction can make a packet
/// drop but never makes one accept.
pub fn reconstruct_counter(
    wire: WireCounter,
    highest: Option<PacketCounter>,
) -> Option<PacketCounter> {
    let expected = highest.map_or(0, |h| i128::from(h.0) + 1);
    let offset = (i128::from(wire.0) - expected).rem_euclid(WIRE_SPAN);
    // Map the offset from 0..2^32 into the window -2^31 < offset <= 2^31.
    let offset = if offset > HALF_SPAN {
        offset - WIRE_SPAN
    } else {
        offset
    };
    u64::try_from(expected + offset).ok().map(PacketCounter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebuild(wire: u32, highest: Option<u64>) -> Option<u64> {
        reconstruct_counter(WireCounter(wire), highest.map(PacketCounter)).map(|c| c.0)
    }

    #[test]
    fn nothing_accepted_yet() {
        assert_eq!(rebuild(0, None), Some(0));
        assert_eq!(rebuild(5, None), Some(5));
        assert_eq!(rebuild(0x8000_0000, None), Some(0x8000_0000));
        assert_eq!(rebuild(0x8000_0001, None), None);
        assert_eq!(rebuild(u32::MAX, None), None);
    }

    #[test]
    fn interval_bounds() {
        // e = 10: the interval is 10 - 2^31 < c <= 10 + 2^31.
        let h = Some(9);
        assert_eq!(rebuild(10, h), Some(10));
        assert_eq!(rebuild(9, h), Some(9));
        assert_eq!(rebuild(0, h), Some(0));
        assert_eq!(rebuild(0x8000_000a, h), Some(0x8000_000a));
        assert_eq!(rebuild(0x8000_000b, h), None);
    }

    #[test]
    fn across_the_32_bit_wrap() {
        let h = Some(0x1_0000_0005);
        assert_eq!(rebuild(0, h), Some(0x1_0000_0000));
        assert_eq!(rebuild(u32::MAX, h), Some(0xffff_ffff));
        assert_eq!(rebuild(6, h), Some(0x1_0000_0006));
        let h = Some(0xffff_fff0);
        assert_eq!(rebuild(0xffff_fff1, h), Some(0xffff_fff1));
        assert_eq!(rebuild(2, h), Some(0x1_0000_0002));
    }

    #[test]
    fn extreme_windows_do_not_overflow() {
        assert_eq!(rebuild(u32::MAX, Some(u64::MAX - 1)), Some(u64::MAX));
        assert_eq!(rebuild(0, Some(u64::MAX)), None);
        assert_eq!(rebuild(u32::MAX, Some(u64::MAX)), Some(u64::MAX));
    }
}
