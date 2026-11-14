//! `alloy init` (ARFC 0002 section 3.1, as section 3.15 keeps it).

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use alloy_api::client::{ConnectError, Credential, connect};
use alloy_api::tls::ApiCaFingerprint;
use alloy_id::LoginCode;
use alloy_proto::cluster::v1alpha1::NodeState;
use alloy_rt::Interface;
use alloyd::NodePaths;
use alloyd::init::{InitError, InitOptions, Initialized};

use super::checks::{self, Kind, Passed};
use super::{
    DEFAULT_API_PORT, DEFAULT_MESH_PORT, Setup, Status, advertise, default_node_name, heading,
    is_private, node_name, open_data_dir, port, probe, remove_written, start_alloyd, step,
    stop_alloyd,
};
use crate::api::within;
use crate::login::{hostname, local_admin};
use crate::text::escape;
use crate::{
    CommandSpec, EXIT_CONFLICT, EXIT_FAILURE, EXIT_UNAVAILABLE, EXIT_USAGE, Failure, Invocation,
    UsageError, emit, note,
};

/// How long `init` waits for its node to be `active` and its API to answer
/// TLS, inside the 30 seconds `init` may take (ARFC 0002 section 3.1).
const READY_LIMIT: Duration = Duration::from_secs(20);

/// How often `init` asks whether its node is `active`.
const READY_POLL: Duration = Duration::from_millis(250);

/// What the command line of `alloy init` gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InitArgs {
    /// `--advertise`, when given.
    pub(crate) advertise: Option<Vec<IpAddr>>,
    /// `--name`, when given.
    pub(crate) name: Option<String>,
    /// `--mesh-port`.
    pub(crate) mesh_port: u16,
    /// `--api-port`.
    pub(crate) api_port: u16,
}

/// The checks of `alloy init` that need only the command line.
///
/// # Errors
///
/// The first [`UsageError`] found.
pub(crate) fn check(spec: &CommandSpec, invocation: &Invocation) -> Result<InitArgs, UsageError> {
    Ok(InitArgs {
        advertise: advertise(spec, invocation)?,
        name: node_name(spec, invocation)?,
        mesh_port: port(spec, invocation, "--mesh-port", DEFAULT_MESH_PORT)?,
        api_port: port(spec, invocation, "--api-port", DEFAULT_API_PORT)?,
    })
}

/// `alloy init`: the checks, the advertise addresses, the cluster, then
/// `alloyd` enabled, started, and ready; the join token and the login
/// code on standard output.
///
/// # Errors
///
/// A [`Failure`] with the exit code of the step that failed. A failure
/// after the checks leaves `alloyd` stopped and disabled and the data
/// directory holding only its host file, `unjoined`.
pub(crate) async fn init(
    setup: &Setup<'_>,
    spec: &CommandSpec,
    args: &InitArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let passed = checks::run(setup, Kind::Init, args.mesh_port, args.api_port, err).await?;
    after_checks(setup, spec, args, &passed, out, err).await
}

/// `alloy init` from step 2 on, once the checks passed with `passed`.
///
/// # Errors
///
/// As [`init`].
pub(crate) async fn after_checks(
    setup: &Setup<'_>,
    spec: &CommandSpec,
    args: &InitArgs,
    passed: &Passed,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let interfaces = probe::interfaces(setup).await;
    let advertise = pick_advertise(setup, spec, args, &interfaces, err).await?;
    let name = match &args.name {
        Some(name) => name.clone(),
        None => default_node_name(&hostname(setup.ctx).await),
    };
    let options = InitOptions {
        name,
        advertise,
        mesh_port: args.mesh_port,
        api_port: args.api_port,
        compute: passed.facts.virtualization,
        cluster_id: passed.cluster_id,
    };
    let (paths, initialized) = create(setup, passed, &options, err).await?;
    heading(err, "Starting alloyd");
    if let Err(failure) = start_and_wait(setup, &options, &initialized, &interfaces, err).await {
        clean_up(setup, &paths, err).await;
        return Err(failure);
    }
    emit(out, &result_text(&initialized))
}

