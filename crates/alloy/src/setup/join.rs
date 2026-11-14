//! `alloy join <token>` (ARFC 0002 section 3.3, as section 3.15 keeps it).
//!
//! `alloyd`'s [`join`](alloyd::join::join) writes the request and runs
//! steps 3 to 7 in this process (D-0069). `join` enables `alloyd` before
//! that call, so a node that is `joining` or `joined` always has `alloyd`
//! enabled (step 2), disables it again after a failed attempt (section 6),
//! and starts it after the call, as `init` does, to serve the joined node
//! or resume a join that waits.

use std::io::Write;
use std::net::IpAddr;

use alloy_crypto::Zeroizing;
use alloy_id::proto::{Disk, Facts};
use alloy_id::{JoinToken, Role, Roles};
use alloy_rt::{HostFacts, Platform};
use alloyd::JoinState;
use alloyd::join::{JoinError, JoinOptions, Joined};

use super::checks::{self, Kind, Passed};
use super::{
    DEFAULT_API_PORT, DEFAULT_MESH_PORT, Setup, Status, advertise, default_node_name,
    enable_alloyd, heading, node_name, open_data_dir, port, probe, restart_alloyd, roles_text,
    step, stop_alloyd,
};
use crate::login::hostname;
use crate::text::escape;
use crate::{CommandSpec, EXIT_USAGE, Failure, Invocation, UsageError, emit, note};

/// The most bytes of `Facts.os` (ARFC 0002 section 3.3 step 6).
const MAX_OS: usize = 64;

/// What the command line of `alloy join` gives.
pub(crate) struct JoinArgs {
    /// The token as given, a secret.
    pub(crate) token: Zeroizing<String>,
    /// The token, decoded.
    pub(crate) decoded: JoinToken,
    /// `--roles`, when given; `Some(Roles::NONE)` for `none`.
    pub(crate) roles: Option<Roles>,
    /// `--zone`, when given.
    pub(crate) zone: Option<String>,
    /// `--name`, when given.
    pub(crate) name: Option<String>,
    /// `--advertise`, when given.
    pub(crate) advertise: Option<Vec<IpAddr>>,
    /// `--mesh-port`.
    pub(crate) mesh_port: u16,
    /// `--api-port`.
    pub(crate) api_port: u16,
}

impl std::fmt::Debug for JoinArgs {
    /// Everything but the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinArgs")
            .field("roles", &self.roles)
            .field("zone", &self.zone)
            .field("name", &self.name)
            .field("advertise", &self.advertise)
            .field("mesh_port", &self.mesh_port)
            .field("api_port", &self.api_port)
            .finish_non_exhaustive()
    }
}

/// The checks of `alloy join` that need only the command line, run before
/// any machine is contacted: the token (ARFC 0002 section 3.2), the flags,
/// `--roles` against the token's preset, and on Windows no role at all.
///
/// # Errors
///
/// The first [`UsageError`] found. No message echoes the token.
pub(crate) fn check(
    spec: &CommandSpec,
    invocation: &Invocation,
    windows: bool,
) -> Result<JoinArgs, UsageError> {
    let Some(text) = invocation.operands.first() else {
        return Err(UsageError::of(
            spec,
            "`alloy join` needs a join token: alloy init printed one.".to_owned(),
        ));
    };
    let token = Zeroizing::new(text.clone());
    let decoded = JoinToken::decode(&token)
        .map_err(|e| UsageError::of(spec, format!("This is not a valid join token: {e}.")))?;
    let roles = invocation
        .value("--roles")
        .map(|text| parse_roles(spec, text))
        .transpose()?;
    if let Some(given) = roles
        && !decoded.preset_roles.is_empty()
        && given != decoded.preset_roles
    {
        return Err(UsageError::of(
            spec,
            format!(
                "`--roles` names {}, and the join token presets {}.",
                roles_text(given),
                roles_text(decoded.preset_roles)
            ),
        ));
    }
    let any_role = roles.is_some_and(|r| !r.is_empty()) || !decoded.preset_roles.is_empty();
    if windows && any_role {
        return Err(UsageError::of(
            spec,
            "On Windows every node is a client node, which holds no role: join with \
             --roles none, and a token that presets no roles."
                .to_owned(),
        ));
    }
    let zone = invocation.value("--zone").map(str::to_owned);
    if let Some(zone) = &zone
        && alloy_api::NodeName::new(zone).is_err()
    {
        return Err(UsageError::of(
            spec,
            format!(
                "`{}` cannot name a zone: a zone is named as a node is.",
                escape(zone)
            ),
        ));
    }
    Ok(JoinArgs {
        token,
        decoded,
        roles,
        zone,
        name: node_name(spec, invocation)?,
        advertise: advertise(spec, invocation)?,
        mesh_port: port(spec, invocation, "--mesh-port", DEFAULT_MESH_PORT)?,
        api_port: port(spec, invocation, "--api-port", DEFAULT_API_PORT)?,
    })
}

