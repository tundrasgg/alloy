//! Two links under loss, duplication and reordering, by seed: no packet is
//! delivered twice, and once the faults stop, traffic flows both ways on
//! every VNI within 10 seconds, whatever rekeys crossed, were lost or came
//! late (ARFC 0003 sections 3.3 and 3.4).

mod common;

use std::time::Duration;

use alloy_wire::Vni;
use common::{A, B, Pair, vni, with_rng};

/// Seeds run by `cargo test`.
const SEEDS: u64 = 32;

/// Steps of faulty traffic before the faults stop: about 375 seconds, so
/// sessions rekey about three times.
const STEPS: usize = 300;

/// The test's own choices, apart from the links' randomness.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// True with probability `percent` in 100.
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// An IPv4 packet carrying `id`, so each delivery can be traced to its send.
fn packet(id: u32) -> Vec<u8> {
    let mut p = vec![0x45; 24];
    p[4..8].copy_from_slice(&id.to_le_bytes());
    p
}

fn id_of(packet: &[u8]) -> u32 {
    u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]])
}

/// The VNIs both nodes carry: the infrastructure network and one VPC.
fn vnis() -> [Vni; 2] {
    [Vni::INFRASTRUCTURE, vni(5)]
}

/// Delivers what is in flight until none is left. While `faulty`, each
/// datagram is lost with 10 percent chance, duplicated with 5, and each
/// batch is shuffled.
fn deliver(p: &mut Pair<'_>, rand: &mut SplitMix, faulty: bool) {
    while !p.inflight[A].is_empty() || !p.inflight[B].is_empty() {
        let mut batch = Vec::new();
        for i in [A, B] {
            for d in p.take(i) {
                if faulty && rand.chance(10) {
                    continue;
                }
                if faulty && rand.chance(5) {
                    batch.push((i, d.clone()));
                }
                batch.push((i, d));
            }
        }
        if faulty {
            for n in (1..batch.len()).rev() {
                let bound = u64::try_from(n + 1).unwrap();
                let m = usize::try_from(rand.below(bound)).unwrap();
                batch.swap(n, m);
            }
        }
        for (i, d) in batch {
            let _ = p.recv(i, d);
        }
    }
}

/// Records what each node delivered since the last call. Fails on a packet
/// delivered twice, or by the node that sent it.
fn collect(
    p: &mut Pair<'_>,
    sent_by: &[usize],
    seen: &mut std::collections::BTreeSet<u32>,
) -> Option<String> {
    for i in [A, B] {
        for (_, packet) in p.delivered(i) {
            let id = id_of(&packet);
            let from = usize::try_from(id).ok().and_then(|n| sent_by.get(n));
            if from != Some(&(1 - i)) || !seen.insert(id) {
                return Some(format!("packet {id} delivered twice or astray"));
            }
        }
    }
    None
}

/// Runs one seed. Returns why it failed, if it did.
fn run(seed: u64) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    with_rng(seed, move |rng| {
        let mut p = Pair::new(rng);
        let mut rand = SplitMix(seed);
        for i in [A, B] {
            p.nodes[i].add_local_vni(vni(5));
        }
        // The node that sent each id, and the ids each node delivered.
        let mut next = 0u32;
        let mut sent_by = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut failure = None;
        for _ in 0..STEPS {
            p.advance(Duration::from_millis(rand.below(2_500)));
            for i in [A, B] {
                if rand.chance(60) {
                    let v = vnis()[usize::try_from(rand.below(2)).unwrap()];
                    sent_by.push(i);
                    let _ = p.send(i, v, &packet(next));
                    next += 1;
                }
            }
            deliver(&mut p, &mut rand, true);
            if let Some(f) = collect(&mut p, &sent_by, &mut seen) {
                failure = Some(format!("seed {seed}: {f}"));
                break;
            }
        }
        // Faults stop. Each second for 10 seconds, each node sends one packet
        // on each VNI; at least one must arrive per direction and VNI.
        let mut arrived = [[false; 2]; 2];
        let mut asked = Vec::new();
        for _ in 0..10 {
            if failure.is_some() {
                break;
            }
            for i in [A, B] {
                for (k, v) in vnis().into_iter().enumerate() {
                    sent_by.push(i);
                    asked.push((next, i, k));
                    let _ = p.send(i, v, &packet(next));
                    next += 1;
                }
            }
            deliver(&mut p, &mut rand, false);
            let before = seen.clone();
            if let Some(f) = collect(&mut p, &sent_by, &mut seen) {
                failure = Some(format!("seed {seed}: {f}"));
            }
            for (id, i, k) in &asked {
                if seen.contains(id) && !before.contains(id) {
                    arrived[*i][*k] = true;
                }
            }
            p.advance(Duration::from_secs(1));
        }
        if failure.is_none() && arrived != [[true; 2]; 2] {
            failure = Some(format!(
                "seed {seed}: after the faults, arrived per (sender, VNI) {arrived:?}"
            ));
        }
        tx.send(failure).unwrap();
    });
    match rx.recv().unwrap() {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

#[test]
fn traffic_recovers_after_loss_duplication_and_reordering() {
    let failures: Vec<String> = (0..SEEDS).filter_map(|seed| run(seed).err()).collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds failed: {failures:#?}",
        failures.len()
    );
}
