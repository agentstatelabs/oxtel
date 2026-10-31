//! The server side: a wrapper around any rmcp server.

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use oxtel_sdk::propagation::extract_trace_context;
use oxtel_sdk::trace::attach_remote_parent;
use rmcp::model::{ClientNotification, ClientRequest, ConstString, ErrorData, ServerResult};
use rmcp::service::{NotificationContext, RequestContext, RoleServer, Service};
use tracing::Instrument;
use tracing::field::Empty;

use crate::common::{MetaReader, Target, describe_metrics, label, payload, server_failed};

#[derive(Clone)]
struct Config {
    propagate: bool,
    metrics: bool,
    payloads: bool,
    transport: Option<&'static str>,
}

/// An rmcp server instrumented as the OpenTelemetry semantic conventions for
/// MCP describe. Wrap your server and serve the wrapper:
///
/// ```no_run
/// # use rmcp::ServiceExt;
/// # async fn run<T, E, A>(server: impl rmcp::ServerHandler, transport: T) -> Result<(), Box<dyn std::error::Error>>
/// # where
/// #     T: rmcp::transport::IntoTransport<rmcp::service::RoleServer, E, A>,
/// #     E: std::error::Error + Send + Sync + 'static,
/// # {
/// // `transport` is rmcp::transport::stdio(), for instance.
/// let service = oxtel_rmcp::Instrumented::new(server)
///     .with_transport("pipe")
///     .serve(transport)
///     .await?;
/// service.waiting().await?;
/// # Ok(()) }
/// ```
///
/// Each request and notification gets:
///
/// - **a span** named `{mcp.method.name} {target}`, such as
///   `tools/call commit`, a child of the caller's span when `params._meta`
///   carries W3C `traceparent`. Attributes: `mcp.method.name`,
///   `jsonrpc.request.id`, `mcp.session.id` (over HTTP),
///   `mcp.protocol.version`, `gen_ai.tool.name` with
///   `gen_ai.operation.name` `execute_tool`, `gen_ai.prompt.name`,
///   `mcp.resource.uri`, `network.transport` and, over HTTP,
///   `network.protocol.name` and `version`.
/// - **errors**: a tool result with `isError` sets `error.type` to
///   `tool_error`. A JSON-RPC error records its code in
///   `rpc.response.status_code`. Codes for requests the server can't serve
///   (parse errors, unknown methods or tools, invalid params, unknown
///   resources) aren't the server's failure. Other codes set `error.type` to
///   the code and the span status to Error with the error's message.
/// - **`mcp.server.operation.duration`**, through the `metrics` facade, and
///   `mcp.server.session.duration` when the session ends.
///
/// Tool arguments and results aren't recorded unless
/// [`with_payloads`](Instrumented::with_payloads) asks for them.
pub struct Instrumented<S> {
    inner: S,
    config: Arc<Config>,
    session_started: OnceLock<Instant>,
    protocol_version: OnceLock<String>,
}

impl<S> Instrumented<S> {
    /// Instrument `inner`, with propagation and metrics on and payloads off.
    /// Describes the metrics to the current recorder, so make it after
    /// installing one.
    pub fn new(inner: S) -> Self {
        describe_metrics();
        Instrumented {
            inner,
            config: Arc::new(Config {
                propagate: true,
                metrics: true,
                payloads: false,
                transport: None,
            }),
            session_started: OnceLock::new(),
            protocol_version: OnceLock::new(),
        }
    }

    /// Continue the caller's trace from `params._meta`. Default: true.
    pub fn with_propagation(mut self, enabled: bool) -> Self {
        Arc::make_mut(&mut self.config).propagate = enabled;
        self
    }

    /// Record the MCP metrics. Default: true.
    pub fn with_metrics(mut self, enabled: bool) -> Self {
        Arc::make_mut(&mut self.config).metrics = enabled;
        self
    }

    /// Record tool arguments and results as `gen_ai.tool.call.arguments` and
    /// `gen_ai.tool.call.result`, as JSON cut at 16 KiB. They may hold
    /// personal or secret data. Default: false.
    pub fn with_payloads(mut self, enabled: bool) -> Self {
        Arc::make_mut(&mut self.config).payloads = enabled;
        self
    }

    /// The `network.transport` to record when the request didn't come over
    /// HTTP: `pipe` for stdio. Over HTTP it's `tcp`.
    pub fn with_transport(mut self, transport: &'static str) -> Self {
        Arc::make_mut(&mut self.config).transport = Some(transport);
        self
    }

