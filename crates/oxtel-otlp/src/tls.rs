//! TLS certificate files: a CA bundle to verify the receiver with, and a
//! client certificate and key for mutual TLS.

use std::path::{Path, PathBuf};

/// The TLS files one signal's connection uses, as the
/// `OTEL_EXPORTER_OTLP_[{SIGNAL}_]CERTIFICATE`, `_CLIENT_CERTIFICATE` and
/// `_CLIENT_KEY` variables name them. Each is a PEM file. They apply only
/// to `https` endpoints.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TlsFiles {
    /// The certificates that may sign the receiver's certificate. When set,
    /// they're the only ones trusted, in place of the bundled roots.
    pub certificate: Option<PathBuf>,
    /// The client certificate (or chain) for mutual TLS.
    pub client_certificate: Option<PathBuf>,
    /// The client certificate's private key.
    pub client_key: Option<PathBuf>,
}

impl TlsFiles {
    /// No files: the bundled roots, and no client certificate.
    pub fn is_empty(&self) -> bool {
        self == &TlsFiles::default()
    }

    /// The client certificate and key, both or neither.
    #[cfg_attr(not(feature = "http-proto"), allow(dead_code))]
    pub(crate) fn client_identity(&self) -> Result<Option<(&Path, &Path)>, String> {
        match (&self.client_certificate, &self.client_key) {
            (Some(cert), Some(key)) => Ok(Some((cert, key))),
            (None, None) => Ok(None),
            (Some(cert), None) => Err(format!(
                "the client certificate {} has no key; set OTEL_EXPORTER_OTLP_CLIENT_KEY too",
                cert.display()
            )),
            (None, Some(key)) => Err(format!(
                "the client key {} has no certificate; set OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE too",
                key.display()
            )),
        }
    }
}

/// The file at `path`, or why it can't be read.
#[cfg_attr(not(feature = "http-proto"), allow(dead_code))]
pub(crate) fn read(path: &Path, what: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("can't read the {what} {}: {e}", path.display()))
}
