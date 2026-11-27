//! The server side: wrappers around any rmcp server.

use std::borrow::Cow;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use oxtel_sdk::SpanContext;
use oxtel_sdk::propagation::extract_trace_context;
use oxtel_sdk::trace::attach_remote_parent;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestMethod, CallToolRequestParams, CallToolResponse, CallToolResult,
    CancelTaskMethod, CancelTaskParams, CancelledNotificationMethod, CancelledNotificationParam,
    ClientNotification, ClientRequest, CompleteRequestMethod, CompleteRequestParams,
    CompleteResult, ConstString, CustomNotification, CustomRequest, CustomResult,
    DiscoverRequestMethod, DiscoverResult, ErrorData, GetPromptRequestMethod,
    GetPromptRequestParams, GetPromptResponse, GetTaskMethod, GetTaskParams, GetTaskResult,
    InitializeRequestParams, InitializeResult, InitializeResultMethod,
    InitializedNotificationMethod, JsonObject, ListPromptsRequestMethod, ListPromptsResult,
    ListResourceTemplatesRequestMethod, ListResourceTemplatesResult, ListResourcesRequestMethod,
    ListResourcesResult, ListToolsRequestMethod, ListToolsResult, PaginatedRequestParams,
    PingRequestMethod, ProgressNotificationMethod, ProgressNotificationParam, ProtocolVersion,
    ReadResourceRequestMethod, ReadResourceRequestParams, ReadResourceResponse,
    RootsListChangedNotificationMethod, ServerConfig, ServerResult, SetLevelRequestMethod,
    SubscribeRequestMethod, SubscribeRequestParams, SubscriptionFilter, Tool,
    UnsubscribeRequestMethod, UnsubscribeRequestParams, UpdateTaskMethod, UpdateTaskParams,
};
// Logging is deprecated by SEP-2577, but clients may still ask for it.
#[allow(deprecated)]
use rmcp::model::SetLevelRequestParams;
use rmcp::service::{
    MaybeSendFuture, NotificationContext, RequestContext, RoleServer, Service, SubscriptionContext,
};
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
///   carries W3C `traceparent`, and otherwise the start of a trace of its
///   own. Attributes: `mcp.method.name`,
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
///
/// Where rmcp takes a `ServerHandler` rather than a service, as its
/// streamable HTTP server does, use [`InstrumentedHandler`].
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

