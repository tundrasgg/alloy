//! `alloy login <login code>` and `alloy login --issue-code` (ARFC 0019
//! clauses 3.1.11, 3.1.13, and 3.1.14, ARFC 0002 sections 3.7 and 3.15).

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use alloy_api::client::{Client, Credential, local_admin_over};
use alloy_api::sigv4::AccessKeyCredential;
use alloy_crypto::Zeroizing;
use alloy_id::login::{CodeId, CodeSecret};
use alloy_id::{ClusterId, LoginCode};
use alloy_proto::iam::v1alpha1::{
    ExchangeLoginCodeRequest, IssueLoginCodeRequest, IssueLoginCodeResponse,
};
use alloy_rt::{Access, Create, OpenOptions, ReadOp, RtPath, StreamOptions};

use crate::api::{api_failure, listed, reach, unreached};
use crate::config::{CREDENTIALS, Context, Store, context_name_ok, default_context_name, shown};
use crate::text::escape;
use crate::{
    CommandSpec, Ctx, EXIT_DENIED, EXIT_FAILURE, Failure, Invocation, UsageError, emit, machine,
    note,
};

/// The user `--issue-code` names on the local admin path by default (ARFC
/// 0019 clause 3.1.14).
const DEFAULT_USER: &str = "admin";

/// The org of [`DEFAULT_USER`].
const DEFAULT_ORG: &str = "default";

/// The longest key name (ARFC 0007 clause 3.9.3).
const MAX_KEY_NAME: usize = 63;

/// The key name when the hostname cannot be found.
const FALLBACK_KEY_NAME: &str = "computer";

/// The configuration directory of this run.
pub(crate) fn store(ctx: &Ctx<'_>) -> Result<Store, Failure> {
    let native = machine::config_dir(ctx.env)?;
    let (_, dir) = machine::split(&native)?;
    Ok(Store::new(
        std::sync::Arc::clone(ctx.runtime.disk_io()),
        dir,
        native,
    ))
}

/// The failure of a command that needs a context when none is current
/// (ARFC 0019 clause 3.1.14, PB-0001's lost admin credential).
pub(crate) fn not_logged_in() -> Failure {
    Failure::new(
        EXIT_DENIED,
        "Not logged in to any cluster. Log in with a login code: alloy init printed \
         one, and `sudo alloy login --issue-code` on a control node prints a new one."
            .to_owned(),
    )
    .with_try("alloy login <login code>")
}

/// The checks of `alloy login` that need nothing but the command line, run
/// before anything is read or sent.
///
/// # Errors
///
/// The first [`UsageError`] found.
pub(crate) fn check(spec: &CommandSpec, invocation: &Invocation) -> Result<(), UsageError> {
    if invocation.has("--issue-code") {
        if !invocation.operands.is_empty() {
            return Err(UsageError::of(
                spec,
                "`alloy login --issue-code` takes no login code: it prints a new one.".to_owned(),
            ));
        }
        if invocation.has("--context") {
            return Err(UsageError::of(
                spec,
                "`--context` is not taken with `--issue-code`, which uses the current \
                 context."
                    .to_owned(),
            ));
        }
        return Ok(());
    }
    for flag in ["--user", "--org"] {
        if invocation.has(flag) {
            return Err(UsageError::of(
                spec,
                format!(
                    "`{flag}` is taken only with `--issue-code`, as root on a control node \
                     with no current context."
                ),
            ));
        }
    }
    decode(spec, invocation)?;
    context_name(spec, invocation, None)?;
    Ok(())
}

/// The login code of the command line.
fn decode(spec: &CommandSpec, invocation: &Invocation) -> Result<LoginCode, UsageError> {
    let Some(text) = invocation.operands.first() else {
        return Err(UsageError::of(
            spec,
            "`alloy login` needs a login code: alloy init printed one, and \
             `sudo alloy login --issue-code` on a control node prints a new one."
                .to_owned(),
        ));
    };
    // The code is a secret: no message echoes it (ARFC 0019 clause 3.1.13).
    LoginCode::decode(text)
        .map_err(|e| UsageError::of(spec, format!("This is not a valid login code: {e}.")))
}

