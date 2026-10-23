#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

pub const SDK_KEY: &str = "client-1.api-key";

/// A fake edge for bulk evaluations. `new-checkout` is on for country CA; `checkout-theme` is
/// `express` for user_tier plus, otherwise `classic`. The ETag changes when the flags change.
#[derive(Clone, Default)]
pub struct FakeEdge {
    state: Arc<Mutex<EdgeState>>,
}

#[derive(Default)]
struct EdgeState {
    version: u32,
    failing: Option<u16>,
    bodies: Vec<Value>,
    if_none_match: Vec<Option<String>>,
}

impl FakeEdge {
    pub async fn start() -> (MockServer, Self) {
        let server = MockServer::start().await;
        let edge = Self::default();
        Mock::given(method("POST"))
            .and(path("/ofrep/v1/evaluate/flags"))
            .respond_with(edge.clone())
            .mount(&server)
            .await;
        (server, edge)
    }

    /// Answers every later call with this HTTP status, or normally with `None`.
    pub fn fail_with(&self, status: Option<u16>) {
        self.state.lock().unwrap().failing = status;
    }

    /// Changes the flag set, so the next call returns new flags and a new ETag.
    pub fn bump(&self) {
        self.state.lock().unwrap().version += 1;
    }

    pub fn contexts(&self) -> Vec<Value> {
        let state = self.state.lock().unwrap();
        state
            .bodies
            .iter()
            .map(|body| body["context"].clone())
            .collect()
    }

    pub fn if_none_match(&self) -> Vec<Option<String>> {
        self.state.lock().unwrap().if_none_match.clone()
    }

    pub fn calls(&self) -> usize {
        self.state.lock().unwrap().bodies.len()
    }
}

impl Respond for FakeEdge {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.state.lock().unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let if_none_match = request
            .headers
            .get("if-none-match")
            .map(|value| value.to_str().unwrap().to_owned());
        state.bodies.push(body.clone());
        state.if_none_match.push(if_none_match.clone());
        if let Some(status) = state.failing {
            return ResponseTemplate::new(status).set_body_json(json!({"errorCode": "GENERAL"}));
        }
        let context = &body["context"];
        let etag = format!(
            "\"v{}-{}-{}\"",
            state.version, context["country"], context["user_tier"]
        );
        if if_none_match.as_deref() == Some(etag.as_str()) {
            return ResponseTemplate::new(304).insert_header("etag", etag.as_str());
        }
        let plus = context["user_tier"] == "plus";
        let theme = if plus { "express" } else { "classic" };
        let mut flags = vec![
            json!({"key": "new-checkout", "value": context["country"] == "CA", "reason": "TARGETING_MATCH"}),
            json!({"key": "checkout-theme", "value": theme, "variant": theme, "reason": "TARGETING_MATCH"}),
        ];
        if state.version > 0 {
            flags.push(
                json!({"key": "limit", "value": state.version, "variant": "v", "reason": "STATIC"}),
            );
        }
        ResponseTemplate::new(200)
            .insert_header("etag", etag.as_str())
            .set_body_json(json!({ "flags": flags }))
    }
}
