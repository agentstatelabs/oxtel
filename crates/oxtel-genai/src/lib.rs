//! Spans and metrics for calls to generative AI models, as the
//! OpenTelemetry GenAI semantic conventions describe them: a span for each
//! call, how long it took, and the tokens it used. Part of
//! [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! It works with any model client: say what the call is when it starts,
//! make the call inside its span, and say how it went when it ends.
//!
//! ```
//! # struct Reply { model: String, prompt_eval_count: u64, eval_count: u64 }
//! # fn ollama_chat() -> Result<Reply, ()> {
//! #     Ok(Reply { model: "llama3.2:latest".into(), prompt_eval_count: 26, eval_count: 298 })
//! # }
//! let call = oxtel_genai::chat("ollama", "llama3.2")
//!     .server("localhost", 11434)
//!     .temperature(0.2)
//!     .start();
//! // Make the call inside the span: in async code, `.instrument(call.span().clone())`.
//! match call.span().in_scope(ollama_chat) {
//!     Ok(reply) => call
//!         .response_model(&reply.model)
//!         .input_tokens(reply.prompt_eval_count)
//!         .output_tokens(reply.eval_count)
//!         .finish(),
//!     Err(_) => call.fail("timeout"),
//! }
//! ```
//!
//! That records:
//!
//! - **A client span** named `chat llama3.2`, with `gen_ai.operation.name`,
//!   `gen_ai.provider.name`, `gen_ai.request.model`, `server.address` and
//!   `server.port`, the request's settings, and the response's model, id
//!   and token counts. A failed call is an error, with `error.type`.
//! - **`gen_ai.client.inference.duration`**, in seconds.
//! - **Token usage**: `gen_ai.client.inference.usage.input_tokens` and
//!   `.output_tokens`, counters by `gen_ai.token.modality` for totals and
//!   cost, and `gen_ai.client.inference.operation.input_tokens` and
//!   `.output_tokens`, histograms of the tokens each call used.
//! - For a streamed response, with [`Call::chunk`] as each chunk arrives,
//!   `gen_ai.client.inference.time_to_first_chunk` and
//!   `gen_ai.client.inference.time_per_output_chunk`.
//!
//! An [`embeddings`] call isn't inference, so it records
//! `gen_ai.client.operation.duration` instead, and its input tokens on its
//! span only.
//!
//! **No prompt or completion content is recorded**: messages, system
//! instructions and tool definitions can hold anything, and they're the
//! conventions' opt-in attributes.
//!
//! Spans are recorded with `tracing` and metrics with `metrics`, so they go
//! wherever the program's subscriber and recorder send them: with
//! `oxtel::init()`, to OTLP or Prometheus, with the buckets the conventions
//! advise. Arrays can't be `tracing` fields, so `gen_ai.response.finish_reasons`
//! and `gen_ai.request.stop_sequences` aren't recorded. The GenAI
//! conventions are still in development, so the names may change in a
//! later release, as they do upstream.

use std::fmt;
use std::time::Instant;

use metrics::Label;
use tracing::Span;

/// What a call asks of the model: `gen_ai.operation.name`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Operation {
    /// A chat completion, such as OpenAI's Chat API or Ollama's `/api/chat`.
    Chat,
    /// A plain text completion, such as Ollama's `/api/generate`.
    TextCompletion,
    /// Multimodal content generation, such as Gemini's Generate Content.
    GenerateContent,
    /// Embeddings for some input.
    Embeddings,
}

impl Operation {
    /// The name the conventions give it, such as `chat`.
    pub fn name(self) -> &'static str {
        match self {
            Operation::Chat => "chat",
            Operation::TextCompletion => "text_completion",
            Operation::GenerateContent => "generate_content",
            Operation::Embeddings => "embeddings",
        }
    }

    /// Inference generates output, and gets the `gen_ai.client.inference.*`
    /// metrics; other operations get `gen_ai.client.operation.duration`.
    fn is_inference(self) -> bool {
        !matches!(self, Operation::Embeddings)
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What kind of input or output tokens are: `gen_ai.token.modality`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Modality {
    /// Text tokens.
    Text,
    /// Image tokens.
    Image,
    /// Audio tokens.
    Audio,
    /// Tokens of a kind the provider doesn't say.
    Unknown,
}

impl Modality {
    const ALL: [Modality; 4] = [
        Modality::Text,
        Modality::Image,
        Modality::Audio,
        Modality::Unknown,
    ];

    /// The name the conventions give it, such as `text`.
    pub fn name(self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Audio => "audio",
            Modality::Unknown => "unknown",
        }
    }
}

