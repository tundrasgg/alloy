//! `alloy login` and `alloy node list` against a control node that
//! `alloyd` creates with `init` and serves, under the `os` runtime on
//! loopback (ADR 0042: TLS is tested under `os`).
//!
//! The data directory is not an Alloy data directory, since a test does not
//! run as root, so the test calls `alloyd`'s `init` and
//! [`ControlNode::start`] itself, as `alloyd`'s own tests do. The local admin
//! listener is a loopback TCP listener whose connections report the peer
//! user id 0, as the kernel reports for root on the Unix socket, and the CLI
//! is pointed at it in place of `/run/alloy/admin.sock`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alloy_id::{DataDir, LoginCode};
use alloy_rt::{
    BoxFuture, DirEntry, DiskError, DiskFile, DiskIo, EntryKind, ListenOptions, Listener, Metadata,
    NetError, OpenOptions, OsHost, Protection, RtPath, Runtime, Security, Stream, StreamAddr,
    UdpOptions,
};
use alloyd::init::{InitOptions, init};
use alloyd::{ControlNode, Listeners, NodePaths};

use super::*;
use crate::machine::{os_runtime, split};

/// A listener whose connections report the peer user id 0.
#[derive(Debug)]
pub(super) struct AsRoot(pub(super) Box<dyn Listener>);

impl Listener for AsRoot {
    fn local_addr(&self) -> StreamAddr {
        self.0.local_addr()
    }

    fn accept(&self) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>> {
        Box::pin(async move {
            let inner = self.0.accept().await?;
            Ok(Box::new(Root(inner)) as Box<dyn Stream>)
        })
    }
}

#[derive(Debug)]
struct Root(Box<dyn Stream>);

impl Stream for Root {
    fn local_addr(&self) -> Option<StreamAddr> {
        self.0.local_addr()
    }

    fn peer_addr(&self) -> Option<StreamAddr> {
        self.0.peer_addr()
    }

    fn read<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, NetError>> {
        self.0.read(buf)
    }

    fn write<'a>(&'a self, bufs: &'a [&'a [u8]]) -> BoxFuture<'a, Result<usize, NetError>> {
        self.0.write(bufs)
    }

    fn shutdown_write(&self) -> BoxFuture<'_, Result<(), NetError>> {
        self.0.shutdown_write()
    }

    fn peer_uid(&self) -> Result<u32, NetError> {
        Ok(0)
    }
}

/// The `os` `disk-io` with every protection given as [`Protection::Default`]:
/// on Windows, a test that does not run as an administrator may not name
/// `Administrators` as an owner, which `init` asks for. What this test
/// checks does not depend on it.
#[derive(Debug)]
pub(super) struct Unprotected(pub(super) Arc<dyn DiskIo>);

impl DiskIo for Unprotected {
    fn open<'a>(
        &'a self,
        path: &'a RtPath,
        options: OpenOptions,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        self.0.open(path, options)
    }
    fn metadata<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>> {
        self.0.metadata(path)
    }
    fn list<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>> {
        self.0.list(dir)
    }
    fn create_dir<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.create_dir(path)
    }
    fn remove<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.remove(path)
    }
    fn rename<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.rename(from, to)
    }
    fn replace<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.replace(from, to)
    }
    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.sync_dir(dir)
    }
    fn create_file<'a>(
        &'a self,
        path: &'a RtPath,
        _protection: Protection,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        self.0.create_file(path, Protection::Default)
    }
    fn create_dir_protected<'a>(
        &'a self,
        path: &'a RtPath,
        _protection: Protection,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.create_dir_protected(path, Protection::Default)
    }
    fn security<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Security, DiskError>> {
        self.0.security(path)
    }
    fn set_mode<'a>(&'a self, path: &'a RtPath, mode: u32) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.set_mode(path, mode)
    }
    fn rt_path(&self, native: &str) -> Result<RtPath, DiskError> {
        self.0.rt_path(native)
    }
}

