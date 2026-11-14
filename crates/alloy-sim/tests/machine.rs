//! The `mem` instance of `host` through the public interface: facts from
//! `MachineConfig`, free space, the service rules, the service state across
//! a crash and a restart, the `service` record, and the registry's factory
//! (ARFC 0005 clauses 3.1.18.1 to 3.1.18.3 and section 4.1).

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_rt::{
    Access, Arch, BoxFuture, Create, DiskFact, Host, HostError, HostSeam, OpenOptions, Platform,
    RtPath, RunState, ServiceAction, ServiceStatus,
};
use alloy_seam::{ConfigValue, Detail, ImplName};
use alloy_sim::{
    Boot, DeviceConfig, Failure, Fault, HostBoot, HostConfig, MachineConfig, Observer, Record,
    RunConfig, RunResult, Unsynced, World, WorldError, run,
};

const SERVICE: u16 = 0x0080;

type Shared<T> = Arc<Mutex<T>>;

fn boot<F, Fut>(f: F) -> Boot
where
    F: Fn(HostBoot) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |b| -> BoxFuture<'static, ()> { Box::pin(f(b)) })
}

fn fail(error: WorldError) -> Failure {
    Failure(Detail::new(&error.to_string()))
}

fn check(ok: bool, what: &str) -> Result<(), Failure> {
    if ok {
        Ok(())
    } else {
        Err(Failure(Detail::new(what)))
    }
}

#[derive(Clone, Default)]
struct Services(Shared<Vec<(Option<u32>, Vec<u8>)>>);

impl Observer for Services {
    fn record(&mut self, record: &Record<'_>) {
        if record.kind == SERVICE {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((record.host.map(|h| h.0), record.body.to_vec()));
        }
    }
}

/// The machine of each start, and the service state its `boot` saw.
type Boots = Shared<Vec<(Arc<dyn Host>, ServiceStatus)>>;

fn recording_host(boots: &Boots) -> HostConfig {
    let kept = Arc::clone(boots);
    HostConfig::new(boot(move |b: HostBoot| {
        let kept = Arc::clone(&kept);
        async move {
            let status = b.machine.service_status().await.expect("installed");
            kept.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((Arc::clone(&b.machine), status));
            std::future::pending::<()>().await;
        }
    }))
}

fn machine_of(boots: &Boots, start: usize) -> Arc<dyn Host> {
    Arc::clone(&boots.lock().unwrap_or_else(PoisonError::into_inner)[start].0)
}