/// A chat completion call to `model` from `provider`, such as `openai`,
/// `anthropic`, `gcp.gemini` or `ollama`: see
/// [`gen_ai.provider.name`](https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/)
/// for the names the conventions know.
pub fn chat(provider: impl Into<String>, model: impl Into<String>) -> CallBuilder {
    CallBuilder::new(Operation::Chat, provider, model)
}

/// A text completion call.
pub fn text_completion(provider: impl Into<String>, model: impl Into<String>) -> CallBuilder {
    CallBuilder::new(Operation::TextCompletion, provider, model)
}

/// A multimodal content generation call.
pub fn generate_content(provider: impl Into<String>, model: impl Into<String>) -> CallBuilder {
    CallBuilder::new(Operation::GenerateContent, provider, model)
}

/// An embeddings call.
pub fn embeddings(provider: impl Into<String>, model: impl Into<String>) -> CallBuilder {
    CallBuilder::new(Operation::Embeddings, provider, model)
}

/// A model call about to be made. Each setting is recorded on the span, as
/// the conventions name it; leave out what the request doesn't set.
#[derive(Clone, Debug)]
#[must_use = "a call is recorded only once it's started"]
pub struct CallBuilder {
    operation: Operation,
    provider: String,
    model: String,
    server: Option<(String, u16)>,
    max_tokens: Option<i64>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    frequency_penalty: Option<f64>,
    presence_penalty: Option<f64>,
    seed: Option<i64>,
    choices: Option<i64>,
    stream: bool,
    conversation_id: Option<String>,
}

impl CallBuilder {
    /// A call for `operation` to `model` from `provider`.
    pub fn new(
        operation: Operation,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> CallBuilder {
        CallBuilder {
            operation,
            provider: provider.into(),
            model: model.into(),
            server: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            choices: None,
            stream: false,
            conversation_id: None,
        }
    }

    /// The host and port the call goes to: `server.address` and
    /// `server.port`.
    pub fn server(mut self, address: impl Into<String>, port: u16) -> Self {
        self.server = Some((address.into(), port));
        self
    }

    /// `gen_ai.request.max_tokens`.
    pub fn max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = Some(tokens.into());
        self
    }

    /// `gen_ai.request.temperature`.
    pub fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// `gen_ai.request.top_p`.
    pub fn top_p(mut self, top_p: f64) -> Self {
        self.top_p = Some(top_p);
        self
    }

    /// `gen_ai.request.top_k`.
    pub fn top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k.into());
        self
    }

    /// `gen_ai.request.frequency_penalty`.
    pub fn frequency_penalty(mut self, penalty: f64) -> Self {
        self.frequency_penalty = Some(penalty);
        self
    }

    /// `gen_ai.request.presence_penalty`.
    pub fn presence_penalty(mut self, penalty: f64) -> Self {
        self.presence_penalty = Some(penalty);
        self
    }

    /// `gen_ai.request.seed`.
    pub fn seed(mut self, seed: i64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// How many candidate responses the request asks for:
    /// `gen_ai.request.choice.count`, recorded unless it's 1.
    pub fn choices(mut self, count: u32) -> Self {
        self.choices = (count != 1).then_some(count.into());
        self
    }

    /// The response is streamed: `gen_ai.request.stream`. Call
    /// [`Call::chunk`] as each chunk arrives.
    pub fn streamed(mut self) -> Self {
        self.stream = true;
        self
    }

    /// The conversation the call belongs to: `gen_ai.conversation.id`.
    pub fn conversation_id(mut self, id: impl Into<String>) -> Self {
        self.conversation_id = Some(id.into());
        self
    }

    /// Starts the call's span and clock. Make the call inside the span.
    pub fn start(self) -> Call {
        describe_metrics();
        let operation = self.operation;
        let (address, port) = match &self.server {
            Some((address, port)) => (Some(address.as_str()), Some(*port)),
            None => (None, None),
        };
        let span = tracing::info_span!(
            target: "oxtel_genai",
            "GenAI client call",
            otel.name = format!("{} {}", operation.name(), self.model),
            otel.kind = "client",
            gen_ai.operation.name = operation.name(),
            gen_ai.provider.name = self.provider.as_str(),
            gen_ai.request.model = self.model.as_str(),
            server.address = address,
            server.port = port,
            gen_ai.request.max_tokens = self.max_tokens,
            gen_ai.request.temperature = self.temperature,
            gen_ai.request.top_p = self.top_p,
            gen_ai.request.top_k = self.top_k,
            gen_ai.request.frequency_penalty = self.frequency_penalty,
            gen_ai.request.presence_penalty = self.presence_penalty,
            gen_ai.request.seed = self.seed,
            gen_ai.request.choice.count = self.choices,
            gen_ai.request.stream = self.stream.then_some(true),
            gen_ai.conversation.id = self.conversation_id.as_deref(),
            gen_ai.response.id = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            gen_ai.response.time_to_first_chunk = tracing::field::Empty,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.embeddings.dimension.count = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        let mut labels = vec![
            Label::new("gen_ai.operation.name", operation.name()),
            Label::new("gen_ai.provider.name", self.provider),
            Label::new("gen_ai.request.model", self.model),
        ];
        if let Some((address, port)) = self.server {
            labels.push(Label::new("server.address", address));
            labels.push(Label::new("server.port", port.to_string()));
        }
        Call {
            span,
            started: Instant::now(),
            operation,
            labels,
            response_model: None,
            input: Tokens::default(),
            output: Tokens::default(),
            last_chunk: None,
        }
    }
}

/// Token counts by modality, and whether any were given.
#[derive(Debug, Default)]
struct Tokens {
    counts: [u64; 4],
    known: bool,
}

impl Tokens {
    fn add(&mut self, modality: Modality, tokens: u64) {
        let i = Modality::ALL
            .iter()
            .position(|m| *m == modality)
            .expect("every modality is in ALL");
        self.counts[i] += tokens;
        self.known = true;
    }

    fn total(&self) -> Option<u64> {
        self.known.then(|| self.counts.iter().sum())
    }

    fn by_modality(&self) -> impl Iterator<Item = (Modality, u64)> + '_ {
        Modality::ALL
            .into_iter()
            .zip(self.counts)
            .filter(|&(_, n)| n > 0)
    }
}

