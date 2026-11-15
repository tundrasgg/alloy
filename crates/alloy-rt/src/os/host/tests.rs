//! Tests of `host` under `os` on the machine that runs them, and of the
//! service rules of clause 3.1.18.3 against a scripted service manager.

use std::sync::Mutex;
use std::time::Duration;

use alloy_seam::{ConfigValue, ImplName, Registry, RegistryBuilder};

use super::*;
use crate::os::register_os;
use crate::{Arch, Host, HostSeam, OsHost, Platform, PortHolder};

fn machine(host: &OsHost) -> Arc<dyn Host> {
    let mut builder = RegistryBuilder::new();
    register_os(&mut builder, host).unwrap();
    let registry: Registry = builder.build();
    let os = ImplName::new("os").unwrap();
    let empty = ConfigValue::defaults(1).unwrap();
    registry.instantiate::<HostSeam>(&(), &os, &empty).unwrap()
}

fn with_machine(test: impl AsyncFnOnce(&dyn Host)) {
    let host = OsHost::start(2).unwrap();
    let machine = machine(&host);
    host.block_on(test(machine.as_ref()));
    host.shutdown(Duration::from_secs(1));
}

#[test]
fn facts_describe_this_machine() {
    with_machine(async |machine| {
        let facts = machine.facts().await.unwrap();
        assert!(facts.cores >= 1);
        assert!(facts.memory > 0);
        match &facts.platform {
            Platform::Linux { release, .. } => {
                assert!(!release.is_empty() && release.len() <= 64);
            }
            Platform::Windows { major, build, .. } => {
                assert!(*major >= 10 && *build > 0);
            }
        }
        let linux = matches!(facts.platform, Platform::Linux { .. });
        assert_eq!(linux, cfg!(target_os = "linux"));
        let expected = if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        };
        // A process may run emulated, so only a native build is checked.
        if let Some(arch) = expected
            && cfg!(target_os = "linux")
        {
            assert_eq!(facts.arch, arch);
        }
        assert!(facts.disks.len() <= 32);
        assert!(facts.disks.windows(2).all(|w| w[0].path < w[1].path));
        if cfg!(windows) {
            assert!(facts.disks.is_empty());
        }
        assert_eq!(machine.facts().await.unwrap().cores, facts.cores);
    });
}

#[test]
fn free_space_reads_the_nearest_existing_ancestor() {
    let temp = std::env::temp_dir();
    let temp = temp.to_str().unwrap().to_owned();
    let separator = if cfg!(windows) { '\\' } else { '/' };
    let missing = format!(
        "{}{separator}alloy-no-such-dir{separator}below",
        temp.trim_end_matches(separator)
    );
    with_machine(async |machine| {
        let here = machine.free_space(&temp).await.unwrap();
        let below = machine.free_space(&missing).await.unwrap();
        assert!(here.abs_diff(below) <= 16_777_216, "{here} {below}");
        for bad in ["relative", "", "x\0y"] {
            assert!(matches!(
                machine.free_space(bad).await,
                Err(HostError::InvalidInput(_))
            ));
        }
        let long = format!("{}{}", &temp[..3.min(temp.len())], "a".repeat(4_100));
        assert!(matches!(
            machine.free_space(&long).await,
            Err(HostError::InvalidInput(_))
        ));
    });
}

/// The name `port_holders` gives this process (clause 3.1.18.5).
fn own_name() -> String {
    if cfg!(windows) {
        let exe = std::env::current_exe().unwrap();
        exe.file_name().unwrap().to_str().unwrap().to_owned()
    } else {
        let comm = std::fs::read_to_string("/proc/self/comm").unwrap();
        comm.strip_suffix('\n').unwrap_or(&comm).to_owned()
    }
}

/// Whether `holders` holds an entry for `local` with this process's pid,
/// and if so, whether it names this process.
fn own_entry(holders: &[PortHolder], local: std::net::SocketAddr) -> Option<bool> {
    let pid = std::process::id();
    holders
        .iter()
        .find(|h| h.local == local && h.pid == Some(pid))
        .map(|h| h.process.as_deref() == Some(own_name().as_str()))
}

