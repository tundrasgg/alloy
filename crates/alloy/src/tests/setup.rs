//! `alloy init` and `alloy join` with the simulator's `mem` instance of
//! `host` as the machine, under the `os` runtime on loopback.
//!
//! The `mem` service runs no program, so each test plays the service
//! manager: once `init` or `join` has started the service, it starts what
//! `alloyd` would serve, a [`ControlNode`] or a [`MemberNode`], on the data
//! directory the command wrote. A test does not run as root, so the data
//! directory is not an Alloy data directory: the tests run the checks on
//! their own, and the steps after them through `after_checks`.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_api::Transport;
use alloy_id::DataDir;
use alloy_id::datadir::InitAction;
use alloy_proto::cluster::v1alpha1::NodeState;
use alloy_rt::{
    Arch, Host, OsHost, Platform, RunState, Runtime, ServiceStatus, StreamAddr, TaskName,
    UdpOptions,
};
use alloy_sim::{HostBoot, HostConfig, MachineConfig, RunConfig, RunResult, World};
use alloyd::{ControlNode, JoinState, Listeners, MemberNode, NodeError, NodePaths};

use super::e2e::{AsRoot, Unprotected, listen, remove_tree};
use super::*;
use crate::machine::{os_runtime, split};
use crate::setup::checks::{self, Kind, Passed};
use crate::setup::{DataPath, Setup};

