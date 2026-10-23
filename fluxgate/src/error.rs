use std::fmt;

/// Why an evaluation failed. Matches OpenFeature's error codes where they overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The flag does not exist for the client's team, or is not deployed to its environment.
    FlagNotFound,
    /// The flag's value has another type than the default value.
    TypeMismatch,
    /// The evaluation context has no targeting key.
    TargetingKeyMissing,
    /// The edge rejected the evaluation context.
    InvalidContext,
    /// The edge, or a [`FlagSnapshot`](crate::FlagSnapshot), has not loaded the flags yet.
    ProviderNotReady,
    /// The edge answered with something that is not valid OFREP.
    ParseError,
    /// The edge rejected the SDK key.
    Unauthorized,
    /// Any other failure, such as a network error or a timeout.
    General,
}

impl ErrorCode {
    /// The OFREP name of the code, for example `FLAG_NOT_FOUND`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FlagNotFound => "FLAG_NOT_FOUND",
            Self::TypeMismatch => "TYPE_MISMATCH",
            Self::TargetingKeyMissing => "TARGETING_KEY_MISSING",
            Self::InvalidContext => "INVALID_CONTEXT",
            Self::ProviderNotReady => "PROVIDER_NOT_READY",
            Self::ParseError => "PARSE_ERROR",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::General => "GENERAL",
        }
    }

    /// Maps an OFREP `errorCode` string to an [`ErrorCode`].
    pub(crate) fn from_wire(wire: &str) -> Option<Self> {
        Some(match wire {
            "FLAG_NOT_FOUND" => Self::FlagNotFound,
            "TYPE_MISMATCH" => Self::TypeMismatch,
            "TARGETING_KEY_MISSING" => Self::TargetingKeyMissing,
            "INVALID_CONTEXT" => Self::InvalidContext,
            "PROVIDER_NOT_READY" => Self::ProviderNotReady,
            "PARSE_ERROR" => Self::ParseError,
            "UNAUTHORIZED" => Self::Unauthorized,
            "GENERAL" => Self::General,
            _ => return None,
        })
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed call to the edge.
///
/// Only [`FluxGateClient::evaluate_all`](crate::FluxGateClient::evaluate_all) and the snapshot
/// methods return it; single-flag evaluations report errors in
/// [`FlagEvaluation::error_code`](crate::FlagEvaluation::error_code) instead.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    code: ErrorCode,
    message: String,
    status: Option<u16>,
    retryable: bool,
}

impl Error {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            status: None,
            retryable: false,
        }
    }

    pub(crate) fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub(crate) fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// Maps an edge error response to an [`Error`].
    pub(crate) fn from_response(
        status: u16,
        wire_code: Option<&str>,
        details: Option<&str>,
    ) -> Self {
        let code = match status {
            400 => wire_code
                .and_then(ErrorCode::from_wire)
                .unwrap_or(ErrorCode::InvalidContext),
            401 | 403 => ErrorCode::Unauthorized,
            404 => ErrorCode::FlagNotFound,
            503 => ErrorCode::ProviderNotReady,
            _ => ErrorCode::General,
        };
        let message = match details.map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => format!("Edge returned HTTP {status}: {text}"),
            None => format!("Edge returned HTTP {status}"),
        };
        Self {
            code,
            message,
            status: Some(status),
            retryable: matches!(status, 502..=504),
        }
    }

    /// What went wrong.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The HTTP status, or `None` for network errors and timeouts.
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// Whether retrying the same call can succeed.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }
}

/// Invalid settings for a [`FluxGateClient`](crate::FluxGateClient).
///
/// Messages never include the SDK key.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The base URL is empty or not an `http://` or `https://` URL.
    #[error("FluxGate: base URL must be an http:// or https:// URL, got {0:?}")]
    InvalidBaseUrl(String),
    /// The SDK key does not have the form `<clientId>.<apiKey>`.
    #[error("FluxGate: SDK key must have the form <clientId>.<apiKey>")]
    InvalidSdkKey,
    /// The HTTP client could not be created, for example because no TLS backend is enabled.
    #[error("FluxGate: could not create the HTTP client")]
    HttpClient(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The blocking client could not start its background runtime.
    #[error("FluxGate: could not start the runtime of the blocking client")]
    Runtime(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_codes() {
        assert_eq!(
            Error::from_response(401, None, None).code(),
            ErrorCode::Unauthorized
        );
        assert_eq!(
            Error::from_response(404, None, None).code(),
            ErrorCode::FlagNotFound
        );
        assert_eq!(
            Error::from_response(400, Some("TARGETING_KEY_MISSING"), None).code(),
            ErrorCode::TargetingKeyMissing
        );
        assert_eq!(
            Error::from_response(400, Some("NOPE"), None).code(),
            ErrorCode::InvalidContext
        );
        let unavailable = Error::from_response(503, None, Some(" warming up "));
        assert_eq!(unavailable.code(), ErrorCode::ProviderNotReady);
        assert!(unavailable.is_retryable());
        assert_eq!(
            unavailable.to_string(),
            "Edge returned HTTP 503: warming up"
        );
        assert!(!Error::from_response(500, None, None).is_retryable());
    }
}
