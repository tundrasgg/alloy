//! Nodes on a one-voter group on `mem` stores, on the simulator: create and
//! update with a generation, heartbeats moving nodes between states on the
//! simulated clock (ARFC 0006 clauses 3.14 and 3.15), and a leader restart.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use alloy_rt::{Runtime, WallTime};
use alloy_seam::{Detail, ImplName, Registry, RegistryBuilder, SeamConfig};
use alloy_sim::{Failure, HostConfig, RunConfig, RunResult, World, run};
use alloy_state::{
    Group, GroupError, Heartbeat, LogStoreSeam, MemStoreConfig, NodeId, NodeSpec, NodeState,
    NodeStatus, NodeView, RecordTerms, Release, ResourceError, Role, Scope, StateStoreSeam,
    StatusReply, register_mem,
};

const ME: NodeId = NodeId([0x11; 16]);

fn fail(error: impl std::fmt::Debug) -> Failure {
    Failure(Detail::new(&format!("{error:?}")))
}

fn check(ok: bool, what: &str) -> Result<(), Failure> {
    if ok {
        Ok(())
    } else {
        Err(Failure(Detail::new(what)))
    }
}

fn registry() -> Registry {
    let mut builder = RegistryBuilder::new();
    register_mem(&mut builder).unwrap();
    builder.build()
}