/// A model call under way. Say how it went with [`finish`](Call::finish)
/// or [`fail`](Call::fail); a call dropped without either, as when its
/// future is cancelled, ends its span without a status and records no
/// metrics.
#[derive(Debug)]
#[must_use = "a call records its metrics when it finishes or fails"]
pub struct Call {
    span: Span,
    started: Instant,
    operation: Operation,
    /// The labels every metric of the call has.
    labels: Vec<Label>,
    response_model: Option<String>,
    input: Tokens,
    output: Tokens,
    last_chunk: Option<Instant>,
}

impl Call {
    /// The call's span. Make the call inside it, so what the client logs
    /// and the HTTP request it makes are part of it.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// A chunk of a streamed response arrived. The first records
    /// `gen_ai.response.time_to_first_chunk` and
    /// `gen_ai.client.inference.time_to_first_chunk`, and each later one
    /// `gen_ai.client.inference.time_per_output_chunk`.
    pub fn chunk(&mut self) {
        let now = Instant::now();
        let labels = self.metric_labels(None);
        match self.last_chunk.replace(now) {
            None => {
                let seconds = (now - self.started).as_secs_f64();
                self.span
                    .record("gen_ai.response.time_to_first_chunk", seconds);
                metrics::histogram!("gen_ai.client.inference.time_to_first_chunk", labels)
                    .record(seconds);
            }
            Some(previous) => {
                metrics::histogram!("gen_ai.client.inference.time_per_output_chunk", labels)
                    .record((now - previous).as_secs_f64());
            }
        }
    }

    /// The id the provider gave the response: `gen_ai.response.id`.
    pub fn response_id(self, id: &str) -> Self {
        self.span.record("gen_ai.response.id", id);
        self
    }

    /// The model that answered, which can be more exact than the one asked
    /// for, such as `llama3.2:latest`: `gen_ai.response.model`.
    pub fn response_model(mut self, model: &str) -> Self {
        self.span.record("gen_ai.response.model", model);
        self.response_model = Some(model.to_string());
        self
    }

    /// The input tokens the call used, as text. When the provider reports
    /// both used and billable tokens, give the billable ones.
    pub fn input_tokens(self, tokens: u64) -> Self {
        self.input_tokens_of(Modality::Text, tokens)
    }

    /// The input tokens of one modality the call used. Calls for different
    /// modalities add up.
    pub fn input_tokens_of(mut self, modality: Modality, tokens: u64) -> Self {
        self.input.add(modality, tokens);
        self
    }

    /// The output tokens the call used, as text, including any spent on
    /// reasoning.
    pub fn output_tokens(self, tokens: u64) -> Self {
        self.output_tokens_of(Modality::Text, tokens)
    }

