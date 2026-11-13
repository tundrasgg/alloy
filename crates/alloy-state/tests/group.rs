//! A one-voter group on `mem` stores, on the simulator: resource operations
//! with generation checks, restart, snapshot and purge, and a seed replay.

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy_rt::Runtime;
use alloy_seam::{Detail, ImplName, Registry, RegistryBuilder, SeamConfig};
use alloy_sim::{Failure, HostConfig, RunConfig, RunResult, RunSummary, World, run};
use alloy_state::{
    Group, GroupError, LogStore, LogStoreSeam, MemStoreConfig, NodeId, ResourceError, Scope,
    StateStore, StateStoreSeam, register_mem,
};

const ME: NodeId = NodeId([0x11; 16]);

type Stores = (Arc<dyn LogStore>, Arc<dyn StateStore>);

fn fail(error: impl std::fmt::Debug) -> Failure {
    Failure(Detail::new(&format!("{error:?}")))
}

fn registry() -> Registry {
    let mut builder = RegistryBuilder::new();
    register_mem(&mut builder).unwrap();
    builder.build()
}

fn stores(registry: &Registry, runtime: &Runtime, name: &str) -> Result<Stores, Failure> {
    let config = MemStoreConfig {
        name: name.to_owned(),
        fail_after: 0,
    }
    .to_value(1)
    .map_err(fail)?;
    let mem = ImplName::new("mem").map_err(fail)?;
    let log = registry
        .instantiate::<LogStoreSeam>(runtime, &mem, &config)
        .map_err(fail)?;
    let state = registry
        .instantiate::<StateStoreSeam>(runtime, &mem, &config)
        .map_err(fail)?;
    Ok((log, state))
}

async fn runtime(world: &World) -> Result<Runtime, Failure> {
    let boot = Arc::new(|_| -> alloy_rt::BoxFuture<'static, ()> { Box::pin(async {}) });
    let host = world.add_host(HostConfig::new(boot)).map_err(fail)?;
    world.runtime(host).map_err(fail)
}

async fn open(registry: &Registry, runtime: &Runtime) -> Result<Group, Failure> {
    let (log, state) = stores(registry, runtime, "global")?;
    Group::open(runtime, ME, log, state).await.map_err(fail)
}

fn check(ok: bool, what: &str) -> Result<(), Failure> {
    if ok {
        Ok(())
    } else {
        Err(Failure(Detail::new(what)))
    }
}

async fn scenario(world: World) -> Result<(), Failure> {
    let registry = registry();
    let runtime = runtime(&world).await?;
    let group = open(&registry, &runtime).await?;
    group.initialize().await.map_err(fail)?;

    let labels = BTreeMap::from([("zone".to_owned(), "a".to_owned())]);
    let n1 = group
        .create("Node", Scope::Cluster, "n1", b"roles".to_vec(), labels)
        .await
        .map_err(fail)?;
    check(
        n1.generation == 1 && !n1.deleting,
        "a new resource has generation 1",
    )?;
    let again = group
        .create("Node", Scope::Cluster, "n1", Vec::new(), BTreeMap::new())
        .await;
    check(
        again == Err(GroupError::Resource(ResourceError::AlreadyExists)),
        "a second create conflicts",
    )?;
    let got = group
        .get("Node", &Scope::Cluster, "n1")
        .await
        .map_err(fail)?;
    check(
        got.as_ref() == Some(&n1),
        "get returns what create returned",
    )?;

    let mut stale = n1.clone();
    stale.generation = 7;
    let refused = group.update(stale).await;
    check(
        refused
            == Err(GroupError::Resource(ResourceError::Conflict {
                named: 7,
                current: 1,
            })),
        "an update naming another generation conflicts",
    )?;
    let mut change = n1.clone();
    change.status = b"reported".to_vec();
    let n1 = group.update(change).await.map_err(fail)?;
    check(
        n1.generation == 2 && n1.status == b"reported",
        "an update applies",
    )?;

    for i in 0..20 {
        let name = format!("n{}", i + 2);
        group
            .create("Node", Scope::Cluster, &name, Vec::new(), BTreeMap::new())
            .await
            .map_err(fail)?;
    }
    group
        .create("Other", Scope::Cluster, "x", Vec::new(), BTreeMap::new())
        .await
        .map_err(fail)?;
    let nodes = group.list("Node").await.map_err(fail)?;
    check(nodes.len() == 21, "list returns one kind only")?;

    let deleting = group
        .delete("Node", &Scope::Cluster, "n3", 1)
        .await
        .map_err(fail)?;
    check(
        deleting.deleting && deleting.generation == 2,
        "delete marks",
    )?;
    group
        .remove("Node", &Scope::Cluster, "n3", 2)
        .await
        .map_err(fail)?;
    let gone = group
        .get("Node", &Scope::Cluster, "n3")
        .await
        .map_err(fail)?;
    check(gone.is_none(), "remove removes")?;

    // Snapshot, purge, restart: the state survives and the log restarts
    // after the purge.
    let at = group.snapshot().await.map_err(fail)?;
    check(at.is_some(), "a snapshot has a position")?;
    group.shutdown().await.map_err(fail)?;
    let (log, _) = stores(&registry, &runtime, "global")?;
    let state = log.state().await.map_err(fail)?;
    check(
        state.last_purged == at && state.last == at,
        "the log is purged through the snapshot",
    )?;
    drop(log);

    let group = open(&registry, &runtime).await?;
    let got = group
        .get("Node", &Scope::Cluster, "n1")
        .await
        .map_err(fail)?;
    check(got.as_ref() == Some(&n1), "a resource survives a restart")?;
    let n9 = group
        .create("Node", Scope::Cluster, "n99", Vec::new(), BTreeMap::new())
        .await
        .map_err(fail)?;
    group.shutdown().await.map_err(fail)?;

    // A restart without a snapshot applies the log at open.
    let group = open(&registry, &runtime).await?;
    let got = group
        .get("Node", &Scope::Cluster, "n99")
        .await
        .map_err(fail)?;
    check(
        got == Some(n9),
        "a write after the snapshot survives a restart",
    )?;
    check(
        group.list("Node").await.map_err(fail)?.len() == 21,
        "the list after restarts",
    )?;
    group.shutdown().await.map_err(fail)?;
    Ok(())
}

fn once(seed: u64) -> RunSummary {
    run(RunConfig::new(seed), scenario)
}

#[test]
fn a_group_writes_reads_and_restarts() {
    let summary = once(1);
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
}

/// R-0004 and ADR 0064: openraft's timers, tasks, and draws all go through
/// the runtime, so a seed replays to the same trace.
#[test]
fn a_seed_replays_to_the_same_trace() {
    for seed in [2, 3] {
        let first = once(seed);
        assert_eq!(first.result, RunResult::Pass, "{:?}", first.failure);
        assert_eq!(first, once(seed), "seed {seed}");
    }
}
