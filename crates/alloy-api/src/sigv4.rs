//! SigV4 on gRPC (ARFC 0008 clause 3.7.1): the client signs each call with
//! an access key, and the server verifies the signature, the body hash, and
//! the time.
//!
//! The values follow AWS Signature Version 4
//! (<https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html>):
//! method `POST`, the gRPC path as canonical URI, an empty query string, the
//! signed headers `content-type;host;x-amz-content-sha256;x-amz-date`,
//! `host` from the `:authority` pseudo-header, the lower case hexadecimal
//! SHA-256 of the whole body as hashed payload, and the scope
//! `<YYYYMMDD>/global/alloy/aws4_request`.

use std::fmt;
use std::time::Duration;

use alloy_crypto::Zeroizing;
use alloy_crypto::sha256::{equal, hmac_sha256, sha256};
use alloy_rt::WallTime;

/// The algorithm name that starts the `authorization` header.
pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The header that holds the request time, `YYYYMMDDTHHMMSSZ`.
pub const DATE_HEADER: &str = "x-amz-date";

/// The header that holds the SHA-256 of the body.
pub const CONTENT_HEADER: &str = "x-amz-content-sha256";

/// The signed headers, in the order SigV4 lists them.
pub const SIGNED_HEADERS: &str = "content-type;host;x-amz-content-sha256;x-amz-date";

/// The region and service of the credential scope.
const REGION: &str = "global";
const SERVICE: &str = "alloy";
const TERMINATOR: &str = "aws4_request";

/// How far a request's time may be from the server's clock.
pub const MAX_SKEW: Duration = Duration::from_secs(5 * 60);

/// The longest access key id the server looks up, in bytes.
const MAX_KEY_ID: usize = 128;

/// An access key as the client holds it: its id and secret. Its `Debug`
/// output shows the id and never the secret (I-22).
#[derive(Clone)]
pub struct AccessKeyCredential {
    id: String,
    secret: Zeroizing<String>,
}

impl AccessKeyCredential {
    /// The key with id `id` and secret `secret`.
    #[must_use]
    pub fn new(id: &str, secret: Zeroizing<String>) -> AccessKeyCredential {
        AccessKeyCredential {
            id: id.to_owned(),
            secret,
        }
    }

    /// The key's id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl fmt::Debug for AccessKeyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessKeyCredential")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// What the `authorization` header of a signed call names, before any of it
/// is checked against a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Authorization {
    pub(crate) key_id: String,
    pub(crate) date: String,
    pub(crate) signature: String,
}

/// The parts of a call that the signature covers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Signed<'a> {
    /// The gRPC path.
    pub(crate) path: &'a str,
    /// The `content-type` header.
    pub(crate) content_type: &'a str,
    /// The `:authority`.
    pub(crate) host: &'a str,
    /// The `x-amz-date` header.
    pub(crate) amz_date: &'a str,
    /// The `x-amz-content-sha256` header.
    pub(crate) content_sha256: &'a str,
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

/// The lower case hexadecimal SHA-256 of `body`.
#[must_use]
pub(crate) fn content_sha256(body: &[u8]) -> String {
    hex(&sha256(body))
}

/// The civil date and time of `seconds` since 1970 in UTC, as
/// `(year, month, day, hour, minute, second)`, by the days-from-civil
/// inverse of the proleptic Gregorian calendar.
fn civil(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    // Each value is within its field's range by construction, so the
    // conversions keep it.
    let small = |v: i64| u32::try_from(v).unwrap_or(0);
    (
        year,
        small(month),
        small(day),
        small(of_day / 3_600),
        small(of_day % 3_600 / 60),
        small(of_day % 60),
    )
}

/// The seconds since 1970 of a civil date and time in UTC.
fn seconds_of(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second)
}

