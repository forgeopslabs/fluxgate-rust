use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde_json::{Map, Value, json};

use crate::{BulkEvaluation, Error, ErrorCode, EvaluationContext, ResolvedFlag};

const FLAG_PATH: &str = "/ofrep/v1/evaluate/flags/";
const BULK_PATH: &str = "/ofrep/v1/evaluate/flags";
const HEALTH_PATH: &str = "/health";

/// HTTP transport for the edge's OFREP endpoints. Network errors, timeouts and HTTP 502, 503
/// and 504 are retried with exponential backoff; everything else fails at once.
pub(crate) struct OfrepTransport {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) authorization: String,
    pub(crate) timeout: Duration,
    pub(crate) max_attempts: u32,
    pub(crate) initial_backoff: Duration,
}

impl std::fmt::Debug for OfrepTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the SDK key.
        f.debug_struct("OfrepTransport")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .finish_non_exhaustive()
    }
}

impl OfrepTransport {
    /// Evaluates one flag.
    pub(crate) async fn evaluate(
        &self,
        key: &str,
        context: &EvaluationContext,
    ) -> Result<ResolvedFlag, Error> {
        let body = request_body(context)?;
        let url = format!("{}{FLAG_PATH}{}", self.base_url, encode_path_segment(key));
        self.with_retry(|| async {
            let response = self.send(self.post(&url, &body)).await?;
            let status = response.status();
            let json = read_json(response, status).await?;
            if status != StatusCode::OK {
                return Err(error_from(status, json.as_ref()));
            }
            to_resolved_flag(json.as_ref(), Some(key))
        })
        .await
    }

    /// Evaluates every flag of the client's team. With the previous version, the edge answers
    /// `not_modified` when nothing changed.
    pub(crate) async fn evaluate_all(
        &self,
        context: &EvaluationContext,
        version: Option<&str>,
    ) -> Result<BulkEvaluation, Error> {
        let body = request_body(context)?;
        let url = format!("{}{BULK_PATH}", self.base_url);
        self.with_retry(|| async {
            let mut request = self.post(&url, &body);
            if let Some(version) = version {
                request = request.header(IF_NONE_MATCH, version);
            }
            let response = self.send(request).await?;
            let status = response.status();
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if status == StatusCode::NOT_MODIFIED {
                return Ok(BulkEvaluation {
                    not_modified: true,
                    version: etag.or_else(|| version.map(str::to_owned)),
                    flags: HashMap::new(),
                });
            }
            let json = read_json(response, status).await?;
            if status != StatusCode::OK {
                return Err(error_from(status, json.as_ref()));
            }
            let Some(list) = json
                .as_ref()
                .and_then(|json| json.get("flags"))
                .and_then(Value::as_array)
            else {
                return Err(parse_error(
                    "Malformed bulk evaluation from edge: missing flags array",
                ));
            };
            let flags = list
                .iter()
                .map(|item| to_resolved_flag(Some(item), None).map(|flag| (flag.key.clone(), flag)))
                .collect::<Result<HashMap<_, _>, _>>()?;
            let metadata_version = json
                .as_ref()
                .and_then(|json| json.pointer("/metadata/version"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(BulkEvaluation {
                not_modified: false,
                version: etag.or(metadata_version),
                flags,
            })
        })
        .await
    }

    /// Probes `GET /health` once, without retries.
    pub(crate) async fn health(&self) -> bool {
        let request = self
            .http
            .get(format!("{}{HEALTH_PATH}", self.base_url))
            .timeout(self.timeout);
        matches!(request.send().await, Ok(response) if response.status().is_success())
    }

    fn post(&self, url: &str, body: &str) -> RequestBuilder {
        self.http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, &self.authorization)
            .timeout(self.timeout)
            .body(body.to_owned())
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, Error> {
        request.send().await.map_err(|error| {
            let message = if error.is_timeout() {
                format!("Edge did not answer within {} ms", self.timeout.as_millis())
            } else {
                format!("Could not reach the edge: {}", describe(&error))
            };
            Error::new(ErrorCode::General, message).retryable()
        })
    }

    async fn with_retry<T, F, Fut>(&self, call: F) -> Result<T, Error>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let mut backoff = self.initial_backoff;
        let mut attempt = 1;
        loop {
            match call().await {
                Err(error) if error.is_retryable() && attempt < self.max_attempts => {
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2);
                    attempt += 1;
                }
                result => return result,
            }
        }
    }
}

