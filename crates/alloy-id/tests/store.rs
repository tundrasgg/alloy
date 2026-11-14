//! The stored data of ARFC 0002 section 3.14 on the simulator's disk: each
//! file written whole, `secrets` sealed under the at-rest key, and the
//! at-rest key rotation, with crashes at every point.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alloy_id::AtRestKey;
use alloy_id::proto::{NodeSecrets, NodeState, SignedNodeRecord};
use alloy_id::store::{
    AT_REST_KEY_FILE, AtRestKeys, DataDir, HOST_FILE, NODE_FILE, SECRETS_AD, SECRETS_FILE,
    StoreError,
};
use alloy_rt::{DiskError, RtPath, Runtime};
use alloy_sim::Unsynced;
use common::{Then, err, on_host, run_starts, start};

fn data_dir(runtime: &Runtime) -> Result<DataDir, String> {
    let dir = RtPath::new("/alloyd").map_err(err("path"))?;
    Ok(DataDir::new(Arc::clone(runtime.disk_io()), dir))
}

/// The data directory, created as `init` creates it: protected, and durable
/// before anything is written in it.
async fn make_dir(runtime: &Runtime) -> Result<DataDir, String> {
    let dir = RtPath::new("/alloyd").map_err(err("path"))?;
    DataDir::create(Arc::clone(runtime.disk_io()), dir)
        .await
        .map_err(err("create"))
}

fn secrets() -> NodeSecrets {
    let mut secrets = NodeSecrets::default();
    secrets.identity_key = vec![0x11; 32];
    secrets.mesh_key = vec![0x22; 32];
    secrets.join_token = "alyt_example".to_owned();
    secrets.cluster_key = vec![0x33; 32];
    // Past one 4,096-byte block of the simulated disk, so that writing
    // `secrets` needs more room than writing `at-rest.key`.
    secrets.api_ca_key = vec![0x44; 5_000];
    secrets
}

fn check(condition: bool, what: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(what.to_owned())
    }
}

#[test]
fn files_round_trip_and_secrets_are_sealed() {
    on_host(1, |runtime| async move {
        let dir = make_dir(&runtime).await?;
        let rng = runtime.rng().as_ref();
        dir.write(HOST_FILE, b"alloy-host 1\njoin joining\n")
            .await
            .map_err(err("host"))?;
        let key = dir
            .create_secrets(rng, &secrets())
            .await
            .map_err(err("create"))?;
        let raw = dir
            .read(SECRETS_FILE)
            .await
            .map_err(err("read"))?
            .ok_or("no secrets")?;
        check(
            raw.len() == 24 + prost::Message::encoded_len(&secrets()) + 16,
            "secrets length",
        )?;
        check(
            !raw.windows(32).any(|w| w == [0x11; 32]),
            "identity key in clear",
        )?;
        let opened = dir.open(rng).await.map_err(err("open"))?.ok_or("no key")?;
        check(opened.secrets == secrets(), "secrets differ")?;
        check(opened.key.as_bytes() == key.as_bytes(), "key differs")?;
        check(
            format!("{:?}", opened.secrets) == "NodeSecrets { .. }",
            "debug shows secrets",
        )?;

        // A second write draws a new nonce.
        dir.write_secrets(rng, &key, &secrets())
            .await
            .map_err(err("rewrite"))?;
        let again = dir
            .read(SECRETS_FILE)
            .await
            .map_err(err("read"))?
            .ok_or("no secrets")?;
        check(raw[..24] != again[..24], "nonce repeated")?;

        // The associated data is part of the seal.
        check(SECRETS_AD == b"alloy/at-rest/secrets/v1", "associated data")?;

        dir.write_handshake_counter(0x0102)
            .await
            .map_err(err("counter"))?;
        check(
            dir.read(alloy_id::store::HANDSHAKE_COUNTER_FILE)
                .await
                .map_err(err("read"))?
                .map(|b| b.to_vec())
                == Some(vec![2, 1, 0, 0, 0, 0, 0, 0]),
            "counter is little endian",
        )?;
        check(
            dir.read_handshake_counter().await.map_err(err("counter"))? == Some(0x0102),
            "counter",
        )?;

        let state = NodeState {
            cluster_id: vec![9; 16],
            cluster_pub: vec![vec![8; 32]],
            record: Some(SignedNodeRecord {
                record: vec![1, 2, 3],
                signature: vec![4; 64],
            }),
            pending: true,
            ..NodeState::default()
        };
        dir.write_node_state(&state).await.map_err(err("node"))?;
        check(
            dir.read_node_state().await.map_err(err("node"))? == Some(state),
            "node state",
        )?;

        // A stale `.new` file is ignored by readers and replaced by writers.
        let stale = RtPath::new("/alloyd/node.new").map_err(err("path"))?;
        let disk = runtime.disk_io();
        drop(
            disk.open(
                &stale,
                alloy_rt::OpenOptions {
                    access: alloy_rt::Access::ReadWrite,
                    create: alloy_rt::Create::IfMissing,
                    direct: false,
                    exclusive: false,
                },
            )
            .await
            .map_err(err("stale"))?,
        );
        check(
            dir.read_node_state().await.map_err(err("node"))?.is_some(),
            "stale ignored",
        )?;
        dir.write_node_state(&NodeState::default())
            .await
            .map_err(err("node"))?;
        check(
            dir.read_node_state().await.map_err(err("node"))? == Some(NodeState::default()),
            "rewrite",
        )?;

        // A failed attempt keeps only the node keys, and removes `node`.
        dir.keep_only_node_keys(rng, &opened)
            .await
            .map_err(err("failed attempt"))?;
        let kept = dir.open(rng).await.map_err(err("open"))?.ok_or("no key")?;
        check(kept.secrets.identity_key == vec![0x11; 32], "identity kept")?;
        check(kept.secrets.mesh_key == vec![0x22; 32], "mesh kept")?;
        check(
            kept.secrets.join_token.is_empty() && kept.secrets.cluster_key.is_empty(),
            "rest cleared",
        )?;
        check(
            dir.read(NODE_FILE).await.map_err(err("read"))?.is_none(),
            "node removed",
        )?;

        // `leave` keeps only the host file.
        let disks = RtPath::new("/alloyd/disks").map_err(err("path"))?;
        disk.create_dir(&disks).await.map_err(err("disks"))?;
        drop(
            disk.open(
                &disks.join("0").map_err(err("path"))?,
                alloy_rt::OpenOptions {
                    access: alloy_rt::Access::ReadWrite,
                    create: alloy_rt::Create::IfMissing,
                    direct: false,
                    exclusive: false,
                },
            )
            .await
            .map_err(err("disk 0"))?,
        );
        dir.remove_all_but_host_file().await.map_err(err("leave"))?;
        let names: Vec<String> = disk
            .list(&RtPath::new("/alloyd").map_err(err("path"))?)
            .await
            .map_err(err("list"))?
            .into_iter()
            .map(|e| e.name)
            .collect();
        check(
            names == vec![HOST_FILE.to_owned()],
            "only the host file is left",
        )?;
        check(
            dir.open(rng).await.map_err(err("open"))?.is_none(),
            "no key after leave",
        )
    });
}

