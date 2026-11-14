//! The entries a control node keeps per origin (ARFC 0002 section 3.13), so
//! that a request is handled at most once while its `alloyd` runs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use alloy_crypto::al::PublicKey;
use alloy_rt::Instant;

/// How long an entry is kept after the last request for it arrived: no
/// resend of a request it took can come later.
pub const ENTRY_LIFETIME: Duration = Duration::from_secs(30);

/// The most entries whose origin is one mesh key, and the most for the
/// requests one anchor forwarded.
pub const MAX_ENTRIES: usize = 8_192;

/// Where a request came from: the mesh key the AL session it arrived on
/// authenticated, and for a request in a `forwarded`, the mesh key the
/// `forwarded` names. An origin is made of mesh keys, never of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    /// The key the session authenticated: the requester, or the anchor that
    /// forwarded the request.
    pub session: PublicKey,
    /// The key a `forwarded` names, `None` for a request that came direct.
    pub forwarded: Option<PublicKey>,
}

/// What to do with a request [`Entries::admit`] was asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Handle it, then give its reply to [`Entries::reply`].
    Handle,
    /// A resend of a request already handled: send this kept reply again.
    Resend(Vec<u8>),
    /// A resend of a request whose reply does not exist yet, or a request
    /// below the highest taken, or one that would pass a bound: drop it.
    Drop,
}

#[derive(Debug)]
struct Entry {
    highest: u64,
    reply: Option<Vec<u8>>,
    last: Instant,
}

/// The entries of one control node.
#[derive(Debug, Default)]
pub struct Entries {
    entries: BTreeMap<Origin, Entry>,
    /// Each entry by the instant its last request arrived, so that
    /// forgetting entries costs time in the number forgotten, not the
    /// number held (T-0111 review F3).
    by_last: BTreeSet<(Instant, Origin)>,
    direct: usize,
    per_anchor: BTreeMap<PublicKey, usize>,
}

impl Entries {
    /// No entries.
    #[must_use]
    pub fn new() -> Entries {
        Entries::default()
    }

    /// Decides what to do with a request with `request_id` from `origin`,
    /// arriving at `now`. A request with the entry's id gets the kept reply
    /// again, or nothing while it does not exist; a lower id is dropped; a
    /// higher one, or any when the origin has no entry, is handled. A
    /// request that would add an entry beyond its bound is dropped. Entries
    /// past [`ENTRY_LIFETIME`] are forgotten first.
    pub fn admit(&mut self, origin: Origin, request_id: u64, now: Instant) -> Admit {
        self.expire(now);
        if let Some(entry) = self.entries.get_mut(&origin) {
            self.by_last.remove(&(entry.last, origin));
            self.by_last.insert((now, origin));
            entry.last = now;
            return match request_id.cmp(&entry.highest) {
                std::cmp::Ordering::Less => Admit::Drop,
                std::cmp::Ordering::Equal => entry.reply.clone().map_or(Admit::Drop, Admit::Resend),
                std::cmp::Ordering::Greater => {
                    entry.highest = request_id;
                    entry.reply = None;
                    Admit::Handle
                }
            };
        }
        let full = match origin.forwarded {
            None => self.direct >= MAX_ENTRIES,
            Some(_) => self.per_anchor.get(&origin.session).copied().unwrap_or(0) >= MAX_ENTRIES,
        };
        if full {
            return Admit::Drop;
        }
        match origin.forwarded {
            None => self.direct += 1,
            Some(_) => *self.per_anchor.entry(origin.session).or_insert(0) += 1,
        }
        self.by_last.insert((now, origin));
        self.entries.insert(
            origin,
            Entry {
                highest: request_id,
                reply: None,
                last: now,
            },
        );
        Admit::Handle
    }

    /// Keeps `reply` as the reply to the request `request_id` of `origin`,
    /// when that is still the entry's request. A reply to a request that a
    /// higher one has since replaced is not kept.
    pub fn reply(&mut self, origin: Origin, request_id: u64, reply: Vec<u8>) {
        if let Some(entry) = self.entries.get_mut(&origin)
            && entry.highest == request_id
        {
            entry.reply = Some(reply);
        }
    }