/// The advertise addresses of step 2, reported each with where it came
/// from and whether it is private.
async fn pick_advertise(
    setup: &Setup<'_>,
    spec: &CommandSpec,
    args: &InitArgs,
    interfaces: &[Interface],
    err: &mut dyn Write,
) -> Result<Vec<IpAddr>, Failure> {
    let picked: Vec<(IpAddr, String)> = match &args.advertise {
        Some(given) => given
            .iter()
            .map(|&ip| (ip, "given with --advertise".to_owned()))
            .collect(),
        None => {
            let routes = probe::routes(setup).await;
            let Some((ip, iface)) = probe::default_route_address(&routes, interfaces) else {
                return Err(UsageError::of(
                    spec,
                    "This machine has no default route, so alloy cannot pick the address \
                     other machines reach it at. Give it with --advertise."
                        .to_owned(),
                )
                .into());
            };
            vec![(ip, format!("{}, default route", escape(&iface)))]
        }
    };
    for (ip, source) in &picked {
        let mut text = format!("Advertise address: {ip} ({source}).");
        if args.advertise.is_none() {
            text.push_str(" Change it with --advertise.");
        }
        note(err, &format!("{text}\n"));
        if is_private(*ip) {
            note(
                err,
                "  This is a private address. Only machines that can reach this network \
                 can join.\n",
            );
        }
    }
    Ok(picked.into_iter().map(|(ip, _)| ip).collect())
}

/// Steps 3 and the data directory: creates the cluster through `alloyd`.
async fn create(
    setup: &Setup<'_>,
    passed: &Passed,
    options: &InitOptions,
    err: &mut dyn Write,
) -> Result<(NodePaths, Initialized), Failure> {
    let style = setup.ctx.styles.err;
    heading(err, "Creating the cluster");
    let paths = open_data_dir(setup, passed.action).await?;
    let initialized = alloyd::init::init(setup.ctx.runtime, &paths, options)
        .await
        .map_err(|e| init_failure(&e))?;
    step(
        err,
        style,
        Status::Done,
        "cluster key and API certificate authority",
    );
    step(err, style, Status::Done, "control plane, 1 node");
    step(err, style, Status::Done, "organization default, user admin");
    let roles = if options.compute {
        "anchor, control, compute, storage"
    } else {
        "anchor, control, storage"
    };
    step(
        err,
        style,
        Status::Done,
        &format!("node {}: {roles}", escape(&initialized.node)),
    );
    Ok((paths, initialized))
}

fn init_failure(error: &InitError) -> Failure {
    let code = match error {
        InitError::Conflict(_) => EXIT_CONFLICT,
        InitError::Usage(_) => EXIT_USAGE,
        _ => EXIT_FAILURE,
    };
    Failure::new(
        code,
        format!(
            "alloy init could not create the cluster: {}.",
            escape(&error.to_string())
        ),
    )
}