#[test]
fn facts_come_from_the_machine_config_and_the_devices() {
    let summary = run(RunConfig::new(1), |world: World| async move {
        let facts: Shared<Option<_>> = Arc::default();
        let kept = Arc::clone(&facts);
        let mut config = HostConfig::new(boot(move |b: HostBoot| {
            let kept = Arc::clone(&kept);
            async move {
                let got = b.machine.facts().await;
                *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(got);
            }
        }));
        let device = |path: &str, len| DeviceConfig {
            path: RtPath::new(path).expect("a valid path"),
            len,
            block_size: 4_096,
        };
        check(
            config.machine == MachineConfig::default(),
            "default machine",
        )?;
        config.devices = vec![device("/dev/vdb", 8_192), device("/dev/vda", 4_096)];
        world.add_host(config).map_err(fail)?;
        world.sleep(Duration::from_secs(1)).await;
        let got = facts.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let facts = got.expect("boot ran").expect("facts");
        check(
            facts.platform
                == Platform::Linux {
                    release: "6.1.0".into(),
                    major: 6,
                    minor: 1,
                },
            "platform",
        )?;
        check(facts.arch == Arch::X86_64 && facts.privileged, "arch")?;
        check(facts.cores == 4 && facts.memory == 8_589_934_592, "size")?;
        check(facts.virtualization, "virtualization")?;
        let disks = vec![
            DiskFact {
                path: "/dev/vda".into(),
                size: 4_096,
            },
            DiskFact {
                path: "/dev/vdb".into(),
                size: 8_192,
            },
        ];
        check(facts.disks == disks, "disks sorted by path")?;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
}

#[test]
fn a_machine_config_out_of_range_is_refused_and_none_is_unsupported() {
    let summary = run(RunConfig::new(2), |world: World| async move {
        let idle = || HostConfig::new(boot(|_| async {}));
        let mut zero = idle();
        zero.machine.cores = 0;
        check(
            matches!(world.add_host(zero), Err(WorldError::Invalid(_))),
            "cores 0",
        )?;
        let mut long = idle();
        long.machine.platform = Some(Platform::Linux {
            release: "6".repeat(65),
            major: 0,
            minor: 0,
        });
        check(
            matches!(world.add_host(long), Err(WorldError::Invalid(_))),
            "release",
        )?;
        for text in [String::new(), "a".repeat(33)] {
            let mut arch = idle();
            arch.machine.arch = Arch::Other(text);
            check(
                matches!(world.add_host(arch), Err(WorldError::Invalid(_))),
                "arch",
            )?;
        }
        let unsupported: Shared<Option<_>> = Arc::default();
        let kept = Arc::clone(&unsupported);
        let mut none = HostConfig::new(boot(move |b: HostBoot| {
            let kept = Arc::clone(&kept);
            async move {
                let got = b.machine.facts().await.map(|_| ());
                *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(got);
            }
        }));
        none.machine.platform = None;
        world.add_host(none).map_err(fail)?;
        world.sleep(Duration::from_secs(1)).await;
        let got = unsupported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        check(got == Some(Err(HostError::Unsupported)), "unsupported")?;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
}

#[test]
fn service_rules_and_state_across_a_crash_and_a_restart() {
    let services = Services::default();
    let seen = services.clone();
    let summary = run(RunConfig::new(3), move |world: World| async move {
        world.observe(Box::new(seen)).map_err(fail)?;
        let boots: Boots = Arc::default();
        let host = world.add_host(recording_host(&boots)).map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        let machine = machine_of(&boots, 0);
        let stopped = ServiceStatus {
            enabled: false,
            run: RunState::Stopped,
        };
        check(boots.lock().unwrap()[0].1 == stopped, "first install state")?;
        check(
            machine.service(ServiceAction::Start).await == Err(HostError::Disabled),
            "a disabled service does not start",
        )?;
        machine
            .service(ServiceAction::Enable)
            .await
            .map_err(host_fail)?;
        machine
            .service(ServiceAction::Start)
            .await
            .map_err(host_fail)?;
        machine
            .service(ServiceAction::Start)
            .await
            .map_err(host_fail)?;
        let running = ServiceStatus {
            enabled: true,
            run: RunState::Running,
        };
        check(machine.service_status().await == Ok(running), "running")?;
        world
            .apply(Fault::Crash {
                host,
                after_polls: 0,
                unsynced: Unsynced::Lost,
            })
            .map_err(fail)?;
        let after_crash = ServiceStatus {
            enabled: true,
            run: RunState::Stopped,
        };
        check(
            machine.service_status().await == Ok(after_crash),
            "crash stops it",
        )?;
        world
            .apply(Fault::Restart {
                host,
                rng_replay: None,
            })
            .map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        check(
            boots.lock().unwrap()[1].1 == running,
            "an enabled service runs at boot",
        )?;
        check(
            Arc::ptr_eq(&machine_of(&boots, 1), &machine),
            "one instance per host",
        )?;
        machine
            .service(ServiceAction::Stop)
            .await
            .map_err(host_fail)?;
        machine
            .service(ServiceAction::Disable)
            .await
            .map_err(host_fail)?;
        check(
            machine.service_status().await == Ok(stopped),
            "as first installed",
        )?;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
    let records = services.0.lock().unwrap().clone();
    let body = |action: u8, result: u8| (Some(0), vec![action, result]);
    assert_eq!(
        records,
        vec![
            body(3, 3),
            body(1, 0),
            body(3, 0),
            body(3, 0),
            body(4, 0),
            body(2, 0)
        ]
    );
}

fn host_fail(error: HostError) -> Failure {
    Failure(Detail::new(&error.to_string()))
}

#[test]
fn privilege_and_installation_are_checked_first() {
    let summary = run(RunConfig::new(4), |world: World| async move {
        let results: Shared<Vec<Result<(), HostError>>> = Arc::default();
        for (privileged, service) in [(false, true), (true, false), (false, false)] {
            let kept = Arc::clone(&results);
            let mut config = HostConfig::new(boot(move |b: HostBoot| {
                let kept = Arc::clone(&kept);
                async move {
                    let got = b.machine.service(ServiceAction::Enable).await;
                    kept.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(got);
                    let status = b.machine.service_status().await.map(|_| ());
                    kept.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(status);
                }
            }));
            config.machine.privileged = privileged;
            config.machine.service = service;
            world.add_host(config).map_err(fail)?;
            world.sleep(Duration::from_millis(1)).await;
        }
        let got = results.lock().unwrap().clone();
        check(
            got == vec![
                Err(HostError::PermissionDenied),
                Ok(()),
                Err(HostError::NotInstalled),
                Err(HostError::NotInstalled),
                Err(HostError::PermissionDenied),
                Err(HostError::NotInstalled),
            ],
            "order of the checks",
        )
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
}

#[test]
fn free_space_is_the_capacity_less_what_files_hold() {
    let summary = run(RunConfig::new(5), |world: World| async move {
        let seen: Shared<Vec<Result<u64, HostError>>> = Arc::default();
        let handle: Shared<Option<Arc<dyn Host>>> = Arc::default();
        let (kept, held) = (Arc::clone(&seen), Arc::clone(&handle));
        let mut config = HostConfig::new(boot(move |b: HostBoot| {
            let (kept, held) = (Arc::clone(&kept), Arc::clone(&held));
            async move {
                let push = |got| {
                    kept.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(got)
                };
                push(b.machine.free_space("/no/such/dir").await);
                let path = RtPath::new("/f").expect("valid");
                let file = b
                    .runtime
                    .disk_io()
                    .open(
                        &path,
                        OpenOptions {
                            access: Access::ReadWrite,
                            create: Create::New,
                            direct: false,
                            exclusive: false,
                        },
                    )
                    .await
                    .expect("created");
                file.truncate(10_000).await.expect("grown");
                push(b.machine.free_space("/").await);
                push(b.machine.free_space("relative").await);
                *held.lock().unwrap_or_else(PoisonError::into_inner) = Some(b.machine);
            }
        }));
        config.capacity = 1_048_576;
        let host = world.add_host(config).map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        let got = seen.lock().unwrap().clone();
        check(got.len() == 3, "three reads")?;
        check(got[0] == Ok(1_048_576), "empty")?;
        check(
            got[1] == Ok(1_048_576 - 12_288),
            "after 10,000 bytes in 4 KiB blocks",
        )?;
        check(
            matches!(got[2], Err(HostError::InvalidInput(_))),
            "relative",
        )?;
        let machine = handle.lock().unwrap().clone().expect("boot ran");
        world
            .apply(Fault::Capacity {
                host,
                bytes: 20_480,
            })
            .map_err(fail)?;
        check(
            machine.free_space("/").await == Ok(8_192),
            "a lower capacity",
        )?;
        // At or below the bytes in use, the disk has no room.
        world
            .apply(Fault::Capacity { host, bytes: 4_096 })
            .map_err(fail)?;
        check(machine.free_space("/").await == Ok(0), "no room")?;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
}

#[test]
fn the_registry_factory_returns_the_hosts_instance() {
    let summary = run(RunConfig::new(6), |world: World| async move {
        let same: Shared<Option<bool>> = Arc::default();
        let kept = Arc::clone(&same);
        world
            .add_host(HostConfig::new(boot(move |b: HostBoot| {
                let kept = Arc::clone(&kept);
                async move {
                    let registry = b.registry.build();
                    let mem = ImplName::new("mem").expect("valid");
                    let empty = ConfigValue::defaults(1).expect("valid");
                    let built = registry
                        .instantiate::<HostSeam>(&(), &mem, &empty)
                        .expect("registered");
                    *kept.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(Arc::ptr_eq(&built, &b.machine));
                }
            })))
            .map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        check(*same.lock().unwrap() == Some(true), "same instance")
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
}
