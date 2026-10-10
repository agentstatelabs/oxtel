//! What the server and client sides share: `_meta` as a propagation
//! carrier, error classification, and metric descriptions.

use std::borrow::Cow;

use oxtel_sdk::propagation::Extractor;
#[cfg(feature = "client")]
use oxtel_sdk::propagation::Injector;
use rmcp::model::{ClientRequest, JsonObject};

/// Reads trace context from a request's `params._meta`.
pub(crate) struct MetaReader<'a>(pub(crate) &'a JsonObject);

impl Extractor for MetaReader<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key)?.as_str().map(str::to_string)
    }
}

/// Writes trace context into a request's `params._meta`, unprefixed, as
/// SEP-414 asks for W3C keys.
#[cfg(feature = "client")]
pub(crate) struct MetaWriter<'a>(pub(crate) &'a mut JsonObject);

#[cfg(feature = "client")]
impl Injector for MetaWriter<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value.into());
    }
}

/// What a request is about, as the conventions record it.
#[derive(Default)]
pub(crate) struct Target {
    pub(crate) tool: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) resource: Option<String>,
}

impl Target {
    pub(crate) fn of(request: &ClientRequest) -> Self {
        match request {
            ClientRequest::CallToolRequest(r) => Target {
                tool: Some(r.params.name.to_string()),
                ..Target::default()
            },
            ClientRequest::GetPromptRequest(r) => Target {
                prompt: Some(r.params.name.clone()),
                ..Target::default()
            },
            ClientRequest::ReadResourceRequest(r) => Target {
                resource: Some(r.params.uri.clone()),
                ..Target::default()
            },
            ClientRequest::SubscribeRequest(r) => Target {
                resource: Some(r.params.uri.clone()),
                ..Target::default()
            },
            ClientRequest::UnsubscribeRequest(r) => Target {
                resource: Some(r.params.uri.clone()),
                ..Target::default()
            },
            _ => Target::default(),
        }
    }

    /// `{method} {tool or prompt}`, or the method alone. Resource URIs are
    /// left out of the name: there may be too many of them.
    pub(crate) fn span_name(&self, method: &str) -> String {
        match self.tool.as_ref().or(self.prompt.as_ref()) {
            Some(target) => format!("{method} {target}"),
            None => method.to_string(),
        }
    }

    /// `execute_tool` for tool calls.
    pub(crate) fn operation(&self) -> Option<&'static str> {
        self.tool.as_ref().map(|_| "execute_tool")
    }
}

/// Whether a JSON-RPC error code means the server failed, as opposed to the
/// caller asking for something the server can't serve.
pub(crate) fn server_failed(code: i32) -> bool {
    !matches!(code, -32700 | -32600 | -32601 | -32602 | -32002)
}

/// Long payloads are cut to this many bytes, at a character boundary.
const MAX_PAYLOAD: usize = 16 * 1024;

/// A value as JSON, for the opt-in payload attributes.
pub(crate) fn payload(value: &impl serde::Serialize) -> String {
    let mut json = serde_json::to_string(value).unwrap_or_default();
    if json.len() > MAX_PAYLOAD {
        let mut end = MAX_PAYLOAD;
        while !json.is_char_boundary(end) {
            end -= 1;
        }
        json.truncate(end);
        json.push('…');
    }
    json
}

/// A label, borrowing when it can.
pub(crate) fn label(key: &'static str, value: impl Into<Cow<'static, str>>) -> metrics::Label {
    match value.into() {
        Cow::Borrowed(v) => metrics::Label::new(key, v),
        Cow::Owned(v) => metrics::Label::new(key, v),
    }
}

/// Give the current `metrics` recorder the units and descriptions of the
/// MCP metrics, so the durations are in seconds with the conventions'
/// buckets. [`Instrumented::new`](crate::Instrumented::new) does this, so
/// make it after installing the recorder, or call this once the recorder
/// is in place.
pub fn describe_metrics() {
    for (name, description) in [
        (
            "mcp.server.operation.duration",
            "MCP request or notification duration as observed on the receiver from the time it was received until the result or ack is sent.",
        ),
        (
            "mcp.client.operation.duration",
            "The duration of the MCP request or notification as observed on the sender from the time it was sent until the response or ack is received.",
        ),
        (
            "mcp.server.session.duration",
            "The duration of the MCP session as observed on the MCP server.",
        ),
    ] {
        metrics::describe_histogram!(name, metrics::Unit::Seconds, description);
    }
}