async fn runtime(world: &World) -> Result<Runtime, Failure> {
    let boot = Arc::new(|_| -> alloy_rt::BoxFuture<'static, ()> { Box::pin(async {}) });
    let host = world.add_host(HostConfig::new(boot)).map_err(fail)?;
    world.runtime(host).map_err(fail)
}

async fn open(registry: &Registry, runtime: &Runtime) -> Result<Group, Failure> {
    let config = MemStoreConfig {
        name: "global".to_owned(),
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
    Group::open(runtime, ME, log, state).await.map_err(fail)
}

fn spec() -> NodeSpec {
    NodeSpec {
        roles: BTreeSet::from([Role::Anchor, Role::Control]),
        zone: "default".to_owned(),
        advertise: vec!["192.0.2.10".parse().unwrap()],
    }
}

/// The status of a node whose record is valid for `secs` more seconds.
fn joined(runtime: &Runtime, id: u8, secs: i64) -> NodeStatus {
    let now = runtime.clock().wall().0.div_euclid(1_000_000_000);
    NodeStatus {
        record: Some(RecordTerms {
            serial: 1,
            not_after: now + secs,
        }),
        ..NodeStatus::pending(NodeId([id; 16]))
    }
}

fn heartbeat(release: &str) -> Heartbeat {
    Heartbeat {
        addresses: vec![vec![192, 0, 2, 10], vec![0x20; 16]],
        release: release.to_owned(),
    }
}

async fn view(group: &Group, name: &str) -> Result<NodeView, Failure> {
    group
        .get_node(name)
        .await
        .map_err(fail)?
        .ok_or_else(|| Failure(Detail::new("the node is missing")))
}

async fn state(group: &Group, name: &str) -> Result<NodeState, Failure> {
    Ok(view(group, name).await?.state)
}

async fn sleep(runtime: &Runtime, secs: u64) {
    runtime.sleep(Duration::from_secs(secs)).await;
}

/// Create and update with a generation, then heartbeats on the simulated
/// clock: `joining`, `active`, `unreachable` after 30 seconds, `active`
/// again, and the replies of clause 3.14.
async fn heartbeats(world: World) -> Result<(), Failure> {
    let registry = registry();
    let rt = runtime(&world).await?;
    let group = open(&registry, &rt).await?;
    check(
        group.accept_heartbeat("n1", &heartbeat("")).await == StatusReply::Unavailable,
        "a member that does not serve the group replies UNAVAILABLE",
    )?;
    group.initialize().await.map_err(fail)?;

    let labels = BTreeMap::from([("rack".to_owned(), "r1".to_owned())]);
    let n1 = group
        .create_node("n1", spec(), joined(&rt, 1, 86_400), labels)
        .await
        .map_err(fail)?;
    check(n1.generation == 1 && n1.spec == spec(), "a new node")?;
    let bad = group
        .create_node("N1", spec(), joined(&rt, 2, 60), BTreeMap::new())
        .await;
    check(
        matches!(bad, Err(GroupError::Resource(ResourceError::Invalid(_)))),
        "a name out of range is refused",
    )?;
    let mut zoned = n1.clone();
    zoned.spec.zone = "z".repeat(64);
    check(
        matches!(
            group.update_node(&zoned).await,
            Err(GroupError::Resource(ResourceError::Invalid(_)))
        ),
        "a zone over 63 bytes is refused",
    )?;
    let mut moved = n1.clone();
    moved.spec.zone = "b".to_owned();
    let n1 = group.update_node(&moved).await.map_err(fail)?;
    check(
        n1.generation == 2 && n1.spec.zone == "b",
        "an update applies",
    )?;
    check(
        group.update_node(&moved).await
            == Err(GroupError::Resource(ResourceError::Conflict {
                named: 1,
                current: 2,
            })),
        "an update naming an old generation conflicts",
    )?;
    group
        .create_node(
            "p1",
            spec(),
            NodeStatus::pending(NodeId([3; 16])),
            BTreeMap::new(),
        )
        .await
        .map_err(fail)?;
    check(state(&group, "p1").await? == NodeState::Pending, "pending")?;
    check(state(&group, "n1").await? == NodeState::Joining, "joining")?;

    let mut many = heartbeat("");
    many.addresses = vec![vec![10, 0, 0, 1]; 65];
    for (name, message, reply) in [
        ("n1", many, StatusReply::Invalid),
        ("n1", heartbeat("v1"), StatusReply::Invalid),
        ("nobody", heartbeat(""), StatusReply::Refused),
        ("p1", heartbeat(""), StatusReply::Refused),
    ] {
        check(
            group.accept_heartbeat(name, &message).await == reply,
            "a heartbeat is refused",
        )?;
    }
    let seen = view(&group, "n1").await?;
    check(
        seen.node.generation == 2 && seen.addresses.is_empty(),
        "a refused heartbeat changes nothing",
    )?;

    let at = rt.clock().wall();
    check(
        group.accept_heartbeat("n1", &heartbeat("0.1.0")).await == StatusReply::Ok,
        "a heartbeat is accepted",
    )?;
    let seen = view(&group, "n1").await?;
    let addresses: Vec<IpAddr> = vec![
        "192.0.2.10".parse().unwrap(),
        "2020:2020:2020:2020:2020:2020:2020:2020".parse().unwrap(),
    ];
    check(
        seen.state == NodeState::Active
            && seen.node.status.reported
            && seen.node.status.running_release == Release::parse("0.1.0")
            && seen.node.generation == 3
            && seen.addresses == addresses
            && seen.last_heartbeat_time.is_some_and(|t| t >= at),
        "the first accepted heartbeat commits reported and the release",
    )?;
    check(
        group.accept_heartbeat("n1", &heartbeat("0.1.0")).await == StatusReply::Ok,
        "a second heartbeat is accepted",
    )?;
    check(
        view(&group, "n1").await?.node.generation == 3,
        "a heartbeat with nothing new commits nothing",
    )?;

    sleep(&rt, 25).await;
    check(
        state(&group, "n1").await? == NodeState::Active,
        "still active",
    )?;
    sleep(&rt, 5).await;
    let seen = view(&group, "n1").await?;
    check(
        seen.state == NodeState::Unreachable && seen.addresses == addresses,
        "unreachable 30 seconds after the last heartbeat, soft state kept",
    )?;
    check(
        group.accept_heartbeat("n1", &heartbeat("")).await == StatusReply::Ok,
        "a heartbeat without a release",
    )?;
    let seen = view(&group, "n1").await?;
    check(
        seen.state == NodeState::Active
            && seen.node.status.running_release == Release::parse("0.1.0"),
        "active again, and an empty release writes nothing",
    )?;

    // A record that expires: heartbeats are refused from then on.
    group
        .create_node("e1", spec(), joined(&rt, 4, 10), BTreeMap::new())
        .await
        .map_err(fail)?;
    check(
        group.accept_heartbeat("e1", &heartbeat("")).await == StatusReply::Ok,
        "accepted while the record is valid",
    )?;
    sleep(&rt, 11).await;
    check(
        group.accept_heartbeat("e1", &heartbeat("")).await == StatusReply::Refused,
        "refused once the record expired by the leader's wall clock",
    )?;
    sleep(&rt, 20).await;
    check(
        state(&group, "e1").await? == NodeState::Unreachable,
        "an expired node shows unreachable",
    )?;

    // A deleted node: refused, gone from the list, and a new node of the
    // same name starts with no soft state.
    let generation = view(&group, "n1").await?.node.generation;
    let deleting = group
        .delete("Node", &Scope::Cluster, "n1", generation)
        .await
        .map_err(fail)?;
    check(
        group.accept_heartbeat("n1", &heartbeat("")).await == StatusReply::Refused,
        "a node being deleted is refused",
    )?;
    group
        .remove("Node", &Scope::Cluster, "n1", deleting.generation)
        .await
        .map_err(fail)?;
    let names: Vec<String> = group
        .list_nodes()
        .await
        .map_err(fail)?
        .into_iter()
        .map(|v| v.node.name)
        .collect();
    check(names == ["e1", "p1"], "a removed node is not listed")?;
    let mut again = joined(&rt, 1, 86_400);
    again.reported = true;
    group
        .create_node("n1", spec(), again, BTreeMap::new())
        .await
        .map_err(fail)?;
    let seen = view(&group, "n1").await?;
    check(
        seen.addresses.is_empty() && seen.last_heartbeat_time.is_none(),
        "a node created again holds none of the old node's soft state",
    )?;
    group.shutdown().await.map_err(fail)
}

/// A leader restart (clause 3.15): the new leader holds no heartbeat times,
/// counts its own start for nodes whose `unreachable_since` is unset, and
/// not for those whose `unreachable_since` is set.
async fn restart(world: World) -> Result<(), Failure> {
    let registry = registry();
    let rt = runtime(&world).await?;
    let group = open(&registry, &rt).await?;
    group.initialize().await.map_err(fail)?;
    for (name, id) in [("a", 1), ("b", 2), ("j", 3)] {
        group
            .create_node(name, spec(), joined(&rt, id, 86_400), BTreeMap::new())
            .await
            .map_err(fail)?;
    }
    group
        .create_node(
            "p",
            NodeSpec::default(),
            NodeStatus::pending(NodeId([4; 16])),
            BTreeMap::new(),
        )
        .await
        .map_err(fail)?;
    for name in ["a", "b"] {
        check(
            group.accept_heartbeat(name, &heartbeat("")).await == StatusReply::Ok,
            "accepted",
        )?;
    }
    // What the node lifecycle controller commits when `b` became
    // unreachable (clause 3.16).
    let mut b = view(&group, "b").await?.node;
    b.status.unreachable_since = Some(WallTime(rt.clock().wall().0));
    group.update_node(&b).await.map_err(fail)?;
    sleep(&rt, 20).await;
    group.shutdown().await.map_err(fail)?;

    let group = open(&registry, &rt).await?;
    let views = group.list_nodes().await.map_err(fail)?;
    let states: Vec<(String, NodeState, bool)> = views
        .into_iter()
        .map(|v| (v.node.name, v.state, v.last_heartbeat_time.is_none()))
        .collect();
    check(
        states
            == [
                ("a".to_owned(), NodeState::Active, true),
                ("b".to_owned(), NodeState::Unreachable, true),
                ("j".to_owned(), NodeState::Joining, true),
                ("p".to_owned(), NodeState::Pending, true),
            ],
        "after a restart: a stays active, b unreachable, no heartbeat times",
    )?;
    sleep(&rt, 25).await;
    check(
        state(&group, "a").await? == NodeState::Active,
        "a counts the new leader's start",
    )?;
    sleep(&rt, 5).await;
    check(
        state(&group, "a").await? == NodeState::Unreachable,
        "a is unreachable 30 seconds after the new leader's start",
    )?;
    check(
        group.accept_heartbeat("b", &heartbeat("")).await == StatusReply::Ok,
        "the new leader accepts b",
    )?;
    check(
        state(&group, "b").await? == NodeState::Active,
        "b is active once the new leader accepts a heartbeat",
    )?;
    group.shutdown().await.map_err(fail)
}

fn failing_seeds<F, Fut>(count: u64, scenario: F) -> Vec<(u64, Option<Detail>)>
where
    F: Fn(World) -> Fut,
    Fut: Future<Output = Result<(), Failure>> + Send + 'static,
{
    (0..count)
        .filter_map(|seed| {
            let summary = run(RunConfig::new(seed), &scenario);
            (summary.result != RunResult::Pass).then_some((seed, summary.failure))
        })
        .collect()
}

#[test]
fn heartbeats_move_nodes_between_states() {
    assert_eq!(failing_seeds(50, heartbeats), Vec::new());
}

#[test]
fn a_leader_restart_keeps_active_and_unreachable_nodes_so() {
    assert_eq!(failing_seeds(50, restart), Vec::new());
}

#[test]
fn a_node_seed_replays_to_the_same_trace() {
    let first = run(RunConfig::new(5), restart);
    assert_eq!(first.result, RunResult::Pass, "{:?}", first.failure);
    assert_eq!(first, run(RunConfig::new(5), restart));
}