/// The context to log in to: `--context`, else the name `endpoint` gives.
fn context_name(
    spec: &CommandSpec,
    invocation: &Invocation,
    endpoint: Option<SocketAddr>,
) -> Result<String, UsageError> {
    match invocation.value("--context") {
        Some(name) if context_name_ok(name) => Ok(name.to_owned()),
        Some(name) => Err(UsageError::of(
            spec,
            format!(
                "`{}` cannot name a context: a context name is 1 to 255 characters with \
                 no space, quote, or control character.",
                escape(name)
            ),
        )),
        None => Ok(endpoint.map(default_context_name).unwrap_or_default()),
    }
}

/// `alloy login`.
pub(crate) async fn login(
    ctx: &Ctx<'_>,
    spec: &CommandSpec,
    invocation: &Invocation,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    check(spec, invocation)?;
    if invocation.has("--issue-code") {
        return issue_code(ctx, spec, invocation, out, err).await;
    }
    let code = decode(spec, invocation)?;
    let name = context_name(spec, invocation, code.endpoints.first().copied())?;
    let store = store(ctx)?;
    let mut contexts = store.contexts().await?;
    let mut credentials = store.credentials().await?;
    let replaces = contexts
        .get(&name)
        .map(|context| context.access_key.clone())
        .unwrap_or_default();

    note(err, &format!("Connecting to {}\n", listed(&code.endpoints)));
    let client = reach(ctx, &code.endpoints, code.api_ca_fp, &Credential::None)
        .await
        .map_err(|why| unreached(&why, &code.endpoints, "the login code"))?;
    note(
        err,
        &format!(
            "  {}    API certificate matches the login code\n",
            ctx.styles.err.green("ok")
        ),
    );
    let key_name = key_name(&hostname(ctx).await);
    let mut response = client
        .exchange_login_code(ExchangeLoginCodeRequest {
            code_id: code.code_id.0.to_vec(),
            secret: code.secret.as_bytes().to_vec(),
            name: key_name,
            replaces: replaces.clone(),
        })
        .await
        .map_err(|e| {
            let failure = api_failure(&e);
            if failure.code == EXIT_DENIED && failure.try_command.is_none() {
                failure.with_try("sudo alloy login --issue-code")
            } else {
                failure
            }
        })?;
    let secret = Zeroizing::new(std::mem::take(&mut response.secret));

    // The key first, so that no saved context names a key the store lacks.
    if !replaces.is_empty() && replaces != response.access_key_id {
        credentials.remove(&replaces);
    }
    credentials.put(&response.access_key_id, secret);
    store.save_credentials(&credentials).await?;
    contexts.put_current(Context {
        name: name.clone(),
        endpoints: code.endpoints.clone(),
        api_ca_fp: code.api_ca_fp,
        cluster_id: *code.cluster_id.as_bytes(),
        user: response.user.clone(),
        org: response.org.clone(),
        project: response.project.clone(),
        access_key: response.access_key_id.clone(),
    });
    store.save_contexts(&contexts).await?;

    emit(
        out,
        &format!(
            "Logged in as {} in organization {}, with the access key {}.\n",
            escape(&response.user),
            escape(&response.org),
            escape(&response.name)
        ),
    )?;
    let project = if response.project.is_empty() {
        "no project".to_owned()
    } else {
        format!("project {}", escape(&response.project))
    };
    note(
        err,
        &format!(
            "Saved context {} and made it current. Its access key is in {}, which \
             only you can read.\nDefaults: organization {}, {project}.\nNext: {}\n",
            escape(&name),
            shown(&store.path_of(CREDENTIALS)),
            escape(&response.org),
            ctx.styles.err.bold("alloy node list"),
        ),
    );
    Ok(())
}

