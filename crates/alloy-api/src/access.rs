//! Authentication and authorization of API calls (ARFC 0008 clauses 3.7 to
//! 3.7.2).
//!
//! A call over TLS authenticates with a session token in the header
//! `authorization: Bearer <token>`; a call through the local admin socket
//! takes no header and is a local admin call. The server checks the header's
//! form, and an [`Access`] implementation, the identity and access
//! subsystem, checks the token and authorizes each call.

use std::fmt;

use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_rt::BoxFuture;

/// The one message of every authentication failure: a call learns nothing
/// about why its credential failed (ARFC 0002 section 3.7, ARFC 0007 clause
/// 3.9.12).
pub const UNAUTHENTICATED_MESSAGE: &str = "The request carries no valid credential, so it was \
     refused. Log in again with a new login code.";

/// The longest session token the server passes to [`Access::authenticate`],
/// in bytes. A longer one fails as any token that does not authenticate.
pub const MAX_TOKEN_LEN: usize = 4_096;

/// A session token as it travels in the `authorization` header (ARFC 0007
/// section 2.3): 1 to [`MAX_TOKEN_LEN`] bytes of visible ASCII. Its `Debug`
/// output never shows it (I-22).
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl SessionToken {
    /// Accepts exactly the strings of 1 to [`MAX_TOKEN_LEN`] bytes, each from
    /// 0x21 to 0x7E.
    ///
    /// # Errors
    ///
    /// The unauthenticated error for any other string.
    pub fn new(token: &str) -> Result<SessionToken, Error> {
        let valid = (1..=MAX_TOKEN_LEN).contains(&token.len())
            && token.bytes().all(|b| (0x21..=0x7E).contains(&b));
        if valid {
            Ok(SessionToken(token.to_owned()))
        } else {
            Err(unauthenticated())
        }
    }

    /// The token, to be checked or sent. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(..)")
    }
}

/// Who makes a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// The node's local admin, `root@<node name>`, through the local admin
    /// socket (ARFC 0007 clause 3.10).
    LocalAdmin,
    /// A principal that a session token authenticated, by the name
    /// [`Access::authenticate`] gave it.
    Authenticated(String),
}

/// An action a call asks to take (ARFC 0007), named `<service>:<Method>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// `cluster:GetNode`.
    GetNode,
    /// `cluster:ListNodes`.
    ListNodes,
}

impl Action {
    /// The action's name, as in `cluster:GetNode`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Action::GetNode => "cluster:GetNode",
            Action::ListNodes => "cluster:ListNodes",
        }
    }
}

/// Why [`Access::authenticate`] did not authenticate a token.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthnError {
    /// The token does not authenticate, whatever the cause: the call fails
    /// with `Unauthenticated` and [`UNAUTHENTICATED_MESSAGE`].
    Refused,
    /// The token could not be checked, for example without quorum: the call
    /// fails with this error, which must not be `Unauthenticated`.
    Failed(Error),
}

/// The identity and access subsystem as the API server uses it.
///
/// Methods take `&self`, may be called concurrently, and never panic.
pub trait Access: Send + Sync + fmt::Debug + 'static {
    /// The principal `token` authenticates.
    ///
    /// # Errors
    ///
    /// [`AuthnError::Refused`] when it authenticates none.
    fn authenticate<'a>(
        &'a self,
        token: &'a SessionToken,
    ) -> BoxFuture<'a, Result<Principal, AuthnError>>;

    /// Whether `principal` may take `action` on the resource with full name
    /// `resource` (`nodes` for a whole collection).
    ///
    /// # Errors
    ///
    /// `PermissionDenied`, or any other error of ARFC 0008 clause 3.6, when
    /// the call must not go on.
    fn authorize<'a>(
        &'a self,
        principal: &'a Principal,
        action: Action,
        resource: &'a str,
    ) -> BoxFuture<'a, Result<(), Error>>;
}

/// The error of every authentication failure.
#[must_use]
pub fn unauthenticated() -> Error {
    Error::new(ErrorCode::Unauthenticated, UNAUTHENTICATED_MESSAGE).with_next_step(
        "alloy.iam.v1alpha1.IamService/ExchangeLoginCode",
        "alloy login <login code>",
    )
}

/// The session token of an `authorization` header value: `Bearer`, one space,
/// then the token.
///
/// # Errors
///
/// The unauthenticated error for any other value, an absent one included.
pub(crate) fn bearer(header: Option<&http::HeaderValue>) -> Result<SessionToken, Error> {
    let value = header
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(unauthenticated)?;
    SessionToken::new(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_prints() {
        let token = SessionToken::new("abc.def").expect("valid");
        assert_eq!(format!("{token:?}"), "SessionToken(..)");
    }

    #[test]
    fn bearer_accepts_only_its_form() {
        let ok = http::HeaderValue::from_static("Bearer abc.def");
        assert_eq!(bearer(Some(&ok)).expect("valid").expose(), "abc.def");
        for bad in ["bearer abc", "Bearer ", "Basic abc", "Bearer a b", "abc"] {
            let value = http::HeaderValue::from_static(bad);
            assert_eq!(bearer(Some(&value)), Err(unauthenticated()), "{bad}");
        }
        assert_eq!(bearer(None), Err(unauthenticated()));
        let long = "x".repeat(MAX_TOKEN_LEN + 1);
        assert!(SessionToken::new(&long).is_err());
    }
}