/// Step 4: `alloyd` enabled and started, then the node `active` and the
/// API answering TLS at the first advertise address on an interface, else
/// at loopback.
async fn start_and_wait(
    setup: &Setup<'_>,
    options: &InitOptions,
    initialized: &Initialized,
    interfaces: &[Interface],
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let style = setup.ctx.styles.err;
    start_alloyd(setup.host).await?;
    step(err, style, Status::Done, "alloyd enabled and started");
    let code = LoginCode::decode(&initialized.login_code).map_err(|e| {
        Failure::new(
            EXIT_FAILURE,
            format!("alloyd made a login code alloy cannot read: {e}."),
        )
    })?;
    let on_interface = options.advertise.iter().copied().find(|ip| {
        interfaces
            .iter()
            .any(|i| i.addrs.iter().any(|a| a.ip == *ip))
    });
    let target = on_interface.unwrap_or(match options.advertise.first() {
        Some(IpAddr::V6(_)) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        _ => IpAddr::V4(Ipv4Addr::LOCALHOST),
    });
    let endpoint = SocketAddr::new(target, options.api_port);
    let ready = within(
        setup.ctx.runtime,
        READY_LIMIT,
        ready(setup, &initialized.node, endpoint, code.api_ca_fp),
    )
    .await;
    match ready {
        Some(Ok(())) => {}
        Some(Err(failure)) => return Err(failure),
        None => {
            return Err(Failure::new(
                EXIT_UNAVAILABLE,
                format!(
                    "alloyd started, but node {} was not active with its API answering \
                     at {endpoint} within 20 seconds.",
                    escape(&initialized.node)
                ),
            ));
        }
    }
    step(
        err,
        style,
        Status::Ok,
        &format!(
            "node {} is active, API answers at {endpoint}",
            escape(&initialized.node)
        ),
    );
    if on_interface.is_none() {
        note(
            err,
            "  No advertise address is on this machine's interfaces, so the API was \
             checked at loopback. alloy did not check whether other machines reach the \
             advertise address.\n",
        );
    }
    Ok(())
}

/// Waits for the node `name` to be `active` through the local admin
/// socket, then for the API at `endpoint` to answer TLS with a chain that
/// verifies to `api_ca_fp`.
async fn ready(
    setup: &Setup<'_>,
    name: &str,
    endpoint: SocketAddr,
    api_ca_fp: [u8; 16],
) -> Result<(), Failure> {
    let runtime = setup.ctx.runtime;
    loop {
        if let Ok(client) = local_admin(setup.ctx).await
            && let Ok(nodes) = client.list_all_nodes().await
        {
            let active = nodes.iter().any(|node| {
                node.name == name
                    && node.status.as_ref().is_some_and(|s| {
                        NodeState::try_from(s.state).is_ok_and(|s| s == NodeState::Active)
                    })
            });
            if active {
                break;
            }
        }
        runtime.sleep(READY_POLL).await;
    }
    loop {
        let fp = ApiCaFingerprint(api_ca_fp);
        match connect(
            runtime,
            setup.ctx.transport,
            endpoint,
            fp,
            &Credential::None,
        )
        .await
        {
            Ok(_) => return Ok(()),
            Err(ConnectError::Certificate(_)) => {
                return Err(Failure::new(
                    EXIT_FAILURE,
                    format!(
                        "The API at {endpoint} presented a certificate authority other \
                         than the cluster's, so another program answers there."
                    ),
                ));
            }
            Err(_) => runtime.sleep(READY_POLL).await,
        }
    }
}

/// After a failed step 4: `alloyd` stopped and disabled, and what `init`
/// wrote removed (ARFC 0002 section 3.1).
async fn clean_up(setup: &Setup<'_>, paths: &NodePaths, err: &mut dyn Write) {
    let style = setup.ctx.styles.err;
    if let Err(failure) = stop_alloyd(setup.host).await {
        step(err, style, Status::Fail, &failure.message);
    }
    if let Err(e) = remove_written(paths, setup.ctx.runtime.disk_io().as_ref()).await {
        step(
            err,
            style,
            Status::Fail,
            &format!(
                "alloy could not remove what init wrote in {}: {}",
                paths.native.display(),
                escape(&e.to_string())
            ),
        );
    }
}

/// Step 5: the join token and the login code, once each, with what each is
/// for. Rung 2 reports no standing warning (ARFC 0002 section 3.15).
pub(crate) fn result_text(initialized: &Initialized) -> String {
    format!(
        "Add a machine. Install Alloy on it, then run:\n  sudo alloy join {}\n\
         This join token works for 24 hours, any number of times. Anyone who has it can\n\
         add a machine to this cluster.\n\n\
         Log in from your own computer:\n  alloy login {}\n\
         This login code works once, within 24 hours. Anyone who has it can act as the\n\
         cluster admin until it is used.\n\n\
         Both strings are shown only now and are not written to any log.\n",
        initialized.join_token.as_str(),
        initialized.login_code.as_str()
    )
}