/// The span of an operation `$op`, with every field declared, and an
/// explicit parent if one is given.
macro_rules! server_span {
    ($op:expr, $id:expr $(, parent: $parent:expr)?) => {
        tracing::info_span!(
            target: "oxtel_rmcp",
            $(parent: $parent,)?
            "MCP server operation",
            otel.name = %$op.target.span_name(&$op.method),
            otel.kind = "server",
            mcp.method.name = $op.method.as_str(),
            jsonrpc.request.id = $id,
            mcp.session.id = $op.network.session.as_deref(),
            mcp.protocol.version = $op.protocol_version.as_deref(),
            gen_ai.tool.name = $op.target.tool.as_deref(),
            gen_ai.operation.name = $op.target.operation(),
            gen_ai.prompt.name = $op.target.prompt.as_deref(),
            mcp.resource.uri = $op.target.resource.as_deref(),
            network.transport = $op.network.transport,
            network.protocol.name = $op.network.protocol,
            network.protocol.version = $op.network.version,
            gen_ai.tool.call.arguments = Empty,
            gen_ai.tool.call.result = Empty,
            rpc.response.status_code = Empty,
            error.type = Empty,
            otel.status_code = Empty,
            otel.status_description = Empty,
        )
    };
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

    /// The operation's span. With the caller's context, it continues the
    /// caller's trace; without, it starts a trace of its own, rather than
    /// joining whatever span the transport runs it in, such as rmcp's span
    /// around a whole session.
    fn span(&self, id: Option<String>, caller: Option<SpanContext>) -> tracing::Span {
        match caller {
            Some(caller) => {
                let _parent = attach_remote_parent(caller);
                server_span!(self, id)
            }
            None => server_span!(self, id, parent: None),
        }
    }

    /// Record how it ended on the span and in the duration metric.
    fn finish(
        mut self,
        span: &tracing::Span,
        error: Option<&ErrorData>,
        outcome: Outcome,
        config: &Config,
        started: Instant,
    ) {
        if let Some(version) = outcome.protocol_version {
            span.record("mcp.protocol.version", version.as_str());
            self.protocol_version = Some(version);
        }
        let tool_error = outcome.tool_error;
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

/// What a successful result says about the operation.
#[derive(Default)]
struct Outcome {
    /// The result is a tool's failure.
    tool_error: bool,
    /// The protocol version `initialize` agreed on.
    protocol_version: Option<String>,
}

impl Outcome {
    /// A tool's result, recording its content first when payloads are on.
    fn tool(result: &CallToolResult, span: &tracing::Span, payloads: bool) -> Self {
        if payloads {
            span.record("gen_ai.tool.call.result", payload(&result.content).as_str());
        }
        Outcome {
            tool_error: result.is_error == Some(true),
            ..Outcome::default()
        }
    }
}

/// What both wrappers share: from a request or notification to its span,
/// and from its result to the measurements.
impl<S> Instrumented<S> {
    /// The caller's context from `params._meta`, if propagation is on.
    fn caller(&self, meta: &JsonObject) -> Option<SpanContext> {
        if !self.config.propagate {
            return None;
        }
        extract_trace_context(&MetaReader(meta))
    }

    /// Note that the session is starting.
    fn initializing(&self) {
        let _ = self.session_started.set(Instant::now());
    }

    /// Note the protocol version `initialize` agreed on, which may not be
    /// the one the client asked for: it holds for the rest of the session.
    fn agreed(&self, version: &ProtocolVersion) -> Outcome {
        let _ = self.protocol_version.set(version.as_str().to_string());
        Outcome {
            protocol_version: Some(version.as_str().to_string()),
            ..Outcome::default()
        }
    }

    fn operation(
        &self,
        method: &str,
        target: Target,
        extensions: &rmcp::model::Extensions,
        version: Option<ProtocolVersion>,
    ) -> Operation {
        Operation {
            method: method.to_string(),
            target,
            protocol_version: self
                .protocol_version
                .get()
                .cloned()
                .or_else(|| version.map(|v| v.as_str().to_string())),
            network: Network::of(extensions, self.config.transport),
        }
    }

    /// A request's operation, and its span.
    fn request(
        &self,
        method: &str,
        target: Target,
        context: &RequestContext<RoleServer>,
    ) -> (Operation, tracing::Span) {
        let operation = self.operation(
            method,
            target,
            &context.extensions,
            context.protocol_version(),
        );
        let span = operation.span(Some(context.id.to_string()), self.caller(&context.meta.0));
        (operation, span)
    }

    /// A notification's operation, and its span.
    fn notification(
        &self,
        method: &str,
        context: &NotificationContext<RoleServer>,
    ) -> (Operation, tracing::Span) {
        let version = context
            .peer
            .peer_info()
            .map(|info| info.protocol_version.clone());
        let operation = self.operation(method, Target::default(), &context.extensions, version);
        let span = operation.span(None, self.caller(&context.meta.0));
        (operation, span)
    }

    /// Run `inner` in `span`, then record how it ended. `inspect` reads a
    /// successful result, given the span and whether payloads are on.
    fn observe<'a, T: 'a>(
        &self,
        operation: Operation,
        span: tracing::Span,
        inner: impl Future<Output = Result<T, ErrorData>> + 'a,
        inspect: impl FnOnce(&T, &tracing::Span, bool) -> Outcome + 'a,
    ) -> impl Future<Output = Result<T, ErrorData>> + 'a {
        let config = Arc::clone(&self.config);
        let started = Instant::now();
        let recorded = span.clone();
        async move {
            let result = inner.await;
            let outcome = match &result {
                Ok(r) => inspect(r, &recorded, config.payloads),
                Err(_) => Outcome::default(),
            };
            operation.finish(&recorded, result.as_ref().err(), outcome, &config, started);
            result
        }
        .instrument(span)
    }

    /// Run a handler's notification method in `span`, then record it.
    fn observe_notification<'a>(
        &self,
        (operation, span): (Operation, tracing::Span),
        inner: impl Future<Output = ()> + 'a,
    ) -> impl Future<Output = ()> + 'a {
        let observed = self.observe(
            operation,
            span,
            async move {
                inner.await;
                Ok::<(), ErrorData>(())
            },
            |_, _, _| Outcome::default(),
        );
        async move {
            let _ = observed.await;
        }
    }
}