    /// The server inside.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

/// What the HTTP transport tells about a request, if it came over HTTP.
struct Network {
    transport: Option<&'static str>,
    protocol: Option<&'static str>,
    version: Option<&'static str>,
    session: Option<String>,
}

impl Network {
    fn of(extensions: &rmcp::model::Extensions, fallback: Option<&'static str>) -> Self {
        match extensions.get::<http::request::Parts>() {
            Some(parts) => Network {
                transport: Some("tcp"),
                protocol: Some("http"),
                version: Some(match parts.version {
                    http::Version::HTTP_10 => "1.0",
                    http::Version::HTTP_11 => "1.1",
                    http::Version::HTTP_2 => "2",
                    http::Version::HTTP_3 => "3",
                    _ => "_OTHER",
                }),
                session: parts
                    .headers
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            },
            None => Network {
                transport: fallback,
                protocol: None,
                version: None,
                session: None,
            },
        }
    }
}

/// What every span and measurement of one operation carries.
struct Operation {
    method: String,
    target: Target,
    protocol_version: Option<String>,
    network: Network,
}

impl Operation {
    fn labels(&self) -> Vec<metrics::Label> {
        let mut labels = vec![label("mcp.method.name", self.method.clone())];
        if let Some(tool) = &self.target.tool {
            labels.push(label("gen_ai.tool.name", tool.clone()));
        }
        if let Some(op) = self.target.operation() {
            labels.push(label("gen_ai.operation.name", op));
        }
        if let Some(prompt) = &self.target.prompt {
            labels.push(label("gen_ai.prompt.name", prompt.clone()));
        }
        if let Some(v) = &self.protocol_version {
            labels.push(label("mcp.protocol.version", v.clone()));
        }
        for (key, value) in [
            ("network.transport", self.network.transport),
            ("network.protocol.name", self.network.protocol),
            ("network.protocol.version", self.network.version),
        ] {
            if let Some(value) = value {
                labels.push(label(key, value));
            }
        }
        labels
    }

    fn span(&self, id: Option<String>) -> tracing::Span {
        tracing::info_span!(
            target: "oxtel_rmcp",
            "MCP server operation",
            otel.name = %self.target.span_name(&self.method),
            otel.kind = "server",
            mcp.method.name = self.method.as_str(),
            jsonrpc.request.id = id,
            mcp.session.id = self.network.session.as_deref(),
            mcp.protocol.version = self.protocol_version.as_deref(),
            gen_ai.tool.name = self.target.tool.as_deref(),
            gen_ai.operation.name = self.target.operation(),
            gen_ai.prompt.name = self.target.prompt.as_deref(),
            mcp.resource.uri = self.target.resource.as_deref(),
            network.transport = self.network.transport,
            network.protocol.name = self.network.protocol,
            network.protocol.version = self.network.version,
            gen_ai.tool.call.arguments = Empty,
            gen_ai.tool.call.result = Empty,
            rpc.response.status_code = Empty,
            error.type = Empty,
            otel.status_code = Empty,
            otel.status_description = Empty,
        )
    }

    /// Record how it ended on the span and in the duration metric.
    fn finish(
        self,
        span: &tracing::Span,
        error: Option<&ErrorData>,
        tool_error: bool,
        config: &Config,
        started: Instant,
    ) {
        let mut labels = config.metrics.then(|| self.labels());
        let fail = |error_type: String, labels: &mut Option<Vec<metrics::Label>>| {
            span.record("error.type", error_type.as_str());
            span.record("otel.status_code", "error");
            if let Some(labels) = labels {
                labels.push(label("error.type", error_type));
            }
        };
        if let Some(e) = error {
            let code = e.code.0.to_string();
            span.record("rpc.response.status_code", code.as_str());
            if let Some(labels) = &mut labels {
                labels.push(label("rpc.response.status_code", code.clone()));
            }
            if server_failed(e.code.0) {
                span.record("otel.status_description", e.message.as_ref());
                fail(code, &mut labels);
            }
        } else if tool_error {
            fail("tool_error".into(), &mut labels);
        }
        if let Some(labels) = labels {
            metrics::histogram!("mcp.server.operation.duration", labels)
                .record(started.elapsed().as_secs_f64());
        }
    }
}

impl<S: Service<RoleServer>> Instrumented<S> {
    fn operation(
        &self,
        method: String,
        target: Target,
        extensions: &rmcp::model::Extensions,
        peer_version: Option<String>,
    ) -> Operation {
        let protocol_version = self.protocol_version.get().cloned().or(peer_version);
        Operation {
            method,
            target,
            protocol_version,
            network: Network::of(extensions, self.config.transport),
        }
    }