/// `time` as `YYYYMMDDTHHMMSSZ`.
#[must_use]
pub(crate) fn amz_date(time: WallTime) -> String {
    let (y, mo, d, h, mi, s) = civil(time.0.div_euclid(1_000_000_000));
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// The seconds since 1970 that `text`, `YYYYMMDDTHHMMSSZ`, names, or `None`
/// when it is not spelled so or names no valid time.
pub(crate) fn parse_amz_date(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let digits = |from: usize, to: usize| -> Option<u32> {
        let part = text.get(from..to)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (year, month, day) = (digits(0, 4)?, digits(4, 6)?, digits(6, 8)?);
    let (hour, minute, second) = (digits(9, 11)?, digits(11, 13)?, digits(13, 15)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59
    {
        return None;
    }
    if second > 59 {
        return None;
    }
    let at = seconds_of(i64::from(year), month, day, hour, minute, second);
    // A day past the month's end names another date.
    let (y, mo, d, ..) = civil(at);
    (y == i64::from(year) && mo == month && d == day).then_some(at)
}

/// The credential scope of a request at `date`, `YYYYMMDD`.
fn scope(date: &str) -> String {
    format!("{date}/{REGION}/{SERVICE}/{TERMINATOR}")
}

/// The signature of `signed` under the key `secret`, lower case hexadecimal.
pub(crate) fn signature(secret: &str, signed: &Signed<'_>) -> String {
    let date = signed.amz_date.get(..8).unwrap_or_default();
    let canonical = format!(
        "POST\n{path}\n\ncontent-type:{ct}\nhost:{host}\n{CONTENT_HEADER}:{sha}\n{DATE_HEADER}:{at}\n\n{SIGNED_HEADERS}\n{sha}",
        path = signed.path,
        ct = signed.content_type.trim(),
        host = signed.host.trim(),
        sha = signed.content_sha256,
        at = signed.amz_date,
    );
    let to_sign = format!(
        "{ALGORITHM}\n{at}\n{scope}\n{hash}",
        at = signed.amz_date,
        scope = scope(date),
        hash = hex(&sha256(canonical.as_bytes())),
    );
    let key = signing_key(secret, date);
    hex(&hmac_sha256(&key[..], to_sign.as_bytes()))
}

/// The SigV4 signing key of `secret` on `date`, `YYYYMMDD`.
fn signing_key(secret: &str, date: &str) -> Zeroizing<[u8; 32]> {
    let mut first = Zeroizing::new(String::with_capacity(4 + secret.len()));
    first.push_str("AWS4");
    first.push_str(secret);
    let k_date = Zeroizing::new(hmac_sha256(first.as_bytes(), date.as_bytes()));
    let k_region = Zeroizing::new(hmac_sha256(&k_date[..], REGION.as_bytes()));
    let k_service = Zeroizing::new(hmac_sha256(&k_region[..], SERVICE.as_bytes()));
    Zeroizing::new(hmac_sha256(&k_service[..], TERMINATOR.as_bytes()))
}

/// The `authorization` header value of a call signed with `key`.
#[must_use]
pub(crate) fn authorization(key: &AccessKeyCredential, signed: &Signed<'_>) -> String {
    let date = signed.amz_date.get(..8).unwrap_or_default();
    format!(
        "{ALGORITHM} Credential={id}/{scope}, SignedHeaders={SIGNED_HEADERS}, Signature={sig}",
        id = key.id,
        scope = scope(date),
        sig = signature(&key.secret, signed),
    )
}

/// The parts of an `authorization` header value that starts with
/// [`ALGORITHM`], or `None` when it is not one SigV4 header of exactly the
/// form clause 3.7.1 states.
pub(crate) fn parse_authorization(value: &str) -> Option<Authorization> {
    let rest = value.strip_prefix(ALGORITHM)?.strip_prefix(' ')?;
    let mut parts = rest.split(", ");
    let credential = parts.next()?.strip_prefix("Credential=")?;
    let signed_headers = parts.next()?.strip_prefix("SignedHeaders=")?;
    let signature = parts.next()?.strip_prefix("Signature=")?;
    if parts.next().is_some() || signed_headers != SIGNED_HEADERS {
        return None;
    }
    let (key_id, scope_text) = credential.split_once('/')?;
    if key_id.is_empty() || key_id.len() > MAX_KEY_ID {
        return None;
    }
    let date = scope_text.get(..8)?;
    if scope_text != scope(date) || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hex_ok = signature.len() == 64
        && signature
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex_ok {
        return None;
    }
    Some(Authorization {
        key_id: key_id.to_owned(),
        date: date.to_owned(),
        signature: signature.to_owned(),
    })
}

/// Whether a call verifies: its `x-amz-date` is within [`MAX_SKEW`] of
/// `now` and on the scope's date, its body hashes to `x-amz-content-sha256`,
/// and its signature is the one `secret` makes. Each check runs whatever the
/// others find, and the signatures are compared in constant time.
pub(crate) fn verify(
    secret: &str,
    authorization: &Authorization,
    signed: &Signed<'_>,
    body: &[u8],
    now: WallTime,
) -> bool {
    let now_secs = now.0.div_euclid(1_000_000_000);
    let timely = parse_amz_date(signed.amz_date).is_some_and(|at| {
        at.abs_diff(now_secs) <= MAX_SKEW.as_secs()
            && signed.amz_date.get(..8) == Some(authorization.date.as_str())
    });
    let hashed = equal(content_sha256(body).as_bytes(), signed.content_sha256.as_bytes());
    let expected = signature(secret, signed);
    let signed_ok = equal(expected.as_bytes(), authorization.signature.as_bytes());
    timely & hashed & signed_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivation example of the AWS documentation ("Examples of how to
    /// derive a signing key for Signature Version 4"), with that example's
    /// region and service in place of Alloy's.
    #[test]
    fn the_signing_key_follows_the_aws_example() {
        let secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
        let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), b"20120215");
        let k_region = hmac_sha256(&k_date, b"us-east-1");
        let k_service = hmac_sha256(&k_region, b"iam");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        assert_eq!(
            hex(&k_date),
            "969fbb94feb542b71ede6f87fe4d5fa29c789342b0f407474670f0c2489e0a0d"
        );
        assert_eq!(
            hex(&k_signing),
            "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
        );
    }

    fn at(seconds: i64) -> WallTime {
        WallTime(seconds * 1_000_000_000)
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(amz_date(at(0)), "19700101T000000Z");
        assert_eq!(amz_date(at(951_782_400)), "20000229T000000Z");
        assert_eq!(amz_date(at(1_329_264_000)), "20120215T000000Z");
        assert_eq!(amz_date(at(4_102_444_799)), "20991231T235959Z");
        for s in [0, 951_782_400, 1_700_000_123, 4_102_444_799] {
            assert_eq!(parse_amz_date(&amz_date(at(s))), Some(s));
        }
        for bad in [
            "20230230T000000Z",
            "20231301T000000Z",
            "20230101T240000Z",
            "20230101 000000Z",
            "2023010T0000000Z",
            "+0230101T000000Z",
        ] {
            assert_eq!(parse_amz_date(bad), None, "{bad}");
        }
    }

    fn signed<'a>(date: &'a str, sha: &'a str) -> Signed<'a> {
        Signed {
            path: "/alloy.cluster.v1alpha1.ClusterService/ListNodes",
            content_type: "application/grpc",
            host: "192.0.2.1:443",
            amz_date: date,
            content_sha256: sha,
        }
    }

    #[test]
    fn a_signed_call_verifies_and_every_change_fails() {
        let key = AccessKeyCredential::new("AKEXAMPLE", Zeroizing::new("secret".to_owned()));
        let now = at(1_700_000_000);
        let date = amz_date(now);
        let body = b"\0\0\0\0\0";
        let sha = content_sha256(body);
        let header = authorization(&key, &signed(&date, &sha));
        let parsed = parse_authorization(&header).expect("parses");
        assert_eq!(parsed.key_id, "AKEXAMPLE");
        assert!(verify("secret", &parsed, &signed(&date, &sha), body, now));
        assert!(!verify("other", &parsed, &signed(&date, &sha), body, now));
        assert!(!verify("secret", &parsed, &signed(&date, &sha), b"x", now));
        let mut moved = signed(&date, &sha);
        moved.path = "/alloy.iam.v1alpha1.IamService/IssueLoginCode";
        assert!(!verify("secret", &parsed, &moved, body, now));
        let late = at(1_700_000_000 + 301);
        assert!(!verify("secret", &parsed, &signed(&date, &sha), body, late));
        let skewed = at(1_700_000_000 + 300);
        assert!(verify("secret", &parsed, &signed(&date, &sha), body, skewed));
        assert!(format!("{key:?}").contains("AKEXAMPLE"));
        assert!(!format!("{key:?}").contains("secret\""));
    }

    #[test]
    fn only_the_stated_header_form_parses() {
        let sig = "a".repeat(64);
        let good = format!(
            "{ALGORITHM} Credential=K/20240101/global/alloy/aws4_request, SignedHeaders={SIGNED_HEADERS}, Signature={sig}"
        );
        assert!(parse_authorization(&good).is_some());
        for bad in [
            good.replace("global", "us-east-1"),
            good.replace(SIGNED_HEADERS, "host;x-amz-date"),
            good.replace(&sig, &"A".repeat(64)),
            good.replace("Credential=K/", "Credential=/"),
            format!("{good}, Extra=1"),
            good.replace("AWS4-HMAC-SHA256 ", "AWS4-HMAC-SHA256  "),
            "Bearer abc".to_owned(),
        ] {
            assert!(parse_authorization(&bad).is_none(), "{bad}");
        }
    }
}