/// A `mem` instance of `host` for `config`, from a simulated host, and the
/// world that holds its state, which must outlive it.
fn mem_machine(config: MachineConfig) -> (Arc<dyn Host>, World) {
    type Slot = Arc<Mutex<Option<(Arc<dyn Host>, World)>>>;
    let slot: Slot = Arc::default();
    let kept = Arc::clone(&slot);
    let summary = alloy_sim::run(RunConfig::new(1), move |world: World| async move {
        let machine: Arc<Mutex<Option<Arc<dyn Host>>>> = Arc::default();
        let held = Arc::clone(&machine);
        let mut host = HostConfig::new(Arc::new(move |b: HostBoot| {
            *held.lock().unwrap() = Some(Arc::clone(&b.machine));
            Box::pin(async {})
        }));
        host.machine = config;
        world.add_host(host).expect("a host");
        world.sleep(Duration::from_millis(1)).await;
        let machine = machine.lock().unwrap().clone().expect("booted");
        *kept.lock().unwrap() = Some((machine, world.clone()));
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    slot.lock().unwrap().take().expect("a machine")
}

/// The `os` runtime on loopback with every protection `Default`, as in
/// [`super::e2e`], and a fresh directory under the temporary directory.
struct Bench {
    host: OsHost,
    runtime: Runtime,
    transport: Transport,
    base: PathBuf,
}

impl Bench {
    fn new() -> Bench {
        let temp = std::env::temp_dir();
        let host = OsHost::start(2).expect("host");
        let (root, _) = split(&temp).expect("split");
        let (os, transport) = os_runtime(&host, &root).expect("runtime");
        let runtime = Runtime::new(
            Arc::clone(os.clock()),
            Arc::clone(os.net()),
            Arc::new(Unprotected(Arc::clone(os.disk_io()))),
            Arc::clone(os.rng()),
            Arc::clone(os.spawner()),
        );
        let mut draw = [0u8; 8];
        runtime.rng().fill(&mut draw).expect("random");
        let base = temp.join(format!(
            "alloy-setup-test-{:016x}",
            u64::from_be_bytes(draw)
        ));
        let (_, base_rt) = split(&base).expect("split");
        host.block_on(runtime.disk_io().create_dir(&base_rt))
            .expect("base");
        Bench {
            host,
            runtime,
            transport,
            base,
        }
    }

    fn finish(self) {
        let (_, base_rt) = split(&self.base).expect("split");
        self.host
            .block_on(remove_tree(self.runtime.disk_io().as_ref(), &base_rt));
        self.host.shutdown(Duration::from_secs(20));
    }
}

fn env_with_admin(admin: Option<SocketAddr>) -> Env {
    Env {
        admin_socket: admin.map(StreamAddr::Ip),
        ..Env::default()
    }
}

fn spec(id: CommandId) -> &'static CommandSpec {
    COMMANDS
        .iter()
        .find(|spec| spec.id == id)
        .expect("registered")
}

fn invocation(args: &[&str]) -> Invocation {
    let args: Vec<String> = args.iter().map(|&a| a.to_owned()).collect();
    match parse(COMMANDS, &args) {
        Ok(Parsed::Run(_, invocation)) => invocation,
        other => panic!("{args:?}: {other:?}"),
    }
}

fn paths_of(runtime: &Runtime, native: &Path) -> NodePaths {
    let data = DataPath::of(native).expect("path");
    NodePaths {
        data: DataDir::new(Arc::clone(runtime.disk_io()), data.dir.clone()),
        dir: data.dir,
        native: native.to_owned(),
    }
}

/// Plays the service manager for `machine`: once its service runs, `serve`
/// starts what `alloyd` would.
fn serve_when_started<T, F, Fut>(
    runtime: &Runtime,
    machine: &Arc<dyn Host>,
    serve: F,
) -> alloy_rt::JoinHandle<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
{
    let machine = Arc::clone(machine);
    let clock = runtime.clone();
    runtime
        .spawn(TaskName::new("service").expect("name"), async move {
            loop {
                let status = machine.service_status().await;
                if status.is_ok_and(|s| s.run == RunState::Running) {
                    break;
                }
                clock.sleep(Duration::from_millis(20)).await;
            }
            serve().await
        })
        .expect("spawn")
}

fn free_udp_port(runtime: &Runtime) -> u16 {
    let socket = runtime
        .net()
        .bind_udp(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            UdpOptions::default(),
        )
        .expect("bind");
    socket.local_addr().port()
}

#[test]
fn the_host_check_fails_first_and_says_why() {
    let bench = Bench::new();
    let data = bench.base.join("data");
    let cases: [(MachineConfig, u8, &str); 4] = [
        (
            MachineConfig {
                platform: Some(Platform::Linux {
                    release: "5.15.0".into(),
                    major: 5,
                    minor: 15,
                }),
                ..MachineConfig::default()
            },
            EXIT_FAILURE,
            "  fail  Linux 5.15.0 on x86_64: a node needs Linux 6.1 or later on x86_64 or\n        aarch64\n",
        ),
        (
            MachineConfig {
                arch: Arch::Other("riscv64".into()),
                ..MachineConfig::default()
            },
            EXIT_FAILURE,
            "  fail  Linux 6.1.0 on riscv64: a node needs",
        ),
        (
            MachineConfig {
                service: false,
                ..MachineConfig::default()
            },
            EXIT_FAILURE,
            "  fail  Linux 6.1.0 on x86_64, but the unit alloyd.service is not installed\n",
        ),
        (
            MachineConfig {
                privileged: false,
                ..MachineConfig::default()
            },
            EXIT_DENIED,
            "  ok    Linux 6.1.0 on x86_64\n  fail  not run as root\n",
        ),
    ];
    let env = Env::default();
    let ctx = Ctx {
        runtime: &bench.runtime,
        transport: bench.transport,
        env: &env,
        styles: Styles::default(),
    };
    for (config, code, line) in cases {
        let (machine, _world) = mem_machine(config);
        let setup = Setup {
            ctx: &ctx,
            host: machine.as_ref(),
            data: DataPath::of(&data).unwrap(),
        };
        let mut err = Vec::new();
        let failure = bench
            .host
            .block_on(checks::run(&setup, Kind::Init, 1, 1, &mut err))
            .unwrap_err();
        let err = String::from_utf8(err).unwrap();
        assert_eq!(failure.code, code, "{err}");
        assert!(err.starts_with("Checking this machine\n"), "{err}");
        assert!(err.contains(line), "{err}");
        // Nothing after a failed host or privilege check runs.
        assert!(!err.contains("UDP"), "{err}");
        if code == EXIT_DENIED {
            assert_eq!(failure.try_command.as_deref(), Some("sudo alloy init"));
        } else if line.contains("installed") {
            let install = failure.try_command.unwrap();
            assert!(install.starts_with("curl -fsSL https://"), "{install}");
            assert!(install.ends_with("/install.sh | sh"), "{install}");
        }
    }
    let (machine, _world) = mem_machine(MachineConfig {
        platform: None,
        ..MachineConfig::default()
    });
    let setup = Setup {
        ctx: &ctx,
        host: machine.as_ref(),
        data: DataPath::of(&data).unwrap(),
    };
    let mut err = Vec::new();
    let failure = bench
        .host
        .block_on(checks::run(&setup, Kind::Init, 1, 1, &mut err))
        .unwrap_err();
    assert_eq!(failure.code, EXIT_FAILURE);
    bench.finish();
}

#[test]
fn every_table_check_prints_and_the_first_fail_decides() {
    let bench = Bench::new();
    // A directory under the temporary directory is not one `init` may use:
    // the directories above it are not root's alone.
    let data = bench.base.join("data");
    let env = Env::default();
    let ctx = Ctx {
        runtime: &bench.runtime,
        transport: bench.transport,
        env: &env,
        styles: Styles::default(),
    };
    let (machine, _world) = mem_machine(MachineConfig {
        virtualization: false,
        ..MachineConfig::default()
    });
    let setup = Setup {
        ctx: &ctx,
        host: machine.as_ref(),
        data: DataPath::of(&data).unwrap(),
    };
    let mesh = free_udp_port(&bench.runtime);
    let held = bench
        .runtime
        .net()
        .listen(
            StreamAddr::Ip(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))),
            alloy_rt::ListenOptions::default(),
        )
        .unwrap();
    let StreamAddr::Ip(api) = held.local_addr() else {
        panic!("a TCP listener has an IP address");
    };
    let mut err = Vec::new();
    let failure = bench
        .host
        .block_on(checks::run(&setup, Kind::Init, mesh, api.port(), &mut err))
        .unwrap_err();
    let err = String::from_utf8(err).unwrap();
    assert_eq!(failure.code, EXIT_CONFLICT, "{err}");
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines[0], "Checking this machine");
    assert_eq!(lines[1], "  ok    Linux 6.1.0 on x86_64");
    assert_eq!(
        lines[2..4],
        [
            "  warn  no hardware virtualization (/dev/kvm), so this node takes no compute",
            "        role"
        ]
    );
    // The API port is held by the listener above.
    assert!(lines[4].starts_with("  fail  TCP "), "{err}");
    assert!(lines[4].ends_with(" is in use"), "{err}");
    assert!(lines[5].starts_with("  fail  "), "{err}");
    assert!(err.contains("no name Alloy reserves is taken"), "{err}");
    assert!(
        err.contains(" free in ") || err.contains("free space in"),
        "{err}"
    );
    assert!(
        failure
            .message
            .starts_with("A check of this machine failed, so alloy init changed nothing: TCP "),
        "{}",
        failure.message
    );
    // Nothing was written.
    let (_, dir) = split(&data).unwrap();
    assert!(
        bench
            .host
            .block_on(bench.runtime.disk_io().metadata(&dir))
            .is_err()
    );
    bench.finish();
}