impl<S: Service<RoleServer>> Service<RoleServer> for Instrumented<S> {
    fn handle_request(
        &self,
        request: ClientRequest,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ServerResult, ErrorData>> + Send + '_ {
        if let ClientRequest::InitializeRequest(_) = &request {
            self.initializing();
        }
        let (operation, span) = self.request(request.method(), Target::of(&request), &context);
        if self.config.payloads
            && let ClientRequest::CallToolRequest(call) = &request
        {
            span.record(
                "gen_ai.tool.call.arguments",
                payload(&call.params.arguments).as_str(),
            );
        }
        let inner = self.inner.handle_request(request, context);
        self.observe(
            operation,
            span,
            inner,
            |result, span, payloads| match result {
                ServerResult::CallToolResult(r) => Outcome::tool(r, span, payloads),
                ServerResult::InitializeResult(r) => self.agreed(&r.protocol_version),
                _ => Outcome::default(),
            },
        )
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
        let (operation, span) = self.notification(&method, &context);
        let inner = self.inner.handle_notification(notification, context);
        self.observe(operation, span, inner, |_, _, _| Outcome::default())
    }

    fn get_info(&self) -> <RoleServer as rmcp::service::ServiceRole>::Info {
        self.inner.get_info()
    }
}

/// An rmcp [`ServerHandler`] instrumented as [`Instrumented`] instruments a
/// service, for where rmcp takes only a `ServerHandler`, such as its
/// streamable HTTP server:
///
/// ```no_run
/// use std::sync::Arc;
///
/// use rmcp::transport::StreamableHttpService;
/// use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
/// # fn app(server: impl rmcp::ServerHandler + Clone) -> axum::Router {
///
/// // A server for each session, each instrumented.
/// let mcp = StreamableHttpService::new(
///     move || Ok(oxtel_rmcp::InstrumentedHandler::new(server.clone())),
///     Arc::new(LocalSessionManager::default()),
///     Default::default(),
/// );
/// axum::Router::new().nest_service("/mcp", mcp)
/// # }
/// ```
///
/// Over HTTP, each span also records `network.transport` `tcp`,
/// `network.protocol.name` and `version`, and, for a session,
/// `mcp.session.id`.
///
/// It sees each request once rmcp has handed it to one of the handler's
/// methods, so it doesn't see a request rmcp turns away itself, such as one
/// for a protocol version the server doesn't support. [`Instrumented`]
/// records those too, where rmcp takes a service. A `subscriptions/listen`
/// request, which lasts as long as its subscription, isn't recorded.
pub struct InstrumentedHandler<S>(Instrumented<S>);

impl<S> InstrumentedHandler<S> {
    /// Instrument `inner`, with propagation and metrics on and payloads off.
    /// Describes the metrics to the current recorder, so make it after
    /// installing one.
    pub fn new(inner: S) -> Self {
        InstrumentedHandler(Instrumented::new(inner))
    }

    /// Continue the caller's trace from `params._meta`. Default: true.
    pub fn with_propagation(self, enabled: bool) -> Self {
        InstrumentedHandler(self.0.with_propagation(enabled))
    }

    /// Record the MCP metrics. Default: true.
    pub fn with_metrics(self, enabled: bool) -> Self {
        InstrumentedHandler(self.0.with_metrics(enabled))
    }