/// A loopback TCP listener on a free port, and its address.
pub(super) fn listen(runtime: &Runtime) -> (Box<dyn Listener>, SocketAddr) {
    let listener = runtime
        .net()
        .listen(
            StreamAddr::Ip(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
            ListenOptions::default(),
        )
        .expect("listen");
    let StreamAddr::Ip(addr) = listener.local_addr() else {
        panic!("a TCP listener has an IP address");
    };
    (listener, addr)
}

/// Removes the directory `dir` and everything below it.
pub(super) async fn remove_tree(disk: &dyn DiskIo, dir: &RtPath) {
    let mut stack = vec![(dir.clone(), false)];
    while let Some((path, listed)) = stack.pop() {
        if listed {
            disk.remove(&path).await.expect("remove a directory");
            continue;
        }
        stack.push((path.clone(), true));
        let entries = match disk.list(&path).await {
            Ok(entries) => entries,
            Err(DiskError::NotFound) => {
                stack.pop();
                continue;
            }
            Err(e) => panic!("{e}"),
        };
        for entry in entries {
            let child = path.join(&entry.name).expect("path");
            if entry.kind == EntryKind::Dir {
                stack.push((child, false));
            } else {
                disk.remove(&child).await.expect("remove a file");
            }
        }
    }
}

/// One computer that runs the CLI: its configuration directory.
struct Computer {
    config: PathBuf,
    env: Env,
}

impl Computer {
    fn new(base: &Path, name: &str, admin: SocketAddr) -> Computer {
        let config = base.join(format!("{name}-config"));
        let mut env = Env {
            admin_socket: Some(StreamAddr::Ip(admin)),
            ..Env::default()
        };
        env.vars.insert(
            "XDG_CONFIG_HOME".to_owned(),
            OsString::from(config.as_os_str()),
        );
        // As on Windows, so that the hostname comes from `COMPUTERNAME` on
        // every platform the test runs on.
        env.vars
            .insert("COMPUTERNAME".to_owned(), OsString::from("Test-PC.lan"));
        env.windows = true;
        Computer { config, env }
    }

    async fn cli(&self, runtime: &Runtime, transport: Transport, args: &[&str]) -> Ran {
        let args: Vec<String> = args.iter().map(|&a| a.to_owned()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = match parse(COMMANDS, &args) {
            Ok(Parsed::Run(spec, invocation)) => {
                let ctx = Ctx {
                    runtime,
                    transport,
                    env: &self.env,
                    styles: Styles::default(),
                };
                execute(&ctx, spec, &invocation, &mut out, &mut err).await
            }
            other => panic!("{args:?} does not run: {other:?}"),
        };
        Ran {
            code,
            out: String::from_utf8(out).expect("UTF-8"),
            err: String::from_utf8(err).expect("UTF-8"),
        }
    }
}

#[test]
fn login_issue_code_and_node_list_against_a_control_node() {
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
    let base = temp.join(format!("alloy-cli-test-{:016x}", u64::from_be_bytes(draw)));
    let (_, base_rt) = split(&base).expect("split");
    let disk = Arc::clone(runtime.disk_io());
    host.block_on(disk.create_dir(&base_rt)).expect("base");
    let outcome = host.block_on(scenario(&runtime, transport, &base));
    host.block_on(remove_tree(disk.as_ref(), &base_rt));
    host.shutdown(Duration::from_secs(20));
    outcome.expect("scenario");
}

async fn scenario(runtime: &Runtime, transport: Transport, base: &Path) -> Result<(), String> {
    let data = base.join("data");
    let (_, dir) = split(&data).map_err(|e| e.message)?;
    runtime
        .disk_io()
        .create_dir(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let paths = NodePaths {
        data: DataDir::new(Arc::clone(runtime.disk_io()), dir.clone()),
        dir,
        native: data.clone(),
    };
    let (api, api_addr) = listen(runtime);
    let mesh = runtime
        .net()
        .bind_udp(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            UdpOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    let done = init(
        runtime,
        &paths,
        &InitOptions {
            name: "linux-1".to_owned(),
            advertise: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            mesh_port: mesh.local_addr().port(),
            api_port: api_addr.port(),
            compute: true,
            cluster_id: None,
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    let (admin, admin_addr) = listen(runtime);
    let listeners = Listeners {
        api,
        admin: Some(Box::new(AsRoot(admin))),
        mesh: vec![mesh],
    };
    let node = ControlNode::start(runtime, &paths, move |_| Ok(listeners), transport)
        .await
        .map_err(|e| e.to_string())?;
    let result = against(
        runtime,
        transport,
        base,
        &done.login_code,
        api_addr,
        admin_addr,
    )
    .await;
    node.stop().await.map_err(|e| e.to_string())?;
    result
}

async fn against(
    runtime: &Runtime,
    transport: Transport,
    base: &Path,
    init_code: &str,
    api_addr: SocketAddr,
    admin_addr: SocketAddr,
) -> Result<(), String> {
    let laptop = Computer::new(base, "laptop", admin_addr);
    let control = Computer::new(base, "control", admin_addr);
    let context = format!("127.0.0.1:{}", api_addr.port());

    // No context: `node list` is not logged in, and points at a login code.
    let ran = laptop.cli(runtime, transport, &["node", "list"]).await;
    assert_eq!(ran.code, EXIT_DENIED, "{}", ran.err);
    assert!(
        ran.err.starts_with("Not logged in to any cluster."),
        "{}",
        ran.err
    );
    assert!(
        ran.err.ends_with("Try: alloy login <login code>\n"),
        "{}",
        ran.err
    );

    // As root on the control node with no context, `--issue-code` goes
    // through the local admin socket and prints the code once, alone, on
    // standard output.
    let ran = control
        .cli(runtime, transport, &["login", "--issue-code"])
        .await;
    assert_eq!(ran.code, EXIT_SUCCESS, "{}", ran.err);
    let issued = ran.out.trim_end().to_owned();
    assert_eq!(ran.out, format!("{issued}\n"));
    let code = LoginCode::decode(&issued).map_err(|e| e.to_string())?;
    assert_eq!(code.endpoints, [api_addr]);
    assert!(!ran.err.contains(&issued), "{}", ran.err);
    assert!(
        ran.err
            .starts_with("This login code logs one computer in as admin in organization default,"),
        "{}",
        ran.err
    );
    // A computer without the socket is not logged in.
    let mut no_socket = Computer::new(base, "elsewhere", admin_addr);
    no_socket.env.admin_socket = None;
    let ran = no_socket
        .cli(runtime, transport, &["login", "--issue-code"])
        .await;
    assert_eq!(ran.code, EXIT_DENIED, "{}", ran.err);
    assert!(ran.err.starts_with("Not logged in"), "{}", ran.err);

    // The laptop logs in with it.
    let ran = laptop.cli(runtime, transport, &["login", &issued]).await;
    assert_eq!(ran.code, EXIT_SUCCESS, "{}", ran.err);
    assert_eq!(
        ran.out,
        "Logged in as admin in organization default, with the access key test-pc.\n"
    );
    assert!(
        ran.err.starts_with(&format!(
            "Connecting to {api_addr}\n  ok    API certificate matches the login code\nSaved context {context} and made it current."
        )),
        "{}",
        ran.err
    );
    assert!(
        ran.err
            .contains("Defaults: organization default, no project.\n"),
        "{}",
        ran.err
    );
    assert!(ran.err.ends_with("Next: alloy node list\n"), "{}", ran.err);
    assert!(!ran.err.contains(&issued) && !ran.out.contains(&issued));
    let contexts = read_text(runtime, &laptop.config.join("alloy").join("contexts")).await;
    assert!(
        contexts.starts_with(&format!(
            "alloy-contexts 1\ncurrent {context}\ncontext {context}\n"
        )),
        "{contexts}"
    );
    let credentials = read_text(runtime, &laptop.config.join("alloy").join("credentials")).await;
    assert_eq!(credentials.lines().count(), 2, "one key");

    // The code is spent: a second exchange is refused, and leads to a new
    // code.
    let ran = control.cli(runtime, transport, &["login", &issued]).await;
    assert_eq!(ran.code, EXIT_DENIED, "{}", ran.err);
    assert!(ran.err.contains("login_code_used"), "{}", ran.err);
    assert!(
        ran.err.ends_with("Try: sudo alloy login --issue-code\n"),
        "{}",
        ran.err
    );

    // `node list` shows the node once it is active.
    let mut table = String::new();
    for _ in 0..100 {
        let ran = laptop.cli(runtime, transport, &["node", "list"]).await;
        assert_eq!(ran.code, EXIT_SUCCESS, "{}", ran.err);
        assert_eq!(ran.err, "");
        if ran.out.contains(" active ") {
            table = ran.out;
            break;
        }
        runtime.sleep(Duration::from_millis(100)).await;
    }
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 2, "{table}");
    assert!(
        lines[0].starts_with("NAME     STATE   ROLES                           MESH  RELEASE"),
        "{table}"
    );
    assert!(lines[0].ends_with("  ADDRESS"), "{table}");
    assert!(
        lines[1].starts_with("linux-1  active  anchor,control,compute,storage  -     "),
        "{table}"
    );
    assert!(lines[1].ends_with("  127.0.0.1"), "{table}");
    let ran = laptop
        .cli(runtime, transport, &["node", "list", "--output", "json"])
        .await;
    assert_eq!(ran.code, EXIT_SUCCESS, "{}", ran.err);
    assert!(
        ran.out.starts_with("[\n  {\n    \"name\": \"linux-1\",\n"),
        "{}",
        ran.out
    );
    assert!(
        ran.out.contains("\"state\": \"NODE_STATE_ACTIVE\""),
        "{}",
        ran.out
    );
    let ran = laptop
        .cli(runtime, transport, &["node", "list", "--output", "yaml"])
        .await;
    assert!(ran.out.starts_with("- name: \"linux-1\"\n"), "{}", ran.out);

    // With a context, `--issue-code` asks the API port, which refuses it in
    // rung 2 (ARFC 0002 section 3.15).
    let ran = laptop
        .cli(runtime, transport, &["login", "--issue-code"])
        .await;
    assert_eq!(ran.code, EXIT_DENIED, "{}", ran.err);
    assert_eq!(ran.out, "");
    let ran = laptop
        .cli(
            runtime,
            transport,
            &["login", "--issue-code", "--user", "bob"],
        )
        .await;
    assert_eq!(ran.code, EXIT_USAGE, "{}", ran.err);

    // Logging in to the same context again replaces its key, which keeps
    // its name (ARFC 0007 clause 3.9.10).
    let ran = laptop.cli(runtime, transport, &["login", init_code]).await;
    assert_eq!(ran.code, EXIT_SUCCESS, "{}", ran.err);
    assert_eq!(
        ran.out,
        "Logged in as admin in organization default, with the access key test-pc.\n"
    );
    let again = read_text(runtime, &laptop.config.join("alloy").join("credentials")).await;
    assert_eq!(again.lines().count(), 2, "still one key");
    assert_ne!(again, credentials, "a new key");
    let ran = laptop.cli(runtime, transport, &["node", "list"]).await;
    assert_eq!(ran.code, EXIT_SUCCESS, "{}", ran.err);

    // A code whose fingerprint is not the cluster's API CA is refused
    // before anything is sent, with no Try: line.
    let ran = control
        .cli(runtime, transport, &["login", "--issue-code"])
        .await;
    let mut forged = LoginCode::decode(ran.out.trim_end()).map_err(|e| e.to_string())?;
    forged.api_ca_fp[0] ^= 1;
    let forged = forged.encode().map_err(|e| e.to_string())?;
    let ran = laptop.cli(runtime, transport, &["login", &forged]).await;
    assert_eq!(ran.code, EXIT_FAILURE, "{}", ran.err);
    assert!(
        flat(&ran.err)
            .contains("presented a certificate authority other than the one the login code pins"),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("Try:"), "{}", ran.err);

    // A code whose endpoint answers nothing exits 6.
    let (closed, closed_addr) = listen(runtime);
    drop(closed);
    let mut silent = LoginCode::decode(&forged).map_err(|e| e.to_string())?;
    silent.endpoints = vec![closed_addr];
    let silent = silent.encode().map_err(|e| e.to_string())?;
    let ran = laptop.cli(runtime, transport, &["login", &silent]).await;
    assert_eq!(ran.code, EXIT_UNAVAILABLE, "{}", ran.err);
    assert!(
        flat(&ran.err).contains(&format!("within 10 seconds: {closed_addr}.")),
        "{}",
        ran.err
    );

    // A context whose key is gone leads to a new login.
    let store_dir = split(&laptop.config.join("alloy"))
        .map_err(|e| e.message)?
        .1;
    let path = store_dir.join("credentials").map_err(|e| e.to_string())?;
    runtime
        .disk_io()
        .remove(&path)
        .await
        .map_err(|e| e.to_string())?;
    let ran = laptop.cli(runtime, transport, &["node", "list"]).await;
    assert_eq!(ran.code, EXIT_DENIED, "{}", ran.err);
    assert!(
        ran.err.ends_with(&format!(
            "Try: alloy login <login code> --context {context}\n"
        )),
        "{}",
        ran.err
    );
    Ok(())
}

/// `text` with its wrapped lines joined again.
fn flat(text: &str) -> String {
    text.replace('\n', " ")
}

async fn read_text(runtime: &Runtime, path: &Path) -> String {
    let (_, rt) = split(path).expect("split");
    let dir = rt.parent().expect("parent");
    let name = rt.file_name().expect("name").to_owned();
    let store = DataDir::new(Arc::clone(runtime.disk_io()), dir);
    let name: &'static str = if name == "contexts" {
        "contexts"
    } else {
        "credentials"
    };
    let bytes = store.read(name).await.expect("read").expect("present");
    String::from_utf8(bytes.to_vec()).expect("UTF-8")
}