/// `init` after its checks, its node served once the service starts, then
/// `join` of a second machine with the token `init` printed, served as a
/// member node; both end `active`.
#[test]
fn init_then_join_on_loopback() {
    let bench = Bench::new();
    let outcome = bench
        .host
        .block_on(init_then_join(&bench.runtime, bench.transport, &bench.base));
    bench.finish();
    outcome.expect("scenario");
}

async fn init_then_join(
    runtime: &Runtime,
    transport: Transport,
    base: &Path,
) -> Result<(), String> {
    let (control, _control_world) = mem_machine(MachineConfig::default());
    let control_data = base.join("control");
    let (api, api_addr) = listen(runtime);
    let (admin, admin_addr) = listen(runtime);
    let mesh = runtime
        .net()
        .bind_udp(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            UdpOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    let mesh_port = mesh.local_addr().port();
    let listeners = Listeners {
        api,
        admin: Some(Box::new(AsRoot(admin))),
        mesh: vec![mesh],
    };
    let served = {
        let (runtime_c, paths) = (runtime.clone(), paths_of(runtime, &control_data));
        serve_when_started(runtime, &control, move || async move {
            ControlNode::start(&runtime_c, &paths, move |_| Ok(listeners), transport)
                .await
                .map_err(|e| e.to_string())
        })
    };
    let env = env_with_admin(Some(admin_addr));
    let ctx = Ctx {
        runtime,
        transport,
        env: &env,
        styles: Styles::default(),
    };
    let setup = Setup {
        ctx: &ctx,
        host: control.as_ref(),
        data: DataPath::of(&control_data).map_err(|e| e.message)?,
    };
    let init_spec = spec(CommandId::Init);
    let args = crate::setup::init::check(
        init_spec,
        &invocation(&[
            "init",
            "--name",
            "linux-1",
            "--advertise",
            "127.0.0.1",
            "--mesh-port",
            &mesh_port.to_string(),
            "--api-port",
            &api_addr.port().to_string(),
        ]),
    )
    .map_err(|e| e.message)?;
    let passed = Passed {
        facts: control.facts().await.map_err(|e| e.to_string())?,
        action: InitAction::Create,
        state: JoinState::Unjoined,
        cluster_id: None,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    crate::setup::init::after_checks(&setup, init_spec, &args, &passed, &mut out, &mut err)
        .await
        .map_err(|f| format!("{f:?}\n{}", String::from_utf8_lossy(&err)))?;
    let node = served.join().await.map_err(|e| e.to_string())??;
    let out = String::from_utf8(out).map_err(|e| e.to_string())?;
    let err = String::from_utf8(err).map_err(|e| e.to_string())?;
    let result = check_init_output(&out, &err, control.as_ref()).await;
    let token = out
        .split_whitespace()
        .find(|word| word.starts_with("alyt_"))
        .unwrap_or_default()
        .to_owned();
    let result = match result {
        Ok(()) => join_second(runtime, transport, base, &token, admin_addr).await,
        Err(e) => Err(e),
    };
    node.stop().await.map_err(|e| e.to_string())?;
    result
}

async fn check_init_output(out: &str, err: &str, control: &dyn Host) -> Result<(), String> {
    let expect = |ok: bool, what: &str| {
        if ok {
            Ok(())
        } else {
            Err(format!("{what}\n{out}\n{err}"))
        }
    };
    expect(
        out.contains("\n  sudo alloy join alyt_"),
        "the join token on standard output",
    )?;
    expect(
        out.contains("\n  alloy login alyc_"),
        "the login code on standard output",
    )?;
    expect(
        !err.contains("alyt_") && !err.contains("alyc_"),
        "no secret on standard error",
    )?;
    expect(
        err.contains("Advertise address: 127.0.0.1 (given with --advertise).\n"),
        "advertise",
    )?;
    expect(err.contains("Creating the cluster\n"), "heading")?;
    expect(
        err.contains("  done  node linux-1: anchor, control, compute, storage\n"),
        "node",
    )?;
    expect(
        err.contains("  done  alloyd enabled and started\n"),
        "started",
    )?;
    expect(
        err.contains("  ok    node linux-1 is active, API answers at 127.0.0.1:"),
        "ready",
    )?;
    let status = control.service_status().await.map_err(|e| e.to_string())?;
    expect(
        status
            == ServiceStatus {
                enabled: true,
                run: RunState::Running,
            },
        "alloyd enabled and running",
    )
}

async fn join_second(
    runtime: &Runtime,
    transport: Transport,
    base: &Path,
    token: &str,
    admin_addr: SocketAddr,
) -> Result<(), String> {
    let (joiner, _joiner_world) = mem_machine(MachineConfig::default());
    let data = base.join("joiner");
    let mesh_port = free_udp_port(runtime);
    let served = {
        let (runtime_c, paths) = (runtime.clone(), paths_of(runtime, &data));
        serve_when_started(runtime, &joiner, move || async move {
            let bind_rt = runtime_c.clone();
            MemberNode::start(&runtime_c, &paths, move |record| {
                bind_rt
                    .net()
                    .bind_udp(
                        SocketAddr::from((Ipv4Addr::UNSPECIFIED, record.mesh_port)),
                        UdpOptions::default(),
                    )
                    .map(|socket| vec![socket])
                    .map_err(NodeError::Listen)
            })
            .await
            .map_err(|e| e.to_string())
        })
    };
    let env = Env::default();
    let ctx = Ctx {
        runtime,
        transport,
        env: &env,
        styles: Styles::default(),
    };
    let setup = Setup {
        ctx: &ctx,
        host: joiner.as_ref(),
        data: DataPath::of(&data).map_err(|e| e.message)?,
    };
    let join_spec = spec(CommandId::Join);
    let given = invocation(&[
        "join",
        token,
        "--name",
        "linux-2",
        "--advertise",
        "127.0.0.1",
        "--mesh-port",
        &mesh_port.to_string(),
    ]);
    // On Windows the node is a client node: a role is a usage error found
    // before any machine is contacted.
    let with_role = invocation(&["join", token, "--roles", "compute"]);
    let refused = crate::setup::join::check(join_spec, &with_role, true).map(|_| ());
    if refused.is_ok() {
        return Err("a Windows join with a role was accepted".to_owned());
    }
    let args = crate::setup::join::check(join_spec, &given, false).map_err(|e| e.message)?;
    let passed = Passed {
        facts: joiner.facts().await.map_err(|e| e.to_string())?,
        action: InitAction::Create,
        state: JoinState::Unjoined,
        cluster_id: None,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    crate::setup::join::after_checks(&setup, join_spec, &args, &passed, &mut out, &mut err)
        .await
        .map_err(|f| format!("{f:?}\n{}", String::from_utf8_lossy(&err)))?;
    let member = served.join().await.map_err(|e| e.to_string())??;
    let out = String::from_utf8_lossy(&out).into_owned();
    let err = String::from_utf8_lossy(&err).into_owned();
    let result = async {
        if out != "Joined as node linux-2, roles compute,storage, zone default.\n" {
            return Err(format!("join printed {out:?}\n{err}"));
        }
        if !err.contains("  done  alloyd enabled and started\n") || err.contains(token) {
            return Err(err.clone());
        }
        wait_active(runtime, transport, admin_addr, &["linux-1", "linux-2"]).await
    }
    .await;
    member.stop().await;
    result
}

/// Waits up to 30 seconds for every node in `names` to be `active`, as the
/// local admin socket at `admin` lists them.
async fn wait_active(
    runtime: &Runtime,
    transport: Transport,
    admin: SocketAddr,
    names: &[&str],
) -> Result<(), String> {
    let env = env_with_admin(Some(admin));
    let ctx = Ctx {
        runtime,
        transport,
        env: &env,
        styles: Styles::default(),
    };
    let mut last = String::new();
    for _ in 0..120 {
        if let Ok(client) = login::local_admin(&ctx).await
            && let Ok(nodes) = client.list_all_nodes().await
        {
            let active = |name: &str| {
                nodes.iter().any(|node| {
                    node.name == name
                        && node
                            .status
                            .as_ref()
                            .is_some_and(|s| s.state == i32::from(NodeState::Active))
                })
            };
            if names.iter().all(|name| active(name)) {
                return Ok(());
            }
            last = format!(
                "{:?}",
                nodes
                    .iter()
                    .map(|n| (&n.name, n.status.as_ref().map(|s| s.state)))
                    .collect::<Vec<_>>()
            );
        }
        runtime.sleep(Duration::from_millis(250)).await;
    }
    Err(format!("not every node became active: {last}"))
}

/// A failed step 4 leaves `alloyd` as a first install leaves it and the
/// data directory with only its host file, `unjoined`.
#[test]
fn a_failed_start_cleans_up() {
    let bench = Bench::new();
    let data = bench.base.join("data");
    // The process is not privileged, so `Enable` is refused; the checks,
    // which would refuse first, are not run here.
    let (machine, _world) = mem_machine(MachineConfig {
        privileged: false,
        ..MachineConfig::default()
    });
    let env = Env::default();
    let ctx = Ctx {
        runtime: &bench.runtime,
        transport: bench.transport,
        env: &env,
        styles: Styles::default(),
    };
    let setup = Setup {
        ctx: &ctx,
        host: machine.as_ref(),
        data: DataPath::of(&data).unwrap(),
    };
    let init_spec = spec(CommandId::Init);
    let args = crate::setup::init::check(
        init_spec,
        &invocation(&[
            "init",
            "--name",
            "linux-1",
            "--advertise",
            "127.0.0.1",
            "--mesh-port",
            "7999",
            "--api-port",
            "8999",
        ]),
    )
    .unwrap();
    let passed = Passed {
        facts: bench.host.block_on(machine.facts()).unwrap(),
        action: InitAction::Create,
        state: JoinState::Unjoined,
        cluster_id: None,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let failure = bench
        .host
        .block_on(crate::setup::init::after_checks(
            &setup, init_spec, &args, &passed, &mut out, &mut err,
        ))
        .unwrap_err();
    assert_eq!(failure.code, EXIT_DENIED, "{failure:?}");
    assert!(
        failure
            .message
            .contains("could not enable the service alloyd"),
        "{failure:?}"
    );
    assert!(out.is_empty());
    let (_, dir) = split(&data).unwrap();
    let disk = bench.runtime.disk_io();
    let mut names: Vec<String> = bench
        .host
        .block_on(disk.list(&dir))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    assert_eq!(names, ["alloy-host"]);
    let head = bench
        .host
        .block_on(paths_of(&bench.runtime, &data).data.read("alloy-host"))
        .unwrap()
        .unwrap();
    assert_eq!(alloyd::parse_host_file(&head), Ok(JoinState::Unjoined));
    bench.finish();
}

/// A join whose attempt fails, here because no anchor answers, runs with
/// `alloyd` already enabled (ARFC 0002 section 3.3 step 2) and leaves it
/// stopped and disabled, with the local join state `unjoined` (section 6).
#[test]
fn a_failed_join_leaves_alloyd_disabled() {
    let bench = Bench::new();
    let outcome = bench
        .host
        .block_on(failed_join(&bench.runtime, bench.transport, &bench.base));
    bench.finish();
    outcome.expect("scenario");
}

async fn failed_join(runtime: &Runtime, transport: Transport, base: &Path) -> Result<(), String> {
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicBool, Ordering};

    use alloy_crypto::al;
    use alloy_id::token::{TokenAddress, TokenAnchor, TokenId, TokenSecret};
    use alloy_id::{ClusterId, ClusterKeyFp, JoinToken, Roles};

    let (machine, _world) = mem_machine(MachineConfig::default());
    let data = base.join("joiner");
    // Nothing listens on the anchor's port.
    let dead = free_udp_port(runtime);
    let token = JoinToken {
        cluster_id: ClusterId::from_bytes([7; 16]),
        cluster_key_fp: ClusterKeyFp::from_bytes([8; 16]),
        token_id: TokenId([9; 8]),
        secret: TokenSecret::from_bytes([10; 16]),
        preset_roles: Roles::NONE,
        anchors: vec![TokenAnchor {
            mesh_pub: al::PublicKey::from_bytes([11; 32]),
            addresses: vec![TokenAddress {
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                mesh_port: dead,
                stream_port: dead,
            }],
        }],
    }
    .encode()
    .map_err(|e| e.to_string())?;
    let seen_enabled = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (machine, seen, done, clock) = (
            Arc::clone(&machine),
            Arc::clone(&seen_enabled),
            Arc::clone(&done),
            runtime.clone(),
        );
        runtime
            .spawn(TaskName::new("watch").expect("name"), async move {
                while !done.load(Ordering::SeqCst) {
                    if machine.service_status().await.is_ok_and(|s| s.enabled) {
                        seen.store(true, Ordering::SeqCst);
                    }
                    clock.sleep(Duration::from_millis(10)).await;
                }
            })
            .expect("spawn")
    };
    let env = Env::default();
    let ctx = Ctx {
        runtime,
        transport,
        env: &env,
        styles: Styles::default(),
    };
    let setup = Setup {
        ctx: &ctx,
        host: machine.as_ref(),
        data: DataPath::of(&data).map_err(|e| e.message)?,
    };
    let join_spec = spec(CommandId::Join);
    let given = invocation(&[
        "join",
        &token,
        "--name",
        "linux-3",
        "--advertise",
        "127.0.0.1",
        "--mesh-port",
        &free_udp_port(runtime).to_string(),
    ]);
    let args = crate::setup::join::check(join_spec, &given, false).map_err(|e| e.message)?;
    let passed = Passed {
        facts: machine.facts().await.map_err(|e| e.to_string())?,
        action: InitAction::Create,
        state: JoinState::Unjoined,
        cluster_id: None,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let outcome =
        crate::setup::join::after_checks(&setup, join_spec, &args, &passed, &mut out, &mut err)
            .await;
    done.store(true, Ordering::SeqCst);
    watcher.join().await.map_err(|e| e.to_string())?;
    let err = String::from_utf8_lossy(&err).into_owned();
    let Err(failure) = outcome else {
        return Err(format!("a join with no anchor answering succeeded\n{err}"));
    };
    if ![EXIT_FAILURE, EXIT_UNAVAILABLE].contains(&failure.code) {
        return Err(format!("{failure:?}\n{err}"));
    }
    if !seen_enabled.load(Ordering::SeqCst) {
        return Err("alloyd was not enabled while the attempt ran".to_owned());
    }
    let status = machine.service_status().await.map_err(|e| e.to_string())?;
    let first_install = ServiceStatus {
        enabled: false,
        run: RunState::Stopped,
    };
    if status != first_install {
        return Err(format!("after a failed join alloyd is {status:?}"));
    }
    if !out.is_empty() || err.contains(token.as_str()) || failure.message.contains(&token) {
        return Err(format!("a result or the token was printed\n{err}"));
    }
    let head = paths_of(runtime, &data)
        .data
        .read("alloy-host")
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no host file")?;
    match alloyd::parse_host_file(&head) {
        Ok(JoinState::Unjoined) => Ok(()),
        other => Err(format!("the local join state is {other:?}")),
    }
}

/// `init`'s `addresses` check draws a `cluster_id` and passes it on when
/// its prefix overlaps nothing, and fails only once all 16 draws overlap
/// (ARFC 0002 section 3.8).
#[test]
fn init_draws_a_cluster_id_that_overlaps_nothing() {
    use alloy_rt::{Interface, InterfaceAddr};

    use crate::setup::Status;
    use crate::setup::checks::draw_cluster_id;
    use crate::setup::probe::Route;

    let bench = Bench::new();
    let rng = bench.runtime.rng().as_ref();
    let (line, id) = draw_cluster_id(rng, &[], &[]);
    assert_eq!(line.status, Status::Ok, "{line:?}");
    let id = id.expect("an id");
    assert_eq!(
        line.text,
        format!("{}/48 overlaps no route or address", id.infra_prefix())
    );
    // Every infrastructure prefix is inside fd00::/8.
    let route = Route {
        dest: "fd00::".parse().unwrap(),
        len: 8,
        iface: "eth0".to_owned(),
        usable: true,
    };
    let (line, id) = draw_cluster_id(rng, &[route], &[]);
    assert_eq!(
        (line.status, line.code, id),
        (Status::Fail, EXIT_CONFLICT, None)
    );
    assert!(
        line.text
            .starts_with("the prefix of each of 16 cluster ids drawn overlaps a route or an"),
        "{}",
        line.text
    );
    assert!(
        line.text.ends_with("overlaps the route fd00::/8 on eth0"),
        "{}",
        line.text
    );
    let interface = Interface {
        index: 3,
        name: "eth1".to_owned(),
        mtu: 1_500,
        up: true,
        loopback: false,
        addrs: vec![InterfaceAddr {
            ip: "fd00::1".parse().unwrap(),
            prefix_len: 8,
        }],
    };
    let (line, id) = draw_cluster_id(rng, &[], &[interface]);
    assert_eq!((line.status, id), (Status::Fail, None));
    assert!(
        line.text.ends_with("the address fd00::1/8 on eth1"),
        "{}",
        line.text
    );
    bench.finish();
}