#[test]
fn damage_is_detected_and_nothing_is_read() {
    on_host(2, |runtime| async move {
        let dir = make_dir(&runtime).await?;
        let rng = runtime.rng().as_ref();
        dir.create_secrets(rng, &secrets())
            .await
            .map_err(err("create"))?;
        let damaged = |r: Result<_, StoreError>| matches!(r, Err(StoreError::Damaged(_)));

        // Another key of the right length.
        dir.write(AT_REST_KEY_FILE, &[7; 32])
            .await
            .map_err(err("key"))?;
        check(damaged(dir.open(rng).await.map(|_| ())), "wrong key opened")?;
        // Lengths other than 32 and 64.
        for len in [0, 31, 33, 63, 65, 96] {
            dir.write(AT_REST_KEY_FILE, &vec![7; len])
                .await
                .map_err(err("key"))?;
            check(
                damaged(dir.read_at_rest_keys().await.map(|_| ())),
                "bad length read",
            )?;
        }
        // Every flipped bit of `secrets`.
        let key = dir
            .create_secrets(rng, &secrets())
            .await
            .map_err(err("create"))?;
        let good = dir
            .read(SECRETS_FILE)
            .await
            .map_err(err("read"))?
            .ok_or("no secrets")?;
        for bit in (0..good.len() * 8).step_by(7) {
            let mut bad = good.to_vec();
            bad[bit / 8] ^= 1 << (bit % 8);
            dir.write(SECRETS_FILE, &bad).await.map_err(err("write"))?;
            check(
                damaged(dir.open(rng).await.map(|_| ())),
                "flipped secrets opened",
            )?;
        }
        dir.write(SECRETS_FILE, &good[..30])
            .await
            .map_err(err("write"))?;
        check(
            damaged(dir.open(rng).await.map(|_| ())),
            "short secrets opened",
        )?;
        dir.write(SECRETS_FILE, &good).await.map_err(err("write"))?;
        check(
            dir.open(rng)
                .await
                .map_err(err("open"))?
                .is_some_and(|o| o.key.as_bytes() == key.as_bytes()),
            "restored",
        )
    });
}