    fn parent(
        &self,
        meta: &rmcp::model::JsonObject,
    ) -> Option<oxtel_sdk::trace::RemoteParentGuard> {
        if !self.config.propagate {
            return None;
        }
        extract_trace_context(&MetaReader(meta)).map(attach_remote_parent)
    }
}

impl<S: Service<RoleServer>> Service<RoleServer> for Instrumented<S> {
    fn handle_request(
        &self,
        request: ClientRequest,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ServerResult, ErrorData>> + Send + '_ {
        if let ClientRequest::InitializeRequest(init) = &request {
            let _ = self.session_started.set(Instant::now());
            let _ = self
                .protocol_version
                .set(init.params.protocol_version.as_str().to_string());
        }
        let peer_version = context
            .peer
            .peer_info()
            .map(|info| info.protocol_version.as_str().to_string());
        let operation = self.operation(
            request.method().to_string(),
            Target::of(&request),
            &context.extensions,
            peer_version,
        );
        let span = {
            let _parent = self.parent(&context.meta.0);
            operation.span(Some(context.id.to_string()))
        };
        if self.config.payloads
            && let ClientRequest::CallToolRequest(call) = &request
        {
            span.record(
                "gen_ai.tool.call.arguments",
                payload(&call.params.arguments).as_str(),
            );
        }
        let config = Arc::clone(&self.config);
        let started = Instant::now();
        let inner = self.inner.handle_request(request, context);
        let recorded = span.clone();
        async move {
            let result = inner.await;
            let tool_error = match &result {
                Ok(ServerResult::CallToolResult(r)) => {
                    if config.payloads {
                        recorded.record("gen_ai.tool.call.result", payload(&r.content).as_str());
                    }
                    r.is_error == Some(true)
                }
                _ => false,
            };
            operation.finish(
                &recorded,
                result.as_ref().err(),
                tool_error,
                &config,
                started,
            );
            result
        }
        .instrument(span)
    }

    fn handle_notification(
        &self,
        notification: ClientNotification,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        let method = match &notification {
            ClientNotification::CancelledNotification(n) => n.method.as_str().to_string(),
            ClientNotification::ProgressNotification(n) => n.method.as_str().to_string(),
            ClientNotification::InitializedNotification(n) => n.method.as_str().to_string(),
            ClientNotification::RootsListChangedNotification(n) => n.method.as_str().to_string(),
            ClientNotification::CustomNotification(n) => n.method.clone(),
            // Non-exhaustive since rmcp 3: a notification added in a later
            // rmcp release is recorded as _OTHER until oxtel learns its name.
            _ => "_OTHER".to_string(),
        };
        let peer_version = context
            .peer
            .peer_info()
            .map(|info| info.protocol_version.as_str().to_string());
        let operation =
            self.operation(method, Target::default(), &context.extensions, peer_version);
        let span = {
            let _parent = self.parent(&context.meta.0);
            operation.span(None)
        };
        let config = Arc::clone(&self.config);
        let started = Instant::now();
        let inner = self.inner.handle_notification(notification, context);
        let recorded = span.clone();
        async move {
            let result = inner.await;
            operation.finish(&recorded, result.as_ref().err(), false, &config, started);
            result
        }
        .instrument(span)
    }

    fn get_info(&self) -> <RoleServer as rmcp::service::ServiceRole>::Info {
        self.inner.get_info()
    }
}

impl<S> Drop for Instrumented<S> {
    fn drop(&mut self) {
        let Some(started) = self.session_started.get() else {
            return;
        };
        if !self.config.metrics {
            return;
        }
        let mut labels = Vec::new();
        if let Some(v) = self.protocol_version.get() {
            labels.push(label("mcp.protocol.version", v.clone()));
        }
        if let Some(t) = self.config.transport {
            labels.push(label("network.transport", t));
        }
        metrics::histogram!("mcp.server.session.duration", labels)
            .record(started.elapsed().as_secs_f64());
    }
}
