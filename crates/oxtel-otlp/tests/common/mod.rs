//! What the TLS and gRPC tests share.

#![allow(dead_code)]

use std::path::PathBuf;

use oxtel_sdk::Resource;
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, TracerProvider};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use tracing_subscriber::layer::SubscriberExt;

/// A CA, a server certificate for `localhost` and a client certificate it
/// signed, written as PEM files to a directory of their own.
pub struct Pki {
    dir: PathBuf,
}

impl Pki {
    pub fn new(name: &str) -> Pki {
        let dir = std::env::temp_dir().join(format!("oxtel-tls-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
        let issue = |names: Vec<String>, cert: &str, key: &str| {
            let pair = KeyPair::generate().unwrap();
            let signed = CertificateParams::new(names)
                .unwrap()
                .signed_by(&pair, &*ca)
                .unwrap();
            std::fs::write(dir.join(cert), signed.pem()).unwrap();
            std::fs::write(dir.join(key), pair.serialize_pem()).unwrap();
        };
        issue(vec!["localhost".into()], "server.pem", "server.key");
        issue(vec!["oxtel-client".into()], "client.pem", "client.key");
        std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
        Pki { dir }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One finished span, to export.
pub fn span() -> Vec<SpanData> {
    let memory = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(memory.clone())
        .build();
    let subscriber = tracing_subscriber::registry().with(traces.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("checkout").in_scope(|| {});
    });
    memory.finished_spans()
}
