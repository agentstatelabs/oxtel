//! The client side: requests sent through an rmcp client.

use std::time::Instant;

use oxtel_sdk::propagation::inject_trace_context;
use oxtel_sdk::trace::{current_span_context, span_context};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, GetMeta, ServerResult,
};
use rmcp::service::{Peer, RoleClient, ServiceError};
use tracing::Instrument;
use tracing::field::Empty;

use crate::common::{MetaWriter, Target, label, payload};

/// Settings for instrumented client requests.
#[derive(Clone, Copy, Debug)]
pub struct ClientOptions {
    /// Send trace context in `params._meta`. Default: true.
    pub propagate: bool,
    /// Record `mcp.client.operation.duration`. Default: true.
    pub metrics: bool,
    /// Record tool arguments and results; they may hold personal or secret
    /// data. Default: false.
    pub payloads: bool,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            propagate: true,
            metrics: true,
            payloads: false,
        }
    }
}

/// Send `request` through `peer` inside a client span, with the trace
/// context in `params._meta` so the server continues the trace, and record
/// `mcp.client.operation.duration`. On the client side every JSON-RPC error
/// is a failure; so is a tool result with `isError` (`error.type`
/// `tool_error`).
pub async fn send(
    peer: &Peer<RoleClient>,
    mut request: ClientRequest,
    options: ClientOptions,
) -> Result<ServerResult, ServiceError> {
    let method = request.method().to_string();
    let target = Target::of(&request);
    let protocol_version = peer
        .peer_info()
        .map(|info| info.protocol_version.as_str().to_string());
    let span = tracing::info_span!(
        target: "oxtel_rmcp",
        "MCP client operation",
        otel.name = %target.span_name(&method),
        otel.kind = "client",
        mcp.method.name = method.as_str(),
        mcp.protocol.version = protocol_version.as_deref(),
        gen_ai.tool.name = target.tool.as_deref(),
        gen_ai.operation.name = target.operation(),
        gen_ai.prompt.name = target.prompt.as_deref(),
        mcp.resource.uri = target.resource.as_deref(),
        gen_ai.tool.call.arguments = Empty,
        gen_ai.tool.call.result = Empty,
        rpc.response.status_code = Empty,
        error.type = Empty,
        otel.status_code = Empty,
        otel.status_description = Empty,
    );
    if options.payloads
        && let ClientRequest::CallToolRequest(call) = &request
    {
        span.record(
            "gen_ai.tool.call.arguments",
            payload(&call.params.arguments).as_str(),
        );
    }
    if options.propagate
        && let Some(cx) = span_context(&span).or_else(current_span_context)
    {
        inject_trace_context(&cx, &mut MetaWriter(&mut request.get_meta_mut().0));
    }
    let started = Instant::now();
    let result = peer.send_request(request).instrument(span.clone()).await;

    let mut labels = vec![label("mcp.method.name", method)];
    if let Some(tool) = target.tool {
        labels.push(label("gen_ai.tool.name", tool));
        labels.push(label("gen_ai.operation.name", "execute_tool"));
    }
    if let Some(prompt) = target.prompt {
        labels.push(label("gen_ai.prompt.name", prompt));
    }
    if let Some(v) = protocol_version {
        labels.push(label("mcp.protocol.version", v));
    }
    let error_type = match &result {
        Ok(ServerResult::CallToolResult(r)) => {
            if options.payloads {
                span.record("gen_ai.tool.call.result", payload(&r.content).as_str());
            }
            (r.is_error == Some(true)).then(|| "tool_error".to_string())
        }
        Ok(_) => None,
        Err(ServiceError::McpError(e)) => {
            let code = e.code.0.to_string();
            span.record("rpc.response.status_code", code.as_str());
            span.record("otel.status_description", e.message.as_ref());
            labels.push(label("rpc.response.status_code", code.clone()));
            Some(code)
        }
        Err(ServiceError::Timeout { .. }) => Some("timeout".into()),
        Err(ServiceError::Cancelled { .. }) => Some("cancelled".into()),
        Err(ServiceError::TransportClosed) => Some("transport_closed".into()),
        Err(_) => Some("_OTHER".into()),
    };
    if let Some(error_type) = error_type {
        span.record("error.type", error_type.as_str());
        span.record("otel.status_code", "error");
        labels.push(label("error.type", error_type));
    }
    if options.metrics {
        metrics::histogram!("mcp.client.operation.duration", labels)
            .record(started.elapsed().as_secs_f64());
    }
    result
}

/// Call a tool through `peer`, instrumented as [`send`] describes.
pub async fn call_tool(
    peer: &Peer<RoleClient>,
    params: CallToolRequestParams,
    options: ClientOptions,
) -> Result<CallToolResult, ServiceError> {
    match send(
        peer,
        ClientRequest::CallToolRequest(CallToolRequest::new(params)),
        options,
    )
    .await?
    {
        ServerResult::CallToolResult(result) => Ok(result),
        _ => Err(ServiceError::UnexpectedResponse),
    }
}