    /// Forgets the entries whose last request arrived [`ENTRY_LIFETIME`] or
    /// more before `now`.
    pub fn expire(&mut self, now: Instant) {
        while let Some(&(last, origin)) = self.by_last.first() {
            if now.saturating_duration_since(last) < ENTRY_LIFETIME {
                break;
            }
            self.by_last.remove(&(last, origin));
            self.entries.remove(&origin);
            match origin.forwarded {
                None => self.direct -= 1,
                Some(_) => {
                    if let Some(count) = self.per_anchor.get_mut(&origin.session) {
                        *count -= 1;
                        if *count == 0 {
                            self.per_anchor.remove(&origin.session);
                        }
                    }
                }
            }
        }
    }

    /// The number of entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no entry is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u16) -> PublicKey {
        let mut bytes = [0u8; 32];
        bytes[..2].copy_from_slice(&n.to_be_bytes());
        PublicKey::from_bytes(bytes)
    }

    fn direct(n: u16) -> Origin {
        Origin {
            session: key(n),
            forwarded: None,
        }
    }

    fn at(secs: u64) -> Instant {
        Instant::from_nanos(secs * 1_000_000_000)
    }

    #[test]
    fn a_request_is_handled_once() {
        let mut e = Entries::new();
        let o = direct(1);
        assert_eq!(e.admit(o, 5, at(0)), Admit::Handle);
        assert_eq!(e.admit(o, 5, at(1)), Admit::Drop, "no reply yet");
        e.reply(o, 5, b"reply".to_vec());
        assert_eq!(e.admit(o, 5, at(2)), Admit::Resend(b"reply".to_vec()));
        assert_eq!(e.admit(o, 4, at(2)), Admit::Drop);
        assert_eq!(e.admit(o, 9, at(3)), Admit::Handle);
        e.reply(o, 5, b"late".to_vec());
        assert_eq!(
            e.admit(o, 9, at(3)),
            Admit::Drop,
            "the old reply is not kept"
        );
    }

    #[test]
    fn origins_are_mesh_keys_and_forwarding() {
        let mut e = Entries::new();
        let anchor = key(9);
        let via = Origin {
            session: anchor,
            forwarded: Some(key(1)),
        };
        assert_eq!(e.admit(direct(1), 3, at(0)), Admit::Handle);
        assert_eq!(e.admit(via, 3, at(0)), Admit::Handle);
        assert_eq!(e.admit(direct(9), 3, at(0)), Admit::Handle);
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn entries_are_forgotten_after_30_seconds_of_quiet() {
        let mut e = Entries::new();
        let o = direct(1);
        e.admit(o, 5, at(0));
        e.reply(o, 5, vec![1]);
        assert_eq!(e.admit(o, 5, at(29)), Admit::Resend(vec![1]));
        assert_eq!(e.admit(o, 5, at(58)), Admit::Resend(vec![1]));
        e.expire(at(88));
        assert!(e.is_empty());
        assert_eq!(
            e.admit(o, 5, at(88)),
            Admit::Handle,
            "a restart of the window"
        );
    }

    #[test]
    fn bounds_hold_per_kind_of_origin() {
        let mut e = Entries::new();
        for n in 0..MAX_ENTRIES as u16 {
            assert_eq!(e.admit(direct(n), 1, at(0)), Admit::Handle);
        }
        assert_eq!(e.admit(direct(u16::MAX), 1, at(0)), Admit::Drop);
        let anchor = key(u16::MAX - 1);
        for n in 0..MAX_ENTRIES as u16 {
            let via = Origin {
                session: anchor,
                forwarded: Some(key(n)),
            };
            assert_eq!(e.admit(via, 1, at(1)), Admit::Handle);
        }
        let over = Origin {
            session: anchor,
            forwarded: Some(key(u16::MAX)),
        };
        assert_eq!(e.admit(over, 1, at(1)), Admit::Drop);
        let other = Origin {
            session: key(u16::MAX - 2),
            forwarded: Some(key(u16::MAX)),
        };
        assert_eq!(e.admit(other, 1, at(1)), Admit::Handle);
        e.expire(at(30));
        assert_eq!(e.admit(direct(u16::MAX), 1, at(30)), Admit::Handle);
    }
}
