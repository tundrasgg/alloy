//! Reassembly of fragmented frames (ARFC 0003 sections 3.5 and 7).
//!
//! The memory bounds of section 7 count everything a reassembly holds, not
//! only fragment data: each fragment's allocation, each reassembly's own
//! record, its places in the three maps that find it by key and by age, and
//! each peer's record. Every one is charged at a bound on what it takes on
//! the heap, so a peer that sends many small fragments reaches its bound
//! with fewer reassemblies, never with more memory.

use std::collections::BTreeMap;
use std::mem::size_of;
use std::time::Duration;

use alloy_crypto::al::PublicKey;
use alloy_rt::Instant;
use alloy_wire::{Fragment, MAX_FRAGMENTS};

/// A reassembly not complete this long after its first fragment arrived is
/// abandoned (section 3.5).
pub(crate) const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Reassembly memory per peer, in bytes (section 7).
pub(crate) const PEER_REASSEMBLY_BYTES: usize = 4 << 20;

/// Reassembly memory per node, in bytes (section 7).
pub(crate) const NODE_REASSEMBLY_BYTES: usize = 256 << 20;

/// What a reassembly is keyed by: the session, the `receiver_index` and the
/// `frag_id` (section 3.5).
pub(crate) type Key = (u64, u32, u32);

/// Bytes a heap allocation of `len` bytes may take: the size asked for,
/// rounded up to 16, and 16 more for the allocator's own header.
const fn heap(len: usize) -> usize {
    len.next_multiple_of(16) + 16
}

/// Bytes one node of a `BTreeMap<K, V>` may take. The standard library's
/// B-tree node holds at most 11 keys and 11 values, a parent pointer, two
/// 16-bit fields, and, when internal, 12 child pointers.
const fn map_node<K, V>() -> usize {
    heap(11 * size_of::<(K, V)>() + 12 * size_of::<usize>() + 16)
}

/// Bytes one element of a `BTreeMap<K, V>` may take, its root node aside:
/// every node other than the root holds at least 5 elements.
const fn map_element<K, V>() -> usize {
    map_node::<K, V>().div_ceil(5)
}

/// Charged for each reassembly: its boxed record and its elements in
/// `entries`, `order` and its peer's `order`.
const ENTRY_COST: usize =
    heap(size_of::<Entry>()) + map_element::<Key, Box<Entry>>() + 2 * map_element::<u64, Key>();

/// Charged to a peer while it has a reassembly: its element in `peers` and
/// the root node of its `order`.
const PEER_COST: usize = map_element::<PublicKey, PeerUse>() + map_node::<u64, Key>();

/// Charged to the node for the root nodes of `entries`, `order` and `peers`.
const NODE_COST: usize =
    map_node::<Key, Box<Entry>>() + map_node::<u64, Key>() + map_node::<PublicKey, PeerUse>();

/// Why a fragment was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Its count differs from a fragment held with the same `frag_id`, or its
    /// index is held already.
    Conflict,
    /// It alone exceeds a memory bound.
    TooLarge,
}

/// One frame being reassembled.
struct Entry {
    peer: PublicKey,
    count: u8,
    held: u8,
    parts: [Option<Box<[u8]>>; MAX_FRAGMENTS as usize],
    /// Bytes charged for this reassembly: [`ENTRY_COST`] and its fragments.
    charged: usize,
    started: Instant,
    seq: u64,
}

/// A peer's reassemblies.
#[derive(Default)]
struct PeerUse {
    /// Bytes charged: [`PEER_COST`] and its reassemblies.
    charged: usize,
    /// Its reassemblies by age.
    order: BTreeMap<u64, Key>,
}

/// Every reassembly a node holds, with the memory bounds of section 7. When a
/// fragment would exceed a bound, whole reassemblies are dropped, oldest
/// first, until it fits.
pub(crate) struct Reassembler {
    entries: BTreeMap<Key, Box<Entry>>,
    /// Every entry by age: `seq` grows with each entry created.
    order: BTreeMap<u64, Key>,
    peers: BTreeMap<PublicKey, PeerUse>,
    /// Bytes charged to the node: [`NODE_COST`] and every peer's charge.
    total: usize,
    next_seq: u64,
    peer_cap: usize,
    node_cap: usize,
}