/// `alloy login --issue-code`: for the current context's user through the
/// API port, or, with no current context, through the local admin socket
/// for `--user` in `--org` (ARFC 0019 clause 3.1.14). The code is printed
/// once, on standard output, and nowhere else (clause 3.1.13).
async fn issue_code(
    ctx: &Ctx<'_>,
    spec: &CommandSpec,
    invocation: &Invocation,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let store = store(ctx)?;
    let contexts = store.contexts().await?;
    let secret = CodeSecret::generate(ctx.runtime.rng().as_ref()).map_err(|e| {
        Failure::new(
            EXIT_FAILURE,
            format!("alloy could not draw the login code's secret: {e}."),
        )
    })?;
    let (client, user, org) = match contexts.current() {
        Some(context) => {
            for flag in ["--user", "--org"] {
                if invocation.has(flag) {
                    return Err(UsageError::of(
                        spec,
                        format!(
                            "`{flag}` is taken only as root on a control node with no \
                             current context. With one, the code is for its user, {}.",
                            escape(&context.user)
                        ),
                    )
                    .into());
                }
            }
            let client = signed_client(ctx, &store, context).await?;
            (client, context.user.clone(), context.org.clone())
        }
        None => {
            let client = local_admin(ctx).await?;
            let user = invocation
                .value("--user")
                .unwrap_or(DEFAULT_USER)
                .to_owned();
            let org = invocation.value("--org").unwrap_or(DEFAULT_ORG).to_owned();
            (client, user, org)
        }
    };
    let response = client
        .issue_login_code(IssueLoginCodeRequest {
            user: user.clone(),
            org: org.clone(),
            secret_hash: secret.hash().to_vec(),
        })
        .await
        .map_err(|e| api_failure(&e))?;
    let code = issued_code(&response, secret)?;
    let text = Zeroizing::new(code.encode().map_err(|e| malformed(&e.to_string()))?);
    emit(out, &text)?;
    emit(out, "\n")?;
    note(
        err,
        &format!(
            "This login code logs one computer in as {} in organization {}, once, \
             within 24 hours. Run alloy login with it on that computer.\nNext: {}\n",
            escape(&user),
            escape(&org),
            ctx.styles.err.bold("alloy login <login code>"),
        ),
    );
    Ok(())
}

/// A connection with the access key of `context`, which `contexts` holds.
///
/// # Errors
///
/// A [`Failure`] when the key is not stored or no endpoint answers.
pub(crate) async fn signed_client(
    ctx: &Ctx<'_>,
    store: &Store,
    context: &Context,
) -> Result<Client, Failure> {
    let credentials = store.credentials().await?;
    let Some(secret) = credentials.get(&context.access_key) else {
        return Err(Failure::new(
            EXIT_DENIED,
            format!(
                "The access key of context {} is not in {}, so alloy cannot call the \
                 cluster's API with it. Log in to the context again with a new login code.",
                escape(&context.name),
                shown(&store.path_of(CREDENTIALS))
            ),
        )
        .with_try(format!(
            "alloy login <login code> --context {}",
            quoted(&context.name)
        )));
    };
    let credential = Credential::AccessKey(AccessKeyCredential::new(
        &context.access_key,
        secret.clone(),
    ));
    reach(ctx, &context.endpoints, context.api_ca_fp, &credential)
        .await
        .map_err(|why| {
            unreached(
                &why,
                &context.endpoints,
                &format!("context {}", escape(&context.name)),
            )
        })
}

/// A connection to the local admin socket (ARFC 0008 clause 3.7.2). A
/// machine without one, and a user the socket refuses, are not logged in
/// (ARFC 0019 clause 3.1.14).
pub(crate) async fn local_admin(ctx: &Ctx<'_>) -> Result<Client, Failure> {
    let Some(address) = ctx.env.admin_socket.clone() else {
        return Err(not_logged_in());
    };
    let stream = ctx
        .runtime
        .net()
        .connect(address, StreamOptions::default())
        .await
        .map_err(|_| not_logged_in())?;
    local_admin_over(ctx.runtime, stream)
        .await
        .map_err(|_| not_logged_in())
}

