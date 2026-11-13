//! The replay window of one sending queue of the peer in one session (ARFC
//! 0003 section 3.4).

use alloy_wire::{PacketCounter, WireCounter, reconstruct_counter};

/// Counters the window remembers: `H` and the 2,047 below it.
pub(crate) const WINDOW: u64 = 2_048;

const WORDS: usize = (WINDOW / 64) as usize;

/// The highest counter accepted, `H`, and which of the [`WINDOW`] counters up
/// to `H` were accepted, as a ring of bits indexed by counter modulo
/// [`WINDOW`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ReplayWindow {
    highest: Option<u64>,
    bits: [u64; WORDS],
}

impl ReplayWindow {
    /// The full counter of a packet whose header carries `wire`, if it passes
    /// the checks of section 3.4: it is not negative, not more than 2,047
    /// below `H`, and not accepted already. Changes nothing.
    pub(crate) fn check(&self, wire: WireCounter) -> Option<u64> {
        let c = reconstruct_counter(wire, self.highest.map(PacketCounter))?.0;
        match self.highest {
            Some(h) if c <= h => (h - c < WINDOW && !self.seen(c)).then_some(c),
            _ => Some(c),
        }
    }

    /// Marks `c` accepted, after its packet opened. `c` passed [`check`].
    ///
    /// [`check`]: ReplayWindow::check
    pub(crate) fn accept(&mut self, c: u64) {
        match self.highest {
            Some(h) if c <= h => {}
            Some(h) if c - h < WINDOW => {
                for n in h + 1..c {
                    self.set(n, false);
                }
                self.highest = Some(c);
            }
            _ => {
                self.bits = [0; WORDS];
                self.highest = Some(c);
            }
        }
        self.set(c, true);
    }

    fn seen(&self, c: u64) -> bool {
        let at = c % WINDOW;
        self.bits[(at / 64) as usize] & (1 << (at % 64)) != 0
    }

    fn set(&mut self, c: u64, on: bool) {
        let at = c % WINDOW;
        let word = &mut self.bits[(at / 64) as usize];
        if on {
            *word |= 1 << (at % 64);
        } else {
            *word &= !(1 << (at % 64));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(c: u64) -> WireCounter {
        PacketCounter(c).wire()
    }

    #[test]
    fn accepts_in_order_and_rejects_repeats() {
        let mut w = ReplayWindow::default();
        for c in 0..10 {
            assert_eq!(w.check(wire(c)), Some(c));
            w.accept(c);
            assert_eq!(w.check(wire(c)), None);
        }
    }

    #[test]
    fn the_window_is_2048_counters() {
        let mut w = ReplayWindow::default();
        w.accept(5_000);
        // 2,047 below H is inside the window, 2,048 below is not.
        assert_eq!(w.check(wire(5_000 - 2_047)), Some(5_000 - 2_047));
        assert_eq!(w.check(wire(5_000 - 2_048)), None);
        w.accept(5_000 - 2_047);
        assert_eq!(w.check(wire(5_000 - 2_047)), None);
        // Reordered packets inside the window are accepted once each.
        assert_eq!(w.check(wire(4_999)), Some(4_999));
        w.accept(4_999);
        assert_eq!(w.check(wire(4_999)), None);
    }

    #[test]
    fn a_jump_clears_the_bits_it_passes() {
        let mut w = ReplayWindow::default();
        w.accept(10);
        w.accept(10 + WINDOW + 5);
        // Counter 10 + WINDOW shares a bit with 10; it was never accepted.
        assert_eq!(w.check(wire(10 + WINDOW)), Some(10 + WINDOW));
        w.accept(20 + WINDOW);
        assert_eq!(w.check(wire(20)), None);
        assert_eq!(w.check(wire(19 + WINDOW)), Some(19 + WINDOW));
    }

    #[test]
    fn reconstructs_across_the_32_bit_wrap() {
        let mut w = ReplayWindow::default();
        let h = (1u64 << 32) - 3;
        w.accept(h);
        assert_eq!(w.check(WireCounter(2)), Some((1u64 << 32) + 2));
        // Nothing accepted yet: a counter of 2^31 + 1 would reconstruct to a
        // negative value and is dropped.
        let fresh = ReplayWindow::default();
        assert_eq!(fresh.check(WireCounter(0x8000_0001)), None);
    }
}