    /// Record tool arguments and results as `gen_ai.tool.call.arguments` and
    /// `gen_ai.tool.call.result`, as JSON cut at 16 KiB. They may hold
    /// personal or secret data. Default: false.
    pub fn with_payloads(self, enabled: bool) -> Self {
        InstrumentedHandler(self.0.with_payloads(enabled))
    }

    /// The `network.transport` to record when the request didn't come over
    /// HTTP: `pipe` for stdio. Over HTTP it's `tcp`.
    pub fn with_transport(self, transport: &'static str) -> Self {
        InstrumentedHandler(self.0.with_transport(transport))
    }

    /// The server inside.
    pub fn inner(&self) -> &S {
        self.0.inner()
    }
}

impl<S> From<Instrumented<S>> for InstrumentedHandler<S> {
    fn from(instrumented: Instrumented<S>) -> Self {
        InstrumentedHandler(instrumented)
    }
}

/// A request method: its span around the inner handler's method.
macro_rules! request {
    ($name:ident, $method:ty, $params:ty, $result:ty $(, $target:expr)?) => {
        fn $name(
            &self,
            request: $params,
            context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<$result, ErrorData>> + MaybeSendFuture + '_ {
            #[allow(unused_variables)]
            let target = Target::default();
            $(let target = $target(&request);)?
            let (operation, span) = self.0.request(<$method>::VALUE, target, &context);
            self.0.observe(
                operation,
                span,
                self.0.inner.$name(request, context),
                |_, _, _| Outcome::default(),
            )
        }
    };
}

/// A request method without parameters.
macro_rules! bare_request {
    ($name:ident, $method:ty, $result:ty) => {
        fn $name(
            &self,
            context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<$result, ErrorData>> + MaybeSendFuture + '_ {
            let (operation, span) = self
                .0
                .request(<$method>::VALUE, Target::default(), &context);
            self.0
                .observe(operation, span, self.0.inner.$name(context), |_, _, _| {
                    Outcome::default()
                })
        }
    };
}

fn prompt(request: &GetPromptRequestParams) -> Target {
    Target {
        prompt: Some(request.name.clone()),
        ..Target::default()
    }
}

fn resource(uri: &str) -> Target {
    Target {
        resource: Some(uri.to_string()),
        ..Target::default()
    }
}