/// Binds only on loopback with port 0, so that no firewall prompts. A
/// connected stream and an accepted one are not listeners, so their ports
/// give no entry with this pid.
#[test]
fn port_holders_find_this_process_on_loopback() {
    use std::net::{TcpListener, TcpStream, UdpSocket};

    use crate::PortProtocol::{Tcp, Udp};

    if !cfg!(any(target_os = "linux", windows)) {
        with_machine(async |machine| {
            assert_eq!(
                machine.port_holders(Udp, 7476).await,
                Err(HostError::Unsupported)
            );
        });
        return;
    }
    let udp4 = UdpSocket::bind("127.0.0.1:0").unwrap();
    let udp6 = UdpSocket::bind("[::1]:0").ok();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let listen6 = TcpListener::bind("[::1]:0").ok();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    with_machine(async |machine| {
        let mut owned = vec![(Udp, udp4.local_addr().unwrap())];
        owned.push((Tcp, listener.local_addr().unwrap()));
        owned.extend(udp6.iter().map(|s| (Udp, s.local_addr().unwrap())));
        owned.extend(listen6.iter().map(|s| (Tcp, s.local_addr().unwrap())));
        for (protocol, local) in owned {
            let holders = machine.port_holders(protocol, local.port()).await.unwrap();
            assert!(holders.iter().all(|h| h.local.port() == local.port()));
            assert!(holders.windows(2).all(|w| w[0] < w[1]), "{holders:?}");
            assert_eq!(
                own_entry(&holders, local),
                Some(true),
                "{protocol:?} {local}: {holders:?}"
            );
        }
        let client = stream.local_addr().unwrap();
        let holders = machine.port_holders(Tcp, client.port()).await.unwrap();
        assert_eq!(own_entry(&holders, client), None, "{holders:?}");
        assert!(matches!(
            machine.port_holders(Tcp, 0).await,
            Err(HostError::InvalidInput(_))
        ));
        assert!(matches!(
            machine.port_holders(Udp, 0).await,
            Err(HostError::InvalidInput(_))
        ));
    });
    let served = listener.local_addr().unwrap();
    drop(listener);
    with_machine(async |machine| {
        let holders = machine.port_holders(Tcp, served.port()).await.unwrap();
        assert_eq!(own_entry(&holders, served), None, "{holders:?}");
    });
    drop((stream, accepted));
}

#[test]
fn service_status_reads_without_privilege() {
    with_machine(async |machine| {
        let status = machine.service_status().await;
        assert!(
            matches!(
                status,
                Ok(_)
                    | Err(HostError::NotInstalled
                        | HostError::Unsupported
                        | HostError::PermissionDenied)
            ),
            "{status:?}"
        );
    });
}

/// The service actions, as ARFC 0005 section 5 says outside the CI hosts:
/// with `ALLOY_TEST_SERVICE` set to `1` the whole cycle on the test service,
/// unprivileged each action's `PermissionDenied`, and otherwise none.
#[test]
fn service_actions_follow_the_privilege_rule() {
    let test_service = std::env::var("ALLOY_TEST_SERVICE").is_ok_and(|v| v == "1");
    with_machine(async |machine| {
        let privileged = machine.facts().await.unwrap().privileged;
        if test_service {
            for action in [
                ServiceAction::Disable,
                ServiceAction::Stop,
                ServiceAction::Enable,
                ServiceAction::Start,
            ] {
                machine.service(action).await.unwrap();
            }
            let status = machine.service_status().await.unwrap();
            assert!(status.enabled && status.run == RunState::Running);
            machine.service(ServiceAction::Stop).await.unwrap();
            machine.service(ServiceAction::Disable).await.unwrap();
            assert_eq!(
                machine.service(ServiceAction::Start).await,
                Err(HostError::Disabled)
            );
            println!("host: ran the service cycle on the test service");
        } else if !privileged {
            for action in [
                ServiceAction::Enable,
                ServiceAction::Disable,
                ServiceAction::Start,
                ServiceAction::Stop,
            ] {
                assert_eq!(
                    machine.service(action).await,
                    Err(HostError::PermissionDenied)
                );
            }
            println!("host: checked the PermissionDenied refusal of each action");
        } else {
            println!("host: privileged without ALLOY_TEST_SERVICE, so no action ran");
        }
    });
}

/// A scripted service manager: a state, and the requests it was sent. One
/// test uses it, since tests run in parallel.
static SCRIPT: Mutex<Script> = Mutex::new(Script {
    privileged: true,
    status: None,
    after_start: RunState::Running,
    requests: Vec::new(),
});