impl Reassembler {
    pub(crate) fn new(peer_cap: usize, node_cap: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            peers: BTreeMap::new(),
            total: NODE_COST,
            next_seq: 0,
            peer_cap,
            node_cap,
        }
    }

    /// Adds a fragment received from `peer` on the reassembly `key`. Returns
    /// the whole frame when this fragment completes it, and the number of
    /// reassemblies dropped to make room.
    pub(crate) fn insert(
        &mut self,
        now: Instant,
        peer: PublicKey,
        key: Key,
        fragment: &Fragment<'_>,
    ) -> Result<(Option<Vec<u8>>, u64), Refused> {
        let data = fragment.data();
        let index = usize::from(fragment.index());
        let count = fragment.count().get();
        if let Some(entry) = self.entries.get(&key)
            && (entry.count != count || entry.parts[index].is_some())
        {
            return Err(Refused::Conflict);
        }
        let piece = heap(data.len());
        let mut evicted = 0;
        loop {
            let need = piece
                + if self.entries.contains_key(&key) {
                    0
                } else {
                    ENTRY_COST
                }
                + if self.peers.contains_key(&peer) {
                    0
                } else {
                    PEER_COST
                };
            let oldest = if self.peer_charged(&peer) + need > self.peer_cap {
                self.peers
                    .get(&peer)
                    .and_then(|p| p.order.values().next().copied())
            } else if self.total + need > self.node_cap {
                self.order.values().next().copied()
            } else {
                break;
            };
            let Some(oldest) = oldest else {
                return Err(Refused::TooLarge);
            };
            self.remove(&oldest);
            evicted += 1;
        }
        let user = self.peers.entry(peer).or_insert_with(|| {
            self.total += PEER_COST;
            PeerUse {
                charged: PEER_COST,
                order: BTreeMap::new(),
            }
        });
        let entry = self.entries.entry(key).or_insert_with(|| {
            let seq = self.next_seq;
            self.next_seq += 1;
            self.order.insert(seq, key);
            user.order.insert(seq, key);
            user.charged += ENTRY_COST;
            self.total += ENTRY_COST;
            Box::new(Entry {
                peer,
                count,
                held: 0,
                parts: Default::default(),
                charged: ENTRY_COST,
                started: now,
                seq,
            })
        });
        user.charged += piece;
        self.total += piece;
        entry.parts[index] = Some(Box::from(data));
        entry.held += 1;
        entry.charged += piece;
        if entry.held < entry.count {
            return Ok((None, evicted));
        }
        let frame = entry
            .parts
            .iter()
            .flatten()
            .flat_map(|p| p.iter())
            .copied()
            .collect();
        self.remove(&key);
        Ok((Some(frame), evicted))
    }

    /// Abandons every reassembly whose first fragment arrived
    /// [`REASSEMBLY_TIMEOUT`] or more before `now`. Returns how many.
    pub(crate) fn expire(&mut self, now: Instant) -> u64 {
        let mut expired = 0;
        while let Some(key) = self.order.values().next().copied() {
            match self.entries.get(&key) {
                Some(e) if now.saturating_duration_since(e.started) >= REASSEMBLY_TIMEOUT => {
                    self.remove(&key);
                    expired += 1;
                }
                _ => break,
            }
        }
        expired
    }

    /// Drops every reassembly of the session `session`.
    pub(crate) fn remove_session(&mut self, session: u64) {
        let keys: Vec<Key> = self
            .entries
            .range((session, 0, 0)..=(session, u32::MAX, u32::MAX))
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            self.remove(&key);
        }
    }

    /// When the oldest reassembly times out.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let key = self.order.values().next()?;
        let entry = self.entries.get(key)?;
        Some(entry.started.saturating_add(REASSEMBLY_TIMEOUT))
    }

    /// Bytes charged to `peer`.
    fn peer_charged(&self, peer: &PublicKey) -> usize {
        self.peers.get(peer).map_or(0, |p| p.charged)
    }

    fn remove(&mut self, key: &Key) {
        let Some(entry) = self.entries.remove(key) else {
            return;
        };
        self.order.remove(&entry.seq);
        self.total -= entry.charged;
        if let Some(p) = self.peers.get_mut(&entry.peer) {
            p.charged -= entry.charged;
            p.order.remove(&entry.seq);
            if p.order.is_empty() {
                self.peers.remove(&entry.peer);
                self.total -= PEER_COST;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_wire::{FragmentCount, FragmentId};

    use super::*;

    const T0: Instant = Instant::ZERO;

    fn peer(n: u8) -> PublicKey {
        PublicKey::from_bytes([n; 32])
    }

    fn frag(index: u8, count: u8, data: &[u8]) -> Fragment<'_> {
        Fragment::new(
            FragmentId(0),
            index,
            FragmentCount::new(count).unwrap(),
            data,
        )
        .unwrap()
    }

    #[test]
    fn reassembles_in_any_order() {
        let mut r = Reassembler::new(PEER_REASSEMBLY_BYTES, NODE_REASSEMBLY_BYTES);
        let key = (1, 7, 9);
        assert_eq!(
            r.insert(T0, peer(1), key, &frag(2, 3, b"ef")),
            Ok((None, 0))
        );
        assert_eq!(
            r.insert(T0, peer(1), key, &frag(0, 3, b"ab")),
            Ok((None, 0))
        );
        let (frame, _) = r.insert(T0, peer(1), key, &frag(1, 3, b"cd")).unwrap();
        assert_eq!(frame.as_deref(), Some(&b"abcdef"[..]));
        assert_eq!(r.peer_charged(&peer(1)), 0);
        assert_eq!(r.total, NODE_COST);
    }

    #[test]
    fn drops_conflicting_fragments() {
        let mut r = Reassembler::new(PEER_REASSEMBLY_BYTES, NODE_REASSEMBLY_BYTES);
        let key = (1, 7, 9);
        r.insert(T0, peer(1), key, &frag(0, 3, b"ab")).unwrap();
        assert_eq!(
            r.insert(T0, peer(1), key, &frag(0, 3, b"xx")),
            Err(Refused::Conflict)
        );
        assert_eq!(
            r.insert(T0, peer(1), key, &frag(1, 4, b"cd")),
            Err(Refused::Conflict)
        );
    }

    #[test]
    fn times_out_after_two_seconds() {
        let mut r = Reassembler::new(PEER_REASSEMBLY_BYTES, NODE_REASSEMBLY_BYTES);
        r.insert(T0, peer(1), (1, 0, 0), &frag(0, 2, b"a")).unwrap();
        let almost = T0.saturating_add(REASSEMBLY_TIMEOUT - Duration::from_nanos(1));
        assert_eq!(r.expire(almost), 0);
        assert_eq!(r.deadline(), Some(T0.saturating_add(REASSEMBLY_TIMEOUT)));
        assert_eq!(r.expire(T0.saturating_add(REASSEMBLY_TIMEOUT)), 1);
        assert_eq!(r.deadline(), None);
        assert_eq!(r.total, NODE_COST);
    }

    /// The charges are at least what the structures take: a reassembly's
    /// record and fragment slots, and its key and age in three maps.
    #[test]
    fn charges_cover_the_structures() {
        assert!(size_of::<Entry>() >= usize::from(MAX_FRAGMENTS) * size_of::<usize>() * 2);
        assert!(ENTRY_COST >= size_of::<Entry>() + size_of::<Key>() + 2 * size_of::<(u64, Key)>());
        assert!(map_element::<u64, Key>() >= 2 * size_of::<(u64, Key)>());
        assert!(PEER_COST >= size_of::<(PublicKey, PeerUse)>() + 11 * size_of::<(u64, Key)>());
        assert_eq!(heap(1), 32);
        assert_eq!(heap(16), 32);
        assert_eq!(heap(17), 48);
    }

    /// One-byte fragments, each the first of a frame of 9 that never
    /// completes, are charged their structures as well as their byte: the
    /// peer holds no more reassemblies than its bound pays for.
    #[test]
    fn small_fragments_pay_for_their_structures() {
        let mut r = Reassembler::new(PEER_REASSEMBLY_BYTES, NODE_REASSEMBLY_BYTES);
        let each = ENTRY_COST + heap(1);
        let fits = (PEER_REASSEMBLY_BYTES - PEER_COST) / each;
        let mut evicted = 0;
        for n in 0..50_000u32 {
            let (_, e) = r.insert(T0, peer(1), (1, 0, n), &frag(0, 9, b"x")).unwrap();
            evicted += e;
            assert!(r.peer_charged(&peer(1)) <= PEER_REASSEMBLY_BYTES);
        }
        assert_eq!(r.entries.len(), fits);
        assert_eq!(evicted, 50_000 - u64::try_from(fits).unwrap());
        assert_eq!(r.peer_charged(&peer(1)), PEER_COST + fits * each);
        // The oldest went first: the newest `fits` remain.
        let first = 50_000 - u32::try_from(fits).unwrap();
        assert!(r.entries.contains_key(&(1, 0, first)));
        assert!(!r.entries.contains_key(&(1, 0, first - 1)));
    }

    /// Fills `peer`'s reassemblies with `frames` frames of 16, each holding
    /// 15 fragments of `data`, so none completes.
    fn fill(r: &mut Reassembler, p: u8, frames: u32, data: &[u8]) {
        for id in 0..frames {
            for index in 0..15 {
                let (frame, evicted) = r
                    .insert(T0, peer(p), (u64::from(p), 0, id), &frag(index, 16, data))
                    .unwrap();
                assert!(frame.is_none());
                assert_eq!(evicted, 0);
            }
        }
    }

    /// What a frame of 15 fragments of `len` bytes is charged.
    fn frame_cost(len: usize) -> usize {
        ENTRY_COST + 15 * heap(len)
    }

    #[test]
    fn holds_its_bound_per_peer_then_drops_the_oldest() {
        let data = vec![0x45; 1_000];
        let cap = PEER_COST + 4 * frame_cost(data.len());
        let mut r = Reassembler::new(cap, NODE_REASSEMBLY_BYTES);
        // Four frames fill the bound exactly: all held.
        fill(&mut r, 1, 4, &data);
        assert_eq!(r.peer_charged(&peer(1)), cap);
        // One more byte drops the oldest frame.
        let (_, evicted) = r.insert(T0, peer(1), (1, 1, 0), &frag(0, 2, b"x")).unwrap();
        assert_eq!(evicted, 1);
        assert!(!r.entries.contains_key(&(1, 0, 0)));
        assert!(r.entries.contains_key(&(1, 0, 1)));
        assert_eq!(
            r.peer_charged(&peer(1)),
            cap - frame_cost(data.len()) + ENTRY_COST + heap(1)
        );
        // Another peer is not affected by the first one's bound.
        r.insert(T0, peer(2), (2, 0, 0), &frag(0, 2, b"y")).unwrap();
        assert_eq!(r.peer_charged(&peer(2)), PEER_COST + ENTRY_COST + heap(1));
        // A fragment larger than the bound alone is refused.
        let mut tiny = Reassembler::new(PEER_COST + ENTRY_COST, NODE_REASSEMBLY_BYTES);
        assert_eq!(
            tiny.insert(T0, peer(1), (1, 0, 0), &frag(0, 2, b"z")),
            Err(Refused::TooLarge)
        );
    }

    #[test]
    fn holds_its_bound_per_node_then_drops_the_oldest() {
        let data = vec![0x45; 1_000];
        let per_peer = PEER_COST + 2 * frame_cost(data.len());
        let cap = NODE_COST + 8 * per_peer;
        let mut r = Reassembler::new(PEER_REASSEMBLY_BYTES, cap);
        // Eight peers at two frames each fill the node's bound exactly.
        for p in 1..=8 {
            fill(&mut r, p, 2, &data);
        }
        assert_eq!(r.total, cap);
        // One more byte, from a ninth peer under its own bound, drops the
        // oldest frame of the node: peer 1's first.
        let (_, evicted) = r.insert(T0, peer(9), (9, 0, 0), &frag(0, 2, b"z")).unwrap();
        assert_eq!(evicted, 1);
        assert!(!r.entries.contains_key(&(1, 0, 0)));
        assert!(r.entries.contains_key(&(1, 0, 1)));
        assert!(r.total <= cap);
        assert_eq!(
            r.total,
            cap - frame_cost(data.len()) + PEER_COST + ENTRY_COST + heap(1)
        );
    }

    #[test]
    fn removes_a_sessions_reassemblies() {
        let mut r = Reassembler::new(PEER_REASSEMBLY_BYTES, NODE_REASSEMBLY_BYTES);
        r.insert(T0, peer(1), (1, 0, 0), &frag(0, 2, b"a")).unwrap();
        r.insert(T0, peer(1), (2, 0, 0), &frag(0, 2, b"b")).unwrap();
        r.remove_session(1);
        assert_eq!(r.peer_charged(&peer(1)), PEER_COST + ENTRY_COST + heap(1));
        assert_eq!(r.entries.len(), 1);
    }
}