// Every method is forwarded, so the inner handler's own versions are used.
// A method that a later rmcp release adds gets rmcp's default until it's
// forwarded here too.
impl<S: ServerHandler> ServerHandler for InstrumentedHandler<S> {
    bare_request!(ping, PingRequestMethod, ());

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, ErrorData>> + MaybeSendFuture + '_ {
        self.0.initializing();
        let (operation, span) =
            self.0
                .request(InitializeResultMethod::VALUE, Target::default(), &context);
        self.0.observe(
            operation,
            span,
            self.0.inner.initialize(request, context),
            |result, _, _| self.0.agreed(&result.protocol_version),
        )
    }

    fn negotiate_initialize(
        &self,
        request: &InitializeRequestParams,
    ) -> Result<InitializeResult, ErrorData> {
        self.0.inner.negotiate_initialize(request)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        self.0.inner.supported_protocol_versions()
    }

    bare_request!(discover, DiscoverRequestMethod, DiscoverResult);
    request!(
        complete,
        CompleteRequestMethod,
        CompleteRequestParams,
        CompleteResult
    );
    #[allow(deprecated)]
    fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + MaybeSendFuture + '_ {
        let (operation, span) =
            self.0
                .request(SetLevelRequestMethod::VALUE, Target::default(), &context);
        self.0.observe(
            operation,
            span,
            self.0.inner.set_level(request, context),
            |_, _, _| Outcome::default(),
        )
    }
    request!(
        get_prompt,
        GetPromptRequestMethod,
        GetPromptRequestParams,
        GetPromptResponse,
        prompt
    );
    request!(
        list_prompts,
        ListPromptsRequestMethod,
        Option<PaginatedRequestParams>,
        ListPromptsResult
    );
    request!(
        list_resources,
        ListResourcesRequestMethod,
        Option<PaginatedRequestParams>,
        ListResourcesResult
    );
    request!(
        list_resource_templates,
        ListResourceTemplatesRequestMethod,
        Option<PaginatedRequestParams>,
        ListResourceTemplatesResult
    );
    request!(
        read_resource,
        ReadResourceRequestMethod,
        ReadResourceRequestParams,
        ReadResourceResponse,
        |r: &ReadResourceRequestParams| resource(&r.uri)
    );

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        self.0.inner.accepted_subscription_filter(requested)
    }

    fn listen(
        &self,
        context: SubscriptionContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + MaybeSendFuture + '_ {
        self.0.inner.listen(context)
    }

    #[allow(deprecated)]
    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + MaybeSendFuture + '_ {
        let (operation, span) = self.0.request(
            SubscribeRequestMethod::VALUE,
            resource(&request.uri),
            &context,
        );
        self.0.observe(
            operation,
            span,
            self.0.inner.subscribe(request, context),
            |_, _, _| Outcome::default(),
        )
    }

    #[allow(deprecated)]
    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + MaybeSendFuture + '_ {
        let (operation, span) = self.0.request(
            UnsubscribeRequestMethod::VALUE,
            resource(&request.uri),
            &context,
        );
        self.0.observe(
            operation,
            span,
            self.0.inner.unsubscribe(request, context),
            |_, _, _| Outcome::default(),
        )
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let target = Target {
            tool: Some(request.name.to_string()),
            ..Target::default()
        };
        let (operation, span) = self
            .0
            .request(CallToolRequestMethod::VALUE, target, &context);
        if self.0.config.payloads {
            span.record(
                "gen_ai.tool.call.arguments",
                payload(&request.arguments).as_str(),
            );
        }
        self.0.observe(
            operation,
            span,
            self.0.inner.call_tool(request, context),
            |response, span, payloads| match response {
                CallToolResponse::Complete(r) => Outcome::tool(r, span, payloads),
                _ => Outcome::default(),
            },
        )
    }

    request!(
        list_tools,
        ListToolsRequestMethod,
        Option<PaginatedRequestParams>,
        ListToolsResult
    );

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.0.inner.get_tool(name)
    }

    fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CustomResult, ErrorData>> + MaybeSendFuture + '_ {
        let (operation, span) = self.0.request(&request.method, Target::default(), &context);
        self.0.observe(
            operation,
            span,
            self.0.inner.on_custom_request(request, context),
            |_, _, _| Outcome::default(),
        )
    }

    fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let observed = self
            .0
            .notification(CancelledNotificationMethod::VALUE, &context);
        self.0
            .observe_notification(observed, self.0.inner.on_cancelled(notification, context))
    }

    fn on_progress(
        &self,
        notification: ProgressNotificationParam,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let observed = self
            .0
            .notification(ProgressNotificationMethod::VALUE, &context);
        self.0
            .observe_notification(observed, self.0.inner.on_progress(notification, context))
    }

    fn on_initialized(
        &self,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let observed = self
            .0
            .notification(InitializedNotificationMethod::VALUE, &context);
        self.0
            .observe_notification(observed, self.0.inner.on_initialized(context))
    }

    fn on_roots_list_changed(
        &self,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let observed = self
            .0
            .notification(RootsListChangedNotificationMethod::VALUE, &context);
        self.0
            .observe_notification(observed, self.0.inner.on_roots_list_changed(context))
    }

    fn on_custom_notification(
        &self,
        notification: CustomNotification,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let observed = self.0.notification(&notification.method, &context);
        self.0.observe_notification(
            observed,
            self.0.inner.on_custom_notification(notification, context),
        )
    }

    fn get_info(&self) -> ServerConfig {
        self.0.inner.get_info()
    }

    request!(get_task, GetTaskMethod, GetTaskParams, GetTaskResult);
    request!(update_task, UpdateTaskMethod, UpdateTaskParams, ());
    request!(cancel_task, CancelTaskMethod, CancelTaskParams, ());
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
