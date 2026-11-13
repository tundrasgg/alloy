//! Errors on the wire (ARFC 0008 clause 3.6).
//!
//! An [`Error`] travels as the call's gRPC status: the status code is the one
//! its [`ErrorCode`] maps to, `grpc-message` is its `message`, and the binary
//! trailer [`ERROR_TRAILER`] holds the encoded `Error`.

use std::time::Duration;

use prost::Message;
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::{Code, Status};

use crate::common::v1alpha1::{Error, ErrorCode, FieldViolation, NextStep};

/// The binary trailer that carries the encoded [`Error`].
pub const ERROR_TRAILER: &str = "alloy-error-bin";

impl ErrorCode {
    /// The gRPC status of the code, from the table of ARFC 0008 clause 3.6.
    /// [`ErrorCode::Unspecified`] is never sent and maps as
    /// [`ErrorCode::Internal`].
    #[must_use]
    pub fn grpc_code(self) -> Code {
        match self {
            ErrorCode::InvalidArgument => Code::InvalidArgument,
            ErrorCode::Unauthenticated => Code::Unauthenticated,
            ErrorCode::PermissionDenied => Code::PermissionDenied,
            ErrorCode::NotFound => Code::NotFound,
            ErrorCode::Conflict => Code::Aborted,
            ErrorCode::QuotaExceeded | ErrorCode::RateLimited => Code::ResourceExhausted,
            ErrorCode::Compacted => Code::OutOfRange,
            ErrorCode::Unavailable => Code::Unavailable,
            ErrorCode::Internal | ErrorCode::Unspecified => Code::Internal,
        }
    }

    /// The HTTP status of the code in the JSON form, from the table of ARFC
    /// 0008 clause 3.6.
    #[must_use]
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::InvalidArgument => 400,
            ErrorCode::Unauthenticated => 401,
            ErrorCode::PermissionDenied | ErrorCode::QuotaExceeded => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::Conflict => 409,
            ErrorCode::RateLimited => 429,
            ErrorCode::Compacted => 410,
            ErrorCode::Unavailable => 503,
            ErrorCode::Internal | ErrorCode::Unspecified => 500,
        }
    }

    /// The CLI exit code of the code (ARFC 0019 clause 3.1.7), from the table
    /// of ARFC 0008 clause 3.6.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            ErrorCode::InvalidArgument => 2,
            ErrorCode::Unauthenticated | ErrorCode::PermissionDenied => 4,
            ErrorCode::NotFound => 3,
            ErrorCode::Conflict => 5,
            ErrorCode::RateLimited | ErrorCode::Unavailable => 6,
            ErrorCode::QuotaExceeded
            | ErrorCode::Compacted
            | ErrorCode::Internal
            | ErrorCode::Unspecified => 1,
        }
    }

    /// The code that a gRPC status with no [`ERROR_TRAILER`] stands for: a
    /// status from the transport or from a peer that is not Alloy. Each code
    /// of the table maps back to its own row, `RESOURCE_EXHAUSTED` to
    /// [`ErrorCode::RateLimited`], cancellation and deadlines to
    /// [`ErrorCode::Unavailable`], and every other status to
    /// [`ErrorCode::Internal`].
    #[must_use]
    pub fn from_grpc(code: Code) -> ErrorCode {
        match code {
            Code::InvalidArgument => ErrorCode::InvalidArgument,
            Code::Unauthenticated => ErrorCode::Unauthenticated,
            Code::PermissionDenied => ErrorCode::PermissionDenied,
            Code::NotFound => ErrorCode::NotFound,
            Code::Aborted => ErrorCode::Conflict,
            Code::ResourceExhausted => ErrorCode::RateLimited,
            Code::OutOfRange => ErrorCode::Compacted,
            Code::Unavailable | Code::Cancelled | Code::DeadlineExceeded => ErrorCode::Unavailable,
            _ => ErrorCode::Internal,
        }
    }
}