    /// The output tokens of one modality the call used. Calls for different
    /// modalities add up.
    pub fn output_tokens_of(mut self, modality: Modality, tokens: u64) -> Self {
        self.output.add(modality, tokens);
        self
    }

    /// How many dimensions each embedding has:
    /// `gen_ai.embeddings.dimension.count`.
    pub fn dimensions(self, count: u64) -> Self {
        self.span.record("gen_ai.embeddings.dimension.count", count);
        self
    }

    /// The call succeeded: records its duration and token usage.
    pub fn finish(self) {
        self.end(None);
    }

    /// The call failed. `error_type` says why, in few enough words to be a
    /// metric label: the provider's error code, such as
    /// `rate_limit_exceeded`, an HTTP status such as `429`, or `timeout`.
    /// Token counts given before failing are still recorded.
    pub fn fail(self, error_type: &str) {
        self.end(Some(error_type));
    }

    /// The labels of the call's metrics: what it was, what answered, and
    /// how it failed if it did.
    fn metric_labels(&self, error: Option<&str>) -> Vec<Label> {
        let mut labels = self.labels.clone();
        if let Some(model) = &self.response_model {
            labels.push(Label::new("gen_ai.response.model", model.clone()));
        }
        if let Some(error) = error {
            labels.push(Label::new("error.type", error.to_string()));
        }
        labels
    }

    fn end(self, error: Option<&str>) {
        let elapsed = self.started.elapsed().as_secs_f64();
        if let Some(tokens) = self.input.total() {
            self.span.record("gen_ai.usage.input_tokens", tokens);
        }
        if let Some(tokens) = self.output.total() {
            self.span.record("gen_ai.usage.output_tokens", tokens);
        }
        if let Some(error) = error {
            self.span.record("error.type", error);
            self.span.record("otel.status_code", "error");
        }
        let labels = self.metric_labels(error);
        if !self.operation.is_inference() {
            metrics::histogram!("gen_ai.client.operation.duration", labels).record(elapsed);
            return;
        }
        metrics::histogram!("gen_ai.client.inference.duration", labels.clone()).record(elapsed);
        // Token usage only when the call says what it used: the conventions
        // forbid reporting counts that can't be had.
        for (tokens, histogram, counter) in [
            (
                &self.input,
                "gen_ai.client.inference.operation.input_tokens",
                "gen_ai.client.inference.usage.input_tokens",
            ),
            (
                &self.output,
                "gen_ai.client.inference.operation.output_tokens",
                "gen_ai.client.inference.usage.output_tokens",
            ),
        ] {
            let Some(total) = tokens.total() else {
                continue;
            };
            metrics::histogram!(histogram, labels.clone()).record(total as f64);
            // The usage counters have no error.type.
            let usage = self.metric_labels(None);
            for (modality, count) in tokens.by_modality() {
                let mut usage = usage.clone();
                usage.push(Label::new("gen_ai.token.modality", modality.name()));
                metrics::counter!(counter, usage).increment(count);
            }
        }
    }
}

/// Gives the current `metrics` recorder the units and descriptions of the
/// GenAI client metrics, so durations are in seconds with the conventions'
/// buckets. [`CallBuilder::start`] does this each time, so there's no need
/// to call it yourself.
pub fn describe_metrics() {
    for (name, description) in [
        (
            "gen_ai.client.inference.duration",
            "GenAI client inference operation duration.",
        ),
        (
            "gen_ai.client.operation.duration",
            "GenAI operation duration.",
        ),
        (
            "gen_ai.client.inference.time_to_first_chunk",
            "Time to receive the first chunk of a streamed response.",
        ),
        (
            "gen_ai.client.inference.time_per_output_chunk",
            "Time per output chunk, after the first.",
        ),
    ] {
        metrics::describe_histogram!(name, metrics::Unit::Seconds, description);
    }
    metrics::describe_histogram!(
        "gen_ai.client.inference.operation.input_tokens",
        "The number of input (prompt) tokens used per inference operation."
    );
    metrics::describe_histogram!(
        "gen_ai.client.inference.operation.output_tokens",
        "The number of output (completion) tokens used per inference operation."
    );
    metrics::describe_counter!(
        "gen_ai.client.inference.usage.input_tokens",
        "The number of input (prompt) tokens used, including cached tokens."
    );
    metrics::describe_counter!(
        "gen_ai.client.inference.usage.output_tokens",
        "The number of output (completion) tokens used, including reasoning tokens."
    );
}