struct Script {
    privileged: bool,
    status: Option<ServiceStatus>,
    after_start: RunState,
    requests: Vec<ServiceAction>,
}

struct Scripted;

impl Manager for Scripted {
    const POLLS_START: bool = true;

    fn privileged() -> bool {
        SCRIPT.lock().unwrap().privileged
    }

    fn status(_deadline: Instant) -> Result<ServiceStatus, HostError> {
        SCRIPT.lock().unwrap().status.ok_or(HostError::NotInstalled)
    }

    fn request(action: ServiceAction, _deadline: Instant) -> Result<(), HostError> {
        let mut script = SCRIPT.lock().unwrap();
        script.requests.push(action);
        let after_start = script.after_start;
        if let Some(status) = &mut script.status {
            match action {
                ServiceAction::Enable => status.enabled = true,
                ServiceAction::Disable => status.enabled = false,
                ServiceAction::Start => status.run = after_start,
                ServiceAction::Stop => status.run = RunState::Stopped,
            }
        }
        Ok(())
    }

    fn stop_reason() -> Option<String> {
        Some("exit code 3".to_owned())
    }
}

fn scripted(
    privileged: bool,
    status: Option<ServiceStatus>,
    action: ServiceAction,
) -> (Result<(), HostError>, Vec<ServiceAction>) {
    {
        let mut script = SCRIPT.lock().unwrap();
        script.privileged = privileged;
        script.status = status;
        script.after_start = RunState::Running;
        script.requests.clear();
    }
    let result = act_with::<Scripted>(action, Instant::now() + Duration::from_secs(1));
    (result, std::mem::take(&mut SCRIPT.lock().unwrap().requests))
}

#[test]
fn service_rules_follow_clause_3_1_18_3() {
    use RunState::{Running, Stopped};
    use ServiceAction::{Disable, Enable, Start, Stop};
    let state = |enabled, run| Some(ServiceStatus { enabled, run });
    // Privilege first, then installation.
    assert_eq!(
        scripted(false, None, Enable),
        (Err(HostError::PermissionDenied), vec![])
    );
    assert_eq!(
        scripted(true, None, Stop),
        (Err(HostError::NotInstalled), vec![])
    );
    // Enabling an enabled service, disabling a disabled one, starting a
    // running one, and stopping a stopped one change nothing.
    assert_eq!(
        scripted(true, state(true, Stopped), Enable),
        (Ok(()), vec![])
    );
    assert_eq!(
        scripted(true, state(false, Running), Disable),
        (Ok(()), vec![])
    );
    assert_eq!(
        scripted(true, state(true, Running), Start),
        (Ok(()), vec![])
    );
    assert_eq!(
        scripted(true, state(false, Stopped), Stop),
        (Ok(()), vec![])
    );
    // A disabled service does not start.
    assert_eq!(
        scripted(true, state(false, Stopped), Start),
        (Err(HostError::Disabled), vec![])
    );
    // The others ask the service manager.
    assert_eq!(
        scripted(true, state(false, Stopped), Enable),
        (Ok(()), vec![Enable])
    );
    assert_eq!(
        scripted(true, state(true, Running), Disable),
        (Ok(()), vec![Disable])
    );
    assert_eq!(
        scripted(true, state(true, Stopped), Start),
        (Ok(()), vec![Start])
    );
    assert_eq!(
        scripted(true, state(true, Running), Stop),
        (Ok(()), vec![Stop])
    );
    // In the same test, since the script is shared: a start that stops is
    // refused, and one that hangs times out.
    {
        let mut script = SCRIPT.lock().unwrap();
        script.privileged = true;
        script.status = Some(ServiceStatus {
            enabled: true,
            run: Stopped,
        });
        script.after_start = Stopped;
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    match act_with::<Scripted>(Start, deadline) {
        Err(HostError::Refused(reason)) => assert_eq!(reason.as_str(), "exit code 3"),
        other => panic!("{other:?}"),
    }
    {
        let mut script = SCRIPT.lock().unwrap();
        script.status = Some(ServiceStatus {
            enabled: true,
            run: Stopped,
        });
        script.after_start = RunState::Starting;
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    assert_eq!(
        act_with::<Scripted>(Start, deadline),
        Err(HostError::TimedOut)
    );
}