fn malformed(reason: &str) -> Failure {
    Failure::new(
        EXIT_FAILURE,
        format!("The control plane returned a login code that alloy cannot use: {reason}."),
    )
}

/// The login code `IssueLoginCode` returned the fields of, with `secret`.
fn issued_code(
    response: &IssueLoginCodeResponse,
    secret: CodeSecret,
) -> Result<LoginCode, Failure> {
    let cluster_id: [u8; 16] = response
        .cluster_id
        .as_slice()
        .try_into()
        .map_err(|_| malformed("its cluster id is not 16 bytes"))?;
    let api_ca_fp: [u8; 16] = response
        .api_ca_fp
        .as_slice()
        .try_into()
        .map_err(|_| malformed("its API CA fingerprint is not 16 bytes"))?;
    let code_id: [u8; 8] = response
        .code_id
        .as_slice()
        .try_into()
        .map_err(|_| malformed("its code id is not 8 bytes"))?;
    if !(1..=4).contains(&response.endpoints.len()) {
        return Err(malformed("it does not list 1 to 4 endpoints"));
    }
    let mut endpoints = Vec::with_capacity(response.endpoints.len());
    for endpoint in &response.endpoints {
        let ip = if let Ok(v4) = <[u8; 4]>::try_from(endpoint.address.as_slice()) {
            IpAddr::V4(Ipv4Addr::from(v4))
        } else if let Ok(v6) = <[u8; 16]>::try_from(endpoint.address.as_slice()) {
            IpAddr::V6(Ipv6Addr::from(v6))
        } else {
            return Err(malformed("an endpoint's address is not 4 or 16 bytes"));
        };
        let port = u16::try_from(endpoint.port)
            .ok()
            .filter(|&port| port != 0)
            .ok_or_else(|| malformed("an endpoint's port is not 1 to 65535"))?;
        endpoints.push(SocketAddr::new(ip, port));
    }
    Ok(LoginCode {
        cluster_id: ClusterId::from_bytes(cluster_id),
        api_ca_fp,
        code_id: CodeId(code_id),
        secret,
        endpoints,
    })
}

/// The name a new access key asks for: the hostname by the rule of ARFC
/// 0007 clause 3.9.3, lower case, cut at the first dot, every character
/// outside `[a-z0-9-]` replaced by `-`, prefixed `key-` when it does not
/// start with a letter, and cut to 63 characters.
pub(crate) fn key_name(hostname: &str) -> String {
    let lower = hostname.trim().to_lowercase();
    let head = lower.split('.').next().unwrap_or_default();
    let mut name: String = head
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name.insert_str(0, "key-");
    }
    name.chars().take(MAX_KEY_NAME).collect()
}

/// This computer's hostname: `COMPUTERNAME` on Windows, else what the
/// kernel reports in `/proc/sys/kernel/hostname`, else `/etc/hostname`, else
/// `HOSTNAME`, else [`FALLBACK_KEY_NAME`].
pub(crate) async fn hostname(ctx: &Ctx<'_>) -> String {
    let var = |name: &str| {
        ctx.env
            .var(name)
            .and_then(|value| value.to_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    if ctx.env.windows
        && let Some(name) = var("COMPUTERNAME")
    {
        return name;
    }
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Some(name) = read_small(ctx, path).await {
            return name;
        }
    }
    var("HOSTNAME").unwrap_or_else(|| FALLBACK_KEY_NAME.to_owned())
}

/// The first line of the small file at `path`, trimmed, when it can be
/// read and is not empty.
async fn read_small(ctx: &Ctx<'_>, path: &str) -> Option<String> {
    let disk = ctx.runtime.disk_io();
    let path = RtPath::new(path).ok()?;
    let options = OpenOptions {
        access: Access::Read,
        create: Create::Never,
        direct: false,
        exclusive: false,
    };
    let file = disk.open(&path, options).await.ok()?;
    let buf = alloy_rt::IoBuf::zeroed(256).ok()?;
    let mut done = file.read_at(vec![ReadOp { offset: 0, buf }]).await;
    let read = done.pop()?;
    let count = match read.result {
        Ok(count) => count,
        Err(_) => return None,
    };
    let text = std::str::from_utf8(read.buf.get(..count)?).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// `word` as one shell word that runs as printed in a POSIX shell and in
/// PowerShell: bare when it holds only characters neither treats
/// specially, else in single quotes, which a context name never holds.
pub(crate) fn quoted(word: &str) -> String {
    let plain = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/@%+=,".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{word}'")
    }
}