/// The OFREP request body: `{"context": {...}}` with `targetingKey` first.
fn request_body(context: &EvaluationContext) -> Result<String, Error> {
    serde_json::to_string(&json!({ "context": context })).map_err(|error| {
        Error::new(
            ErrorCode::InvalidContext,
            format!("Evaluation context is not serializable: {error}"),
        )
    })
}

/// The response body as JSON, or `None` when it is empty. A non-JSON body is a parse error
/// only on HTTP 200; error responses fall back to the status code.
async fn read_json(response: Response, status: StatusCode) -> Result<Option<Value>, Error> {
    let text = match response.text().await {
        Ok(text) => text,
        Err(error) => {
            return Err(Error::new(
                ErrorCode::General,
                format!("Could not read the edge's response: {}", describe(&error)),
            )
            .retryable());
        }
    };
    if text.is_empty() {
        return Ok(None);
    }
    match serde_json::from_str(&text) {
        Ok(json) => Ok(Some(json)),
        Err(_) if status == StatusCode::OK => {
            Err(parse_error("Malformed response from edge: not JSON"))
        }
        Err(_) => Ok(None),
    }
}

fn error_from(status: StatusCode, json: Option<&Value>) -> Error {
    let field = |name: &str| json.and_then(|json| json.get(name)).and_then(Value::as_str);
    Error::from_response(status.as_u16(), field("errorCode"), field("errorDetails"))
}

fn to_resolved_flag(
    node: Option<&Value>,
    requested_key: Option<&str>,
) -> Result<ResolvedFlag, Error> {
    let Some(object) = node.and_then(Value::as_object) else {
        return Err(parse_error(
            "Malformed flag evaluation from edge: not an object",
        ));
    };
    let text = |name: &str| object.get(name).and_then(Value::as_str).map(str::to_owned);
    let key = text("key")
        .or_else(|| requested_key.map(str::to_owned))
        .unwrap_or_default();
    let error_code = match object.get("errorCode") {
        None | Some(Value::Null) => None,
        Some(wire) => Some(
            wire.as_str()
                .and_then(ErrorCode::from_wire)
                .unwrap_or(ErrorCode::General),
        ),
    };
    let reason = text("reason").unwrap_or_else(|| {
        if error_code.is_some() {
            "ERROR"
        } else {
            "UNKNOWN"
        }
        .to_owned()
    });
    let value = object
        .get("value")
        .filter(|value| !value.is_null())
        .cloned();
    let metadata = match object.get("metadata") {
        Some(Value::Object(metadata)) => metadata.clone(),
        _ => Map::new(),
    };
    Ok(ResolvedFlag {
        key,
        value,
        variant: text("variant"),
        reason,
        error_code,
        error_details: text("errorDetails"),
        metadata,
    })
}

fn parse_error(message: &str) -> Error {
    Error::new(ErrorCode::ParseError, message).with_status(200)
}

/// Percent-encodes a flag key for use as one URL path segment.
fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The error and its causes, which hold the useful detail for connection errors.
fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_flag_keys() {
        assert_eq!(encode_path_segment("new-checkout"), "new-checkout");
        assert_eq!(encode_path_segment("a b/ü"), "a%20b%2F%C3%BC");
    }

    #[test]
    fn reads_flag_fields() {
        let json = json!({"key": "k", "value": "x", "variant": "x", "reason": "SPLIT", "metadata": {"team": "a"}});
        let flag = to_resolved_flag(Some(&json), None).unwrap();
        assert_eq!(flag.value, Some(json!("x")));
        assert_eq!(flag.reason, "SPLIT");
        assert_eq!(flag.metadata.get("team"), Some(&json!("a")));

        let error = json!({"key": "k", "errorCode": "FLAG_NOT_FOUND", "errorDetails": "gone"});
        let flag = to_resolved_flag(Some(&error), None).unwrap();
        assert_eq!(flag.error_code, Some(ErrorCode::FlagNotFound));
        assert_eq!(flag.reason, "ERROR");

        let flag = to_resolved_flag(Some(&json!({"value": null})), Some("requested")).unwrap();
        assert_eq!(flag.key, "requested");
        assert_eq!(flag.value, None);
        assert_eq!(flag.reason, "UNKNOWN");

        assert_eq!(
            to_resolved_flag(Some(&json!([1])), None)
                .unwrap_err()
                .code(),
            ErrorCode::ParseError
        );
    }
}
