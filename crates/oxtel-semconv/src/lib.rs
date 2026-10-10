//! The OpenTelemetry semantic conventions as Rust constants: attribute names
//! and metric definitions. Generated with OpenTelemetry Weaver from pinned
//! releases, so every name matches the conventions exactly. Part of
//! [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! ```
//! use oxtel_semconv::{attribute, metric};
//!
//! assert_eq!(attribute::HTTP_REQUEST_METHOD, "http.request.method");
//! let duration = metric::HTTP_SERVER_REQUEST_DURATION;
//! assert_eq!((duration.name, duration.unit), ("http.server.request.duration", "s"));
//! ```
//!
//! - [`attribute`] and [`metric`] come from
//!   [semantic-conventions](https://github.com/open-telemetry/semantic-conventions)
//!   v1.44.0; [`SCHEMA_URL`] names that release.
//! - [`genai`] holds the generative AI and MCP conventions, which live in
//!   [semantic-conventions-genai](https://github.com/open-telemetry/semantic-conventions-genai).
//! - Only stable conventions are compiled by default. The `experimental`
//!   feature adds those in development, which can change in any release.
//! - Deprecated conventions are left out.

/// Attribute names.
pub mod attribute {
    // The conventions' own prose, kept as written.
    #![allow(clippy::all, rustdoc::all)]
    include!("generated/attribute.rs");
}

/// Metric definitions.
pub mod metric {
    #![allow(clippy::all, rustdoc::all)]
    use crate::{Instrument, Metric};
    include!("generated/metric.rs");
}

/// The generative AI and MCP conventions (all in development, so all behind
/// the `experimental` feature).
pub mod genai {
    /// Attribute names: `gen_ai.*`, `mcp.*` and the providers'.
    pub mod attribute {
        #![allow(clippy::all, rustdoc::all)]
        include!("generated/genai/attribute.rs");
    }

    /// Metric definitions: `gen_ai.*` and `mcp.*`.
    pub mod metric {
        #![allow(clippy::all, rustdoc::all)]
        #[allow(unused_imports)]
        use crate::{Instrument, Metric};
        include!("generated/genai/metric.rs");
    }
}

/// The schema URL of the conventions release these constants come from.
pub const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.44.0";

/// A metric as the conventions define it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metric {
    /// Its name, such as `http.server.request.duration`.
    pub name: &'static str,
    /// The kind of instrument that records it.
    pub instrument: Instrument,
    /// Its unit in UCUM, such as `s` or `By`, or a `{annotation}`.
    pub unit: &'static str,
    /// What it measures, in one sentence.
    pub brief: &'static str,
}

/// The kinds of instrument the conventions use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instrument {
    /// A monotonic sum: a `metrics` counter.
    Counter,
    /// A sum that goes up and down: a `metrics` gauge, incremented and
    /// decremented.
    UpDownCounter,
    /// The latest value: a `metrics` gauge.
    Gauge,
    /// A distribution: a `metrics` histogram.
    Histogram,
}
