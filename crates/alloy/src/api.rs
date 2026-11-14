//! Reaching the API: the endpoints of a login code or a context, tried in
//! order for at most 10 seconds in all (ARFC 0002 section 3.7, ARFC 0019
//! clause 3.1.11), and the API's errors as the CLI prints them.

use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use alloy_api::client::{Client, ConnectError, Credential, connect};
use alloy_api::tls::ApiCaFingerprint;
use alloy_proto::common::v1alpha1::Error;
use alloy_rt::Runtime;

use crate::text::escape;
use crate::{Ctx, EXIT_FAILURE, EXIT_UNAVAILABLE, Failure};

/// How long `alloy` tries the endpoints, in all (ARFC 0002 section 3.7).
pub(crate) const REACH_LIMIT: Duration = Duration::from_secs(10);

/// Why no endpoint gave a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unreached {
    /// An endpoint presented a chain that does not verify to the pinned API
    /// CA, or does not name its address, and no endpoint after it answered.
    /// Nothing was sent to it.
    Mismatch(SocketAddr),
    /// No endpoint answered within [`REACH_LIMIT`].
    Silent,
}

/// A connection to the first of `endpoints` that answers with a chain that
/// verifies to `api_ca_fp`, trying each in order for at most
/// [`REACH_LIMIT`] in all, with each call carrying `credential`.
///
/// # Errors
///
/// [`Unreached`].
pub(crate) async fn reach(
    ctx: &Ctx<'_>,
    endpoints: &[SocketAddr],
    api_ca_fp: [u8; 16],
    credential: &Credential,
) -> Result<Client, Unreached> {
    let mismatch = std::cell::Cell::new(None);
    let attempts = async {
        for &endpoint in endpoints {
            match connect(
                ctx.runtime,
                ctx.transport,
                endpoint,
                ApiCaFingerprint(api_ca_fp),
                credential,
            )
            .await
            {
                Ok(client) => return Some(client),
                Err(ConnectError::Certificate(_)) => mismatch.set(Some(endpoint)),
                Err(_) => {}
            }
        }
        None
    };
    let client = within(ctx.runtime, REACH_LIMIT, attempts).await.flatten();
    match (client, mismatch.get()) {
        (Some(client), _) => Ok(client),
        (None, Some(endpoint)) => Err(Unreached::Mismatch(endpoint)),
        (None, None) => Err(Unreached::Silent),
    }
}

/// `future`'s output, or `None` when `limit` passes first on the runtime's
/// clock. Unlike [`Runtime::timeout`] it needs no `Send` future.
pub(crate) async fn within<F: Future>(
    runtime: &Runtime,
    limit: Duration,
    future: F,
) -> Option<F::Output> {
    let mut future = pin!(future);
    let mut timer = runtime.sleep(limit);
    poll_fn(|cx| {
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        timer.as_mut().poll(cx).map(|()| None)
    })
    .await
}

/// The endpoints, for a message: `192.168.1.10:443, [fd00::1]:443`.
pub(crate) fn listed(endpoints: &[SocketAddr]) -> String {
    endpoints
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What [`Unreached`] means for a command reaching `endpoints`, which `what`
/// names, as in `the login code`: a certificate mismatch exits 1 with no
/// `Try:` line, since no command makes it safe, and silence exits 6 (ARFC
/// 0019 clause 3.1.11).
pub(crate) fn unreached(why: &Unreached, endpoints: &[SocketAddr], what: &str) -> Failure {
    match why {
        Unreached::Mismatch(endpoint) => Failure::new(
            EXIT_FAILURE,
            format!(
                "The API at {endpoint} presented a certificate authority other than \
                 the one {what} pins, so alloy sent it nothing. Another machine may \
                 answer at that address now, or something between this computer and \
                 the cluster answers in its place."
            ),
        ),
        Unreached::Silent => Failure::new(
            EXIT_UNAVAILABLE,
            format!(
                "No API address of {what} answered within 10 seconds: {}. alloyd may \
                 be stopped on the control nodes, or a firewall may block the API \
                 port.",
                listed(endpoints)
            ),
        ),
    }
}

/// An API error as the CLI prints it (ARFC 0019 clause 3.1.8): its message,
/// then its next step's command on a `Try:` line when it has one, with its
/// exit code (ARFC 0008 clause 3.6).
pub(crate) fn api_failure(error: &Error) -> Failure {
    let failure = Failure::new(error.code().exit_code(), message_of(error));
    match error.next_step.as_ref().map(|step| step.command.as_str()) {
        Some(command) if !command.is_empty() => failure.with_try(escape(command)),
        _ => failure,
    }
}

/// The message of an API error, ending in a full stop, control characters
/// escaped.
fn message_of(error: &Error) -> String {
    let message = escape(error.message.trim());
    if message.is_empty() {
        format!(
            "The API failed the call with the code {}, and no message.",
            error.code().as_str_name()
        )
    } else if message.ends_with(['.', '!', '?']) {
        message
    } else {
        format!("{message}.")
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use alloy_proto::common::v1alpha1::ErrorCode;

    use super::*;
    use crate::{EXIT_DENIED, EXIT_NOT_FOUND};

    #[test]
    fn api_errors_keep_their_exit_codes_and_next_steps() {
        let denied = api_failure(&Error::new(ErrorCode::PermissionDenied, "No"));
        assert_eq!(denied.code, EXIT_DENIED);
        assert_eq!(denied.message, "No.");
        assert_eq!(denied.try_command, None);
        let missing = api_failure(
            &Error::new(ErrorCode::NotFound, "No node is named x.")
                .with_next_step("ListNodes", "alloy node list"),
        );
        assert_eq!(missing.code, EXIT_NOT_FOUND);
        assert_eq!(missing.try_command.as_deref(), Some("alloy node list"));
        let raw = api_failure(&Error::new(ErrorCode::Internal, "bad \u{1b}[2J"));
        assert_eq!(raw.message, "bad \\u{1b}[2J.");
        let empty = api_failure(&Error::new(ErrorCode::Unavailable, ""));
        assert_eq!(empty.code, EXIT_UNAVAILABLE);
        assert!(
            empty.message.contains("ERROR_CODE_UNAVAILABLE"),
            "{empty:?}"
        );
    }

    #[test]
    fn an_unreached_api_exits_as_clause_3_1_11_says() {
        let endpoints = [
            SocketAddr::from((Ipv4Addr::new(192, 168, 1, 10), 443)),
            SocketAddr::from((Ipv4Addr::new(192, 168, 1, 11), 8443)),
        ];
        let mismatch = unreached(
            &Unreached::Mismatch(endpoints[1]),
            &endpoints,
            "the login code",
        );
        assert_eq!(mismatch.code, EXIT_FAILURE);
        assert_eq!(mismatch.try_command, None);
        assert!(
            mismatch
                .message
                .starts_with("The API at 192.168.1.11:8443 presented")
        );
        let silent = unreached(&Unreached::Silent, &endpoints, "the login code");
        assert_eq!(silent.code, EXIT_UNAVAILABLE);
        assert!(
            silent
                .message
                .contains("within 10 seconds: 192.168.1.10:443, 192.168.1.11:8443."),
            "{silent:?}"
        );
    }
}