#[cfg(test)]
mod tests {
    use alloy_proto::iam::v1alpha1::Endpoint;

    use super::*;

    #[test]
    fn a_key_name_follows_clause_3_9_3() {
        assert_eq!(key_name("Laptop.home.arpa"), "laptop");
        assert_eq!(key_name("DESKTOP-4F2K9QZ"), "desktop-4f2k9qz");
        assert_eq!(key_name("my_box"), "my-box");
        assert_eq!(key_name("1node"), "key-1node");
        assert_eq!(key_name("-x"), "key--x");
        assert_eq!(key_name("é"), "key--");
        assert_eq!(key_name(""), "key-");
        assert_eq!(key_name(&"a".repeat(80)).len(), 63);
        assert_eq!(key_name(&"1".repeat(80)), format!("key-{}", "1".repeat(59)));
    }

    #[test]
    fn a_context_name_is_quoted_only_when_it_must_be() {
        assert_eq!(quoted("192.168.1.10"), "192.168.1.10");
        assert_eq!(quoted("192.168.1.10:8443"), "192.168.1.10:8443");
        assert_eq!(quoted("[fd00::1]:8443"), "'[fd00::1]:8443'");
        assert_eq!(quoted("a*b"), "'a*b'");
    }

    fn response(endpoints: Vec<Endpoint>) -> IssueLoginCodeResponse {
        IssueLoginCodeResponse {
            cluster_id: vec![1; 16],
            api_ca_fp: vec![2; 16],
            code_id: vec![3; 8],
            endpoints,
            expire_time: None,
        }
    }

    #[test]
    fn an_issued_code_is_the_response_and_the_secret() {
        let endpoints = vec![
            Endpoint {
                address: vec![192, 168, 1, 10],
                port: 443,
            },
            Endpoint {
                address: Ipv6Addr::LOCALHOST.octets().to_vec(),
                port: 8443,
            },
        ];
        let code = issued_code(&response(endpoints), CodeSecret::from_bytes([4; 16])).unwrap();
        let back = LoginCode::decode(&code.encode().unwrap()).unwrap();
        assert_eq!(back.cluster_id.as_bytes(), &[1; 16]);
        assert_eq!(back.api_ca_fp, [2; 16]);
        assert_eq!(back.code_id.0, [3; 8]);
        assert_eq!(back.secret.as_bytes(), &[4; 16]);
        assert_eq!(
            back.endpoints,
            [
                "192.168.1.10:443".parse::<SocketAddr>().unwrap(),
                "[::1]:8443".parse().unwrap()
            ]
        );
        let secret = || CodeSecret::from_bytes([4; 16]);
        let bad_address = Endpoint {
            address: vec![1, 2, 3],
            port: 443,
        };
        assert!(issued_code(&response(vec![bad_address]), secret()).is_err());
        let bad_port = Endpoint {
            address: vec![1, 2, 3, 4],
            port: 70_000,
        };
        assert!(issued_code(&response(vec![bad_port]), secret()).is_err());
        assert!(issued_code(&response(vec![]), secret()).is_err());
        let mut short = response(vec![Endpoint {
            address: vec![1, 2, 3, 4],
            port: 1,
        }]);
        short.code_id = vec![3; 7];
        let e = issued_code(&short, secret()).unwrap_err();
        assert_eq!(e.code, EXIT_FAILURE);
        assert!(e.message.contains("code id"), "{e:?}");
    }
}
