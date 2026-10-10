//! OpenTelemetry declarative configuration for
//! [oxtel](https://github.com/agentstatelabs/oxtel): the YAML file
//! `OTEL_CONFIG_FILE` names, read into a typed [`Config`].
//!
//! - Environment variables are substituted in values (`${NAME}`,
//!   `${env:NAME}`, `${NAME:-default}`, `$$` for a literal `$`). Unquoted
//!   values are then typed as YAML scalars, so `${OTEL_SDK_DISABLED:-false}`
//!   is a boolean, while quoted values stay strings. A substituted value is
//!   never read as YAML structure.
//! - `file_format` must be `1.x`.
//! - Values of the wrong type are errors, naming the field's path.
//! - Keys and components oxtel doesn't support, such as views or the gRPC
//!   exporter, are reported in [`Parsed::warnings`] and otherwise ignored.
//!
//! The `oxtel` crate builds its pipelines from the result when
//! `OTEL_CONFIG_FILE` is set.
//!
//! ```
//! let parsed = oxtel_config::parse_with(
//!     "file_format: \"1.2\"\ndisabled: ${DISABLED:-false}\n",
//!     &|_| None,
//! )
//! .unwrap();
//! assert!(!parsed.config.disabled);
//! ```

mod model;
mod yaml;

use std::fmt;
use std::path::Path;

pub use model::{
    AttributeValue, Config, ExporterConfig, Limits, LoggerProviderConfig, MeterProviderConfig,
    OtlpHttpConfig, ProcessorConfig, ReaderConfig, ResourceConfig, SamplerConfig,
    TracerProviderConfig,
};

/// A configuration and what in it was ignored.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// The configuration.
    pub config: Config,
    /// Keys and components oxtel doesn't support, one message each.
    pub warnings: Vec<String>,
}

/// Why a configuration couldn't be read.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// The file couldn't be read.
    Io(String),
    /// The YAML is malformed, or a substitution is invalid.
    Parse {
        /// The line, from 1.
        line: usize,
        /// The column, from 1.
        column: usize,
        /// What's wrong.
        message: String,
    },
    /// `file_format` is missing, or isn't a version oxtel reads.
    FileFormat(Option<String>),
    /// A value has the wrong type.
    Invalid {
        /// Where, such as `tracer_provider.processors[0].batch.max_queue_size`.
        path: String,
        /// What it should be.
        expected: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "couldn't read the configuration file: {e}"),
            Error::Parse {
                line,
                column,
                message,
            } => write!(
                f,
                "configuration file, line {line}, column {column}: {message}"
            ),
            Error::FileFormat(None) => write!(f, "the configuration file has no `file_format`"),
            Error::FileFormat(Some(v)) => {
                write!(f, "`file_format: {v}` isn't supported; oxtel reads 1.x")
            }
            Error::Invalid { path, expected } => {
                write!(f, "`{path}` in the configuration file should be {expected}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Read the configuration in `text`, substituting the process's
/// environment variables.
pub fn parse(text: &str) -> Result<Parsed, Error> {
    parse_with(text, &|name| std::env::var(name).ok())
}

/// Read the configuration in `text`, substituting variables from `lookup`.
pub fn parse_with(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Parsed, Error> {
    let root = yaml::parse(text, lookup)?;
    let mut warnings = Vec::new();
    let config = model::config(&root, &mut warnings)?;
    Ok(Parsed { config, warnings })
}

/// Read the configuration file at `path`.
pub fn from_file(path: impl AsRef<Path>) -> Result<Parsed, Error> {
    let text = std::fs::read_to_string(path.as_ref())
        .map_err(|e| Error::Io(format!("{}: {e}", path.as_ref().display())))?;
    parse(&text)
}