fn parse_roles(spec: &CommandSpec, text: &str) -> Result<Roles, UsageError> {
    if text == "none" {
        return Ok(Roles::NONE);
    }
    let mut roles = Vec::new();
    for word in text.split(',') {
        let role = match word.trim() {
            "anchor" => Role::Anchor,
            "control" => Role::Control,
            "compute" => Role::Compute,
            "storage" => Role::Storage,
            other => {
                return Err(UsageError::of(
                    spec,
                    format!(
                        "`--roles` takes anchor, control, compute, and storage separated by \
                         commas, or none, and `{}` is not one.",
                        escape(other)
                    ),
                ));
            }
        };
        roles.push(role);
    }
    Ok(Roles::of(&roles))
}

/// The roles the node takes for the checks (ARFC 0002 section 3.8):
/// `--roles`, else the token's preset, else none on Windows, else
/// `compute,storage`.
fn planned_roles(args: &JoinArgs, windows: bool) -> Roles {
    match args.roles {
        Some(roles) => roles,
        None if !args.decoded.preset_roles.is_empty() => args.decoded.preset_roles,
        None if windows => Roles::NONE,
        None => Roles::of(&[Role::Compute, Role::Storage]),
    }
}

/// `Facts` of the join request from the `host` seam's facts (ARFC 0002
/// section 3.3 step 6).
pub(crate) fn request_facts(facts: &HostFacts) -> Facts {
    let os = match &facts.platform {
        Platform::Linux { release, .. } => format!("linux {release}"),
        Platform::Windows {
            major,
            minor,
            build,
        } => format!("windows {major}.{minor}.{build}"),
    };
    let mut end = os.len().min(MAX_OS);
    while !os.is_char_boundary(end) {
        end -= 1;
    }
    Facts {
        os: os[..end].to_owned(),
        arch: facts.arch.as_str().to_owned(),
        cores: facts.cores,
        memory_bytes: facts.memory,
        virtualization: facts.virtualization,
        disks: facts
            .disks
            .iter()
            .map(|disk| Disk {
                path: disk.path.clone(),
                size_bytes: disk.size,
            })
            .collect(),
    }
}

/// `alloy join`: the checks, then `alloyd`'s join, then `alloyd` enabled
/// and started.
///
/// # Errors
///
/// A [`Failure`] with the exit code of the step that failed:
/// [`JoinError::exit_code`] for the join itself.
pub(crate) async fn join(
    setup: &Setup<'_>,
    spec: &CommandSpec,
    args: &JoinArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let planned = planned_roles(args, setup.ctx.env.windows);
    let kind = Kind::Join {
        roles: planned,
        cluster_id: args.decoded.cluster_id,
    };
    let passed = checks::run(setup, kind, args.mesh_port, args.api_port, err).await?;
    after_checks(setup, spec, args, &passed, out, err).await
}

