//! The built-in `/metrics` endpoint, over real sockets.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use oxtel_prometheus::{PrometheusExporter, PrometheusServer};
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::MeterProvider;

fn server() -> PrometheusServer {
    let meters = MeterProvider::builder().build();
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("jobs.done").increment(3);
    });
    let resource = Resource::builder()
        .without_env()
        .with_service_name("svc")
        .build();
    PrometheusServer::serve("127.0.0.1:0", PrometheusExporter::new(meters, resource)).unwrap()
}

/// Send `request` and read the whole response.
fn send(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn scrapes_get_the_format_they_ask_for() {
    let server = server();
    let text = send(
        server.local_addr(),
        b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n",
    );
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    assert!(
        text.contains("Content-Type: text/plain; version=0.0.4"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("jobs_done_total{") && l.ends_with(" 3")),
        "{text}"
    );
    assert!(text.contains("service_name=\"svc\""));

    let open = send(
        server.local_addr(),
        b"GET /metrics?x=1 HTTP/1.1\r\nAccept: application/openmetrics-text;version=1.0.0\r\n\r\n",
    );
    assert!(
        open.contains("Content-Type: application/openmetrics-text"),
        "{open}"
    );
    assert!(open.trim_end().ends_with("# EOF"));
}

#[test]
fn head_sends_no_body_and_other_requests_are_refused() {
    let server = server();
    let head = send(server.local_addr(), b"HEAD /metrics HTTP/1.1\r\n\r\n");
    assert!(head.starts_with("HTTP/1.1 200 OK"));
    assert!(head.ends_with("\r\n\r\n"), "no body: {head:?}");
    let missing = send(server.local_addr(), b"GET / HTTP/1.1\r\n\r\n");
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
    let post = send(server.local_addr(), b"POST /metrics HTTP/1.1\r\n\r\n");
    assert!(post.starts_with("HTTP/1.1 405"), "{post}");
    assert!(post.contains("Allow: GET, HEAD"));
}

#[test]
fn oversized_or_silent_requests_dont_block_the_endpoint() {
    let server = server();
    let mut huge = b"GET /metrics HTTP/1.1\r\n".to_vec();
    for i in 0..1000 {
        huge.extend_from_slice(format!("X-Filler-{i}: {}\r\n", "a".repeat(20)).as_bytes());
    }
    huge.extend_from_slice(b"\r\n");
    let refused = send(server.local_addr(), &huge);
    assert!(refused.starts_with("HTTP/1.1 431"), "{refused}");

    // A client that connects and says nothing is dropped after the read
    // timeout; meanwhile the next scrape waits, then succeeds.
    let _silent = TcpStream::connect(server.local_addr()).unwrap();
    let started = Instant::now();
    let text = send(server.local_addr(), b"GET /metrics HTTP/1.1\r\n\r\n");
    assert!(text.starts_with("HTTP/1.1 200"));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn dropping_the_server_stops_it() {
    let server = server();
    let addr = server.local_addr();
    drop(server);
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err());
}
