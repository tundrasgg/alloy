//! Newtypes for the identifiers and counts carried in AL messages.

use crate::{MAX_FRAGMENTS, MAX_QUEUES, MIN_FRAGMENTS};

/// A session index: the `sender_index` a node chooses and the
/// `receiver_index` its peer copies back (ARFC 0003 section 3.1). Opaque to
/// the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionIndex(pub u32);

/// The initiator's handshake counter carried in an initiation payload (ARFC
/// 0003 section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HandshakeCounter(pub u64);

/// The sender's queue id in a transport header (ARFC 0003 section 3.4).
///
/// The wire allows any 16-bit value. Whether a queue id is one the session
/// negotiated is for the session state machine to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueId(pub u16);

/// The per-session, per-queue packet counter in a transport header (ARFC 0003
/// section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PacketCounter(pub u64);

/// The fragment id that ties the fragments of one inner packet together (ARFC
/// 0003 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FragmentId(pub u32);

/// The number of sending queues a node announces in a handshake payload: 1 to
/// [`MAX_QUEUES`] (ARFC 0003 section 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueCount(u16);

impl QueueCount {
    /// Returns the count, or `None` when `n` is outside 1 to [`MAX_QUEUES`].
    pub const fn new(n: u16) -> Option<Self> {
        if n >= 1 && n <= MAX_QUEUES {
            Some(Self(n))
        } else {
            None
        }
    }

    /// The count, 1 to [`MAX_QUEUES`].
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A virtual network identifier: 24 bits, where 0 is the infrastructure
/// network (ARFC 0003 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Vni(u32);

impl Vni {
    /// The infrastructure network.
    pub const INFRASTRUCTURE: Self = Self(0);

    /// The largest VNI, `2^24 - 1`.
    pub const MAX: Self = Self(0x00FF_FFFF);

    /// Returns the VNI, or `None` when `n` does not fit in 24 bits.
    pub const fn new(n: u32) -> Option<Self> {
        if n <= Self::MAX.0 {
            Some(Self(n))
        } else {
            None
        }
    }

    /// The VNI, at most [`Vni::MAX`].
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The three little-endian bytes of the VNI on the wire.
    pub(crate) const fn to_le_bytes(self) -> [u8; 3] {
        let [a, b, c, _] = self.0.to_le_bytes();
        [a, b, c]
    }

    /// The VNI of three little-endian bytes. Every value fits.
    pub(crate) const fn from_le_bytes([a, b, c]: [u8; 3]) -> Self {
        Self(u32::from_le_bytes([a, b, c, 0]))
    }
}

/// The number of fragments a packet is split into: [`MIN_FRAGMENTS`] to
/// [`MAX_FRAGMENTS`] (ARFC 0003 section 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FragmentCount(u8);

impl FragmentCount {
    /// Returns the count, or `None` when `n` is outside [`MIN_FRAGMENTS`] to
    /// [`MAX_FRAGMENTS`].
    pub const fn new(n: u8) -> Option<Self> {
        if n >= MIN_FRAGMENTS && n <= MAX_FRAGMENTS {
            Some(Self(n))
        } else {
            None
        }
    }

    /// The count, [`MIN_FRAGMENTS`] to [`MAX_FRAGMENTS`].
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_count_bounds() {
        assert_eq!(QueueCount::new(0), None);
        assert_eq!(QueueCount::new(1).map(QueueCount::get), Some(1));
        assert_eq!(QueueCount::new(64).map(QueueCount::get), Some(64));
        assert_eq!(QueueCount::new(65), None);
        assert_eq!(QueueCount::new(u16::MAX), None);
    }

    #[test]
    fn vni_bounds_and_bytes() {
        assert_eq!(Vni::new(0), Some(Vni::INFRASTRUCTURE));
        assert_eq!(Vni::new(0x00FF_FFFF), Some(Vni::MAX));
        assert_eq!(Vni::new(0x0100_0000), None);
        let vni = Vni::new(0x0012_3456).unwrap();
        assert_eq!(vni.to_le_bytes(), [0x56, 0x34, 0x12]);
        assert_eq!(Vni::from_le_bytes([0x56, 0x34, 0x12]), vni);
    }

    #[test]
    fn fragment_count_bounds() {
        assert_eq!(FragmentCount::new(1), None);
        assert_eq!(FragmentCount::new(2).map(FragmentCount::get), Some(2));
        assert_eq!(FragmentCount::new(16).map(FragmentCount::get), Some(16));
        assert_eq!(FragmentCount::new(17), None);
    }
}