/// The rotation, stopped at every point by a disk that fills up after a
/// growing number of bytes and then crashed, with unsynced changes lost,
/// kept by a draw, or torn: the next start opens the secrets every time and
/// finishes the rotation.
#[test]
fn at_rest_key_rotation_survives_a_crash_anywhere() {
    for unsynced in [Unsynced::Lost, Unsynced::Seeded, Unsynced::Torn] {
        for capacity in (0..=8u64).map(|blocks| blocks * 4_096) {
            let setup = start(|runtime| async move {
                let dir = make_dir(&runtime).await?;
                dir.write(HOST_FILE, b"alloy-host 1\njoin joined\n")
                    .await
                    .map_err(err("host"))?;
                dir.create_secrets(runtime.rng().as_ref(), &secrets())
                    .await
                    .map_err(err("create"))?;
                Ok(())
            });
            let rotate = start(|runtime| async move {
                let dir = data_dir(&runtime)?;
                let rng = runtime.rng().as_ref();
                let opened = dir.open(rng).await.map_err(err("open"))?.ok_or("no key")?;
                match dir.rotate_at_rest_key(rng, &opened).await {
                    Ok(_) => ROTATED.fetch_add(1, Ordering::Relaxed),
                    Err(StoreError::Disk(DiskError::NoSpace)) => {
                        FULL.fetch_add(1, Ordering::Relaxed)
                    }
                    Err(e) => return Err(format!("rotate: {e}")),
                };
                Ok(())
            });
            let verify = start(|runtime| async move {
                let dir = data_dir(&runtime)?;
                if let Some(AtRestKeys::Rotating { .. }) =
                    dir.read_at_rest_keys().await.map_err(err("keys"))?
                {
                    MID_ROTATION.fetch_add(1, Ordering::Relaxed);
                }
                let opened = dir
                    .open(runtime.rng().as_ref())
                    .await
                    .map_err(err("open"))?
                    .ok_or("no key")?;
                check(
                    opened.secrets == secrets(),
                    "secrets differ after the crash",
                )?;
                let keys = dir
                    .read_at_rest_keys()
                    .await
                    .map_err(err("keys"))?
                    .ok_or("no key")?;
                let AtRestKeys::One(key) = keys else {
                    return Err("the rotation was not finished".to_owned());
                };
                check(
                    key.as_bytes() == opened.key.as_bytes(),
                    "the opened key is not the stored one",
                )?;
                // `open` left the directory in a state that opens again.
                let again = dir
                    .open(runtime.rng().as_ref())
                    .await
                    .map_err(err("reopen"))?
                    .ok_or("no key")?;
                check(again.secrets == secrets(), "secrets differ on reopen")
            });
            run_starts(
                capacity * 31 + 7,
                vec![
                    (
                        setup,
                        Then {
                            unsynced: Unsynced::Lost,
                            capacity,
                        },
                    ),
                    (
                        rotate,
                        Then {
                            unsynced,
                            capacity: 1 << 30,
                        },
                    ),
                    (verify, Then::FINISH),
                ],
            );
        }
    }
    // The sweep stopped rotations at each point: some failed before any
    // write, some left both keys for the restart to finish, and some
    // finished.
    assert!(FULL.load(Ordering::Relaxed) > MID_ROTATION.load(Ordering::Relaxed));
    assert!(MID_ROTATION.load(Ordering::Relaxed) > 0);
    assert!(ROTATED.load(Ordering::Relaxed) > 0);
}

static MID_ROTATION: AtomicUsize = AtomicUsize::new(0);
static FULL: AtomicUsize = AtomicUsize::new(0);
static ROTATED: AtomicUsize = AtomicUsize::new(0);

/// Each state a crash can leave between the three writes of a rotation
/// (section 3.14), built directly: the next start opens the secrets and
/// ends with the new key alone, or with the old key when the rotation had
/// not begun.
#[test]
fn every_rotation_state_opens() {
    on_host(3, |runtime| async move {
        let dir = make_dir(&runtime).await?;
        let rng = runtime.rng().as_ref();
        let old = AtRestKey::generate(rng).map_err(err("old"))?;
        let new = AtRestKey::generate(rng).map_err(err("new"))?;
        let mut both = old.as_bytes().to_vec();
        both.extend_from_slice(new.as_bytes());
        // (at-rest.key, the key sealing secrets, the key expected after open)
        let states: [(&[u8], &AtRestKey, &AtRestKey); 4] = [
            (old.as_bytes(), &old, &old),
            (&both, &old, &new),
            (&both, &new, &new),
            (new.as_bytes(), &new, &new),
        ];
        for (file, sealing, expected) in states {
            dir.write(AT_REST_KEY_FILE, file)
                .await
                .map_err(err("key"))?;
            dir.write_secrets(rng, sealing, &secrets())
                .await
                .map_err(err("secrets"))?;
            let opened = dir.open(rng).await.map_err(err("open"))?.ok_or("no key")?;
            check(opened.secrets == secrets(), "secrets differ")?;
            check(
                opened.key.as_bytes() == expected.as_bytes(),
                "wrong key after open",
            )?;
            let stored = dir
                .read(AT_REST_KEY_FILE)
                .await
                .map_err(err("read"))?
                .ok_or("no key")?;
            check(
                stored[..] == expected.as_bytes()[..],
                "rotation not finished on disk",
            )?;
            check(
                dir.open(rng).await.map_err(err("reopen"))?.is_some(),
                "reopen",
            )?;
        }
        Ok(())
    });
}
