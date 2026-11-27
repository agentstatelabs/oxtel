//! OTLP/HTTP to a receiver that speaks TLS and asks for a client
//! certificate: the CA file verifies the receiver, and the client
//! certificate and key get through its mutual TLS. Certificates are made on
//! the spot.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{Pki, span};
use oxtel_otlp::{OtlpHttpExporter, OtlpHttpExporterBuilder, Signal};
use oxtel_sdk::Resource;
use oxtel_sdk::export::ExportError;
use oxtel_sdk::trace::SpanExporter;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

mod common;

/// What the receiver saw of each connection: the request path, and whether
/// the client presented a certificate; or the handshake's failure.
type Seen = Arc<Mutex<Vec<Result<(String, bool), String>>>>;

/// A receiver on `https://localhost:{port}` that requires a client
/// certificate signed by `pki`'s CA, and answers every request with 200.
fn receiver(pki: &Pki) -> (u16, Seen) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let ca = CertificateDer::from_pem_file(pki.path("ca.pem")).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let verifier =
        rustls::server::WebPkiClientVerifier::builder_with_provider(roots.into(), provider.clone())
            .build()
            .unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![CertificateDer::from_pem_file(pki.path("server.pem")).unwrap()],
                PrivateKeyDer::from_pem_file(pki.path("server.key")).unwrap(),
            )
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Seen::default();
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let connection = rustls::ServerConnection::new(Arc::clone(&config)).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, stream);
            let result = serve(&mut tls);
            record.lock().unwrap().push(result);
        }
    });
    (port, seen)
}

/// Reads one request and answers 200.
fn serve(
    tls: &mut rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>,
) -> Result<(String, bool), String> {
    let mut reader = BufReader::new(&mut *tls);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| e.to_string())?;
    let mut len = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap();
        }
    }
    reader
        .read_exact(&mut vec![0; len])
        .map_err(|e| e.to_string())?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let client_cert = tls.conn.peer_certificates().is_some();
    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .map_err(|e| e.to_string())?;
    tls.flush().map_err(|e| e.to_string())?;
    Ok((path, client_cert))
}

fn export(exporter: &OtlpHttpExporter) -> Result<(), ExportError> {
    SpanExporter::export(
        exporter,
        &span(),
        &Resource::builder().without_env().build(),
    )
}

fn to(port: u16) -> OtlpHttpExporterBuilder {
    OtlpHttpExporterBuilder::new()
        .with_endpoint(&format!("https://localhost:{port}"))
        .with_timeout(std::time::Duration::from_secs(2))
        .with_initial_backoff(std::time::Duration::from_millis(10))
}

#[test]
fn the_ca_file_verifies_the_receiver_and_the_client_certificate_gets_in() {
    let pki = Pki::new("mutual");
    let (port, seen) = receiver(&pki);
    let exporter = to(port)
        .with_certificate(pki.path("ca.pem"))
        .with_client_certificate(pki.path("client.pem"), pki.path("client.key"))
        .build();
    assert_eq!(exporter.setup_error(Signal::Traces), None);
    export(&exporter).expect("exported over mutual TLS");
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [Ok(("/v1/traces".to_string(), true))]
    );
}

#[test]
fn without_the_ca_file_the_receiver_isnt_trusted() {
    let pki = Pki::new("untrusted");
    let (port, _) = receiver(&pki);
    let exporter = to(port)
        .with_client_certificate(pki.path("client.pem"), pki.path("client.key"))
        .build();
    let Err(ExportError::Transport(why)) = export(&exporter) else {
        panic!("a receiver the bundled roots don't know must fail")
    };
    assert!(why.to_lowercase().contains("certificate"), "{why}");
}

#[test]
fn without_a_client_certificate_mutual_tls_refuses() {
    let pki = Pki::new("no-client");
    let (port, _) = receiver(&pki);
    let exporter = to(port).with_certificate(pki.path("ca.pem")).build();
    assert!(
        matches!(export(&exporter), Err(ExportError::Transport(_))),
        "the receiver requires a client certificate"
    );
}

#[test]
fn files_that_cant_be_used_are_named() {
    let missing = Path::new("/nonexistent/oxtel/ca.pem");
    let exporter = OtlpHttpExporterBuilder::new()
        .with_endpoint("https://localhost:1")
        .with_certificate(missing)
        .build();
    let why = exporter.setup_error(Signal::Traces).expect("an error");
    assert!(
        why.contains("can't read the CA certificate file /nonexistent/oxtel/ca.pem"),
        "{why}"
    );
    assert!(matches!(
        export(&exporter),
        Err(ExportError::Transport(e)) if e == why
    ));

    let pki = Pki::new("half");
    let exporter = OtlpHttpExporterBuilder::new()
        .with_tls(
            Signal::Logs,
            oxtel_otlp::TlsFiles {
                client_certificate: Some(pki.path("client.pem")),
                ..Default::default()
            },
        )
        .build();
    assert!(
        exporter
            .setup_error(Signal::Logs)
            .is_some_and(|why| why.contains("has no key")),
        "{:?}",
        exporter.setup_error(Signal::Logs)
    );
    assert_eq!(exporter.setup_error(Signal::Traces), None, "only logs");

    let exporter = OtlpHttpExporterBuilder::new()
        .with_certificate(pki.path("client.key"))
        .build();
    assert!(
        exporter
            .setup_error(Signal::Metrics)
            .is_some_and(|why| why.contains("holds no PEM certificate")),
        "{:?}",
        exporter.setup_error(Signal::Metrics)
    );
}

#[test]
fn tls_files_come_from_the_environment() {
    // `with_lookup` isn't public, so this reads the process environment in
    // a child process, as oxtel's own environment tests do.
    if std::env::var_os("OXTEL_TLS_CHILD").is_some() {
        let exporter = OtlpHttpExporter::builder().build();
        assert!(
            exporter
                .setup_error(Signal::Traces)
                .is_some_and(|why| why.contains("/nonexistent/traces-ca.pem")),
            "the signal's own variable wins: {:?}",
            exporter.setup_error(Signal::Traces)
        );
        assert!(
            exporter
                .setup_error(Signal::Logs)
                .is_some_and(|why| why.contains("/nonexistent/ca.pem")),
            "{:?}",
            exporter.setup_error(Signal::Logs)
        );
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tls_files_come_from_the_environment"])
        .env("OXTEL_TLS_CHILD", "1")
        .env("OTEL_EXPORTER_OTLP_CERTIFICATE", "/nonexistent/ca.pem")
        .env(
            "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
            "/nonexistent/traces-ca.pem",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