impl Error {
    /// An error with `code` and `message`, and every other field empty.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Error {
        Error {
            code: code.into(),
            message: message.into(),
            ..Error::default()
        }
    }

    /// Sets `resource`, the full name of the resource concerned.
    #[must_use]
    pub fn with_resource(mut self, resource: impl Into<String>) -> Error {
        self.resource = resource.into();
        self
    }

    /// Adds one invalid field and the rule it breaks.
    #[must_use]
    pub fn with_violation(mut self, field: impl Into<String>, rule: impl Into<String>) -> Error {
        self.field_violations.push(FieldViolation {
            field: field.into(),
            rule: rule.into(),
        });
        self
    }

    /// Sets `retry_delay_duration`. A delay beyond the range of
    /// `google.protobuf.Duration`, about 10,000 years, is clamped to it.
    #[must_use]
    pub fn with_retry_delay(mut self, delay: Duration) -> Error {
        self.retry_delay_duration = Some(prost_types::Duration::try_from(delay).unwrap_or(
            prost_types::Duration {
                seconds: 315_576_000_000,
                nanos: 0,
            },
        ));
        self
    }

    /// Sets `next_step`: the API method that fixes the error, and its CLI form.
    #[must_use]
    pub fn with_next_step(
        mut self,
        method: impl Into<String>,
        command: impl Into<String>,
    ) -> Error {
        self.next_step = Some(NextStep {
            method: method.into(),
            command: command.into(),
        });
        self
    }

    /// `retry_delay_duration` as a [`Duration`], or `None` when it is unset or
    /// negative.
    #[must_use]
    pub fn retry_delay(&self) -> Option<Duration> {
        self.retry_delay_duration
            .and_then(|d| Duration::try_from(d).ok())
    }

    /// The gRPC status that carries this error: the code's status, the
    /// message, and the encoded error in [`ERROR_TRAILER`].
    #[must_use]
    pub fn into_status(self) -> Status {
        let mut metadata = MetadataMap::new();
        metadata.insert_bin(
            ERROR_TRAILER,
            MetadataValue::from_bytes(&self.encode_to_vec()),
        );
        Status::with_metadata(self.code().grpc_code(), self.message, metadata)
    }

    /// The error a gRPC status carries: the decoded [`ERROR_TRAILER`] when it
    /// is present and decodes, else an error whose code
    /// [`ErrorCode::from_grpc`] gives and whose message is the status's.
    #[must_use]
    pub fn from_status(status: &Status) -> Error {
        let carried = status
            .metadata()
            .get_bin(ERROR_TRAILER)
            .and_then(|v| v.to_bytes().ok())
            .and_then(|bytes| Error::decode(bytes.as_ref()).ok());
        carried.unwrap_or_else(|| {
            Error::new(
                ErrorCode::from_grpc(status.code()),
                status.message().to_owned(),
            )
        })
    }
}

impl From<Error> for Status {
    fn from(error: Error) -> Status {
        error.into_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ErrorCode; 10] = [
        ErrorCode::InvalidArgument,
        ErrorCode::Unauthenticated,
        ErrorCode::PermissionDenied,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::QuotaExceeded,
        ErrorCode::RateLimited,
        ErrorCode::Compacted,
        ErrorCode::Unavailable,
        ErrorCode::Internal,
    ];

    #[test]
    fn every_code_maps_as_the_table_says() {
        let table: Vec<(Code, u16, u8)> = ALL
            .iter()
            .map(|c| (c.grpc_code(), c.http_status(), c.exit_code()))
            .collect();
        assert_eq!(
            table,
            [
                (Code::InvalidArgument, 400, 2),
                (Code::Unauthenticated, 401, 4),
                (Code::PermissionDenied, 403, 4),
                (Code::NotFound, 404, 3),
                (Code::Aborted, 409, 5),
                (Code::ResourceExhausted, 403, 1),
                (Code::ResourceExhausted, 429, 6),
                (Code::OutOfRange, 410, 1),
                (Code::Unavailable, 503, 6),
                (Code::Internal, 500, 1),
            ]
        );
    }

    #[test]
    fn a_status_carries_the_whole_error() {
        let error = Error::new(ErrorCode::RateLimited, "Too many requests.")
            .with_resource("nodes/linux-1")
            .with_violation("page_size", "must not be negative")
            .with_retry_delay(Duration::from_millis(1_500))
            .with_next_step(
                "alloy.cluster.v1alpha1.ClusterService/ListNodes",
                "alloy node list",
            );
        let status = error.clone().into_status();
        assert_eq!(status.code(), Code::ResourceExhausted);
        assert_eq!(status.message(), "Too many requests.");
        assert_eq!(Error::from_status(&status), error);
        assert_eq!(error.retry_delay(), Some(Duration::from_millis(1_500)));
    }

    #[test]
    fn a_status_without_the_trailer_maps_its_code() {
        let status = Status::unavailable("connection refused");
        let error = Error::from_status(&status);
        assert_eq!(error.code(), ErrorCode::Unavailable);
        assert_eq!(error.message, "connection refused");
        assert_eq!(
            Error::from_status(&Status::unimplemented("")).code(),
            ErrorCode::Internal
        );
    }

    #[test]
    fn grpc_codes_of_the_table_map_back_to_their_row() {
        for code in ALL {
            let back = ErrorCode::from_grpc(code.grpc_code());
            assert_eq!(back.grpc_code(), code.grpc_code(), "{code:?}");
        }
    }
}