/// `alloy join` from the end of step 1 on, once the checks passed with
/// `passed`.
///
/// # Errors
///
/// As [`join`].
pub(crate) async fn after_checks(
    setup: &Setup<'_>,
    spec: &CommandSpec,
    args: &JoinArgs,
    passed: &Passed,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let style = setup.ctx.styles.err;
    let planned = planned_roles(args, setup.ctx.env.windows);
    if args.roles.is_some_and(|r| r.contains(Role::Compute)) && !passed.facts.virtualization {
        return Err(UsageError::of(
            spec,
            "`--roles` names compute, and this machine has no hardware virtualization, so \
             it may hold compute only once a cloud admin dedicates it to one organization."
                .to_owned(),
        )
        .into());
    }
    let advertise = match &args.advertise {
        Some(given) => given.clone(),
        None => {
            let routes = probe::routes(setup).await;
            let interfaces = probe::interfaces(setup).await;
            match probe::default_route_address(&routes, &interfaces) {
                Some((ip, iface)) => {
                    note(
                        err,
                        &format!(
                            "Advertise address: {ip} ({}, default route). Change it with \
                             --advertise.\n",
                            escape(&iface)
                        ),
                    );
                    vec![ip]
                }
                None if planned.contains(Role::Anchor) || planned.contains(Role::Control) => {
                    return Err(UsageError::of(
                        spec,
                        "This machine has no default route, and an anchor or control node \
                         needs an address other machines reach it at. Give it with \
                         --advertise."
                            .to_owned(),
                    )
                    .into());
                }
                None => Vec::new(),
            }
        }
    };
    let (name, name_is_default) = match &args.name {
        Some(name) => (name.clone(), false),
        None => (default_node_name(&hostname(setup.ctx).await), true),
    };
    let options = JoinOptions {
        token: args.token.clone(),
        name,
        name_is_default,
        roles: args.roles,
        zone: args.zone.clone(),
        advertise,
        mesh_port: args.mesh_port,
        api_port: args.api_port,
        facts: request_facts(&passed.facts),
    };
    let anchors = args.decoded.anchors.len();
    heading(
        err,
        &format!(
            "Joining the cluster through {anchors} anchor{}",
            if anchors == 1 { "" } else { "s" }
        ),
    );
    let paths = open_data_dir(setup, passed.action).await?;
    // Step 2: `alloyd` is enabled before the request is written and the
    // attempt runs, so no failure or interruption from here on leaves a
    // node `joining` or `joined` whose `alloyd` does not start at boot.
    enable_alloyd(setup.host).await?;
    let outcome = alloyd::join::join(setup.ctx.runtime, &paths, &options).await;
    let joined = match outcome {
        Ok(joined) => Some(joined),
        Err(JoinError::Pending) => None,
        Err(e) => {
            // A failed attempt leaves the local join state `unjoined` and
            // `alloyd` disabled (section 6). A refusal of a rerun, before
            // anything was written, leaves a join in progress or a joined
            // node as it was, its `alloyd` enabled.
            let state = checks::read_state(setup).await;
            if !matches!(state, Ok(JoinState::Joining | JoinState::Joined))
                && let Err(stop) = stop_alloyd(setup.host).await
            {
                step(err, style, Status::Fail, &stop.message);
            }
            return Err(join_failure(&e));
        }
    };
    match &joined {
        Some(joined) => step(
            err,
            style,
            Status::Done,
            &format!("node record issued for {}", escape(&joined.node)),
        ),
        None => step(
            err,
            style,
            Status::Done,
            "join request sent; it waits for a cloud admin's approval",
        ),
    }
    // The node is a node now: a failed start leaves `alloyd` enabled, so
    // that it serves the node from the next boot, or once started by hand.
    if let Err(failure) = restart_alloyd(setup.host).await {
        let start = if setup.ctx.env.windows {
            "Start-Service alloyd"
        } else {
            "sudo systemctl start alloyd"
        };
        return Err(Failure::new(
            failure.code,
            format!(
                "This machine is a node, and alloyd stays enabled, so it starts at the next \
                 boot. {}",
                failure.message
            ),
        )
        .with_try(start));
    }
    step(err, style, Status::Done, "alloyd enabled and started");
    emit(out, &result_text(joined.as_ref()))?;
    note(
        err,
        "Next: from a logged-in computer, run alloy node list\n",
    );
    Ok(())
}

fn join_failure(error: &JoinError) -> Failure {
    let code = error.exit_code();
    let message = format!(
        "alloy join did not make this machine a node: {}.",
        escape(&error.to_string())
    );
    if code == EXIT_USAGE {
        return Failure::new(code, message).with_try("alloy join --help");
    }
    Failure::new(code, message)
}

/// What `join` made, for standard output.
pub(crate) fn result_text(joined: Option<&Joined>) -> String {
    match joined {
        Some(joined) => format!(
            "Joined as node {}, roles {}, zone {}.\n",
            escape(&joined.node),
            roles_text(joined.roles),
            escape(&joined.zone)
        ),
        None => "The join request waits for a cloud admin's approval. alloyd finishes the \
                 join once it is approved.\n"
            .to_owned(),
    }
}
