//! A minimal `/metrics` endpoint on its own thread, for programs without an
//! HTTP server of their own. It needs only the standard library.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::PrometheusExporter;

/// The most a request's head may take: request line and headers.
const MAX_HEAD: u64 = 8 * 1024;
/// How long a client gets to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How much of a refused request is read and discarded before closing, so
/// the client receives the refusal instead of a connection reset.
const MAX_DRAIN: u64 = 1024 * 1024;

/// Serves `GET /metrics` from a [`PrometheusExporter`], negotiating the
/// format from each scrape's `Accept` header. Connections are handled one
/// at a time and closed after each response. Stops when dropped.
pub struct PrometheusServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl PrometheusServer {
    /// Listen on `addr`, such as `localhost:9464`.
    pub fn serve(addr: impl ToSocketAddrs, exporter: PrometheusExporter) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("oxtel-prometheus".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if worker_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Ok(stream) = stream {
                        let _ = respond(stream, &exporter);
                    }
                }
            })?;
        Ok(PrometheusServer {
            addr,
            stop,
            worker: Some(worker),
        })
    }

    /// The address it listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for PrometheusServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn respond(stream: TcpStream, exporter: &PrometheusExporter) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new((&stream).take(MAX_HEAD));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut accept = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            // The head was cut off: too long, or the client gave up.
            reply(&stream, 431, "text/plain", "", false)?;
            return linger(&stream);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("accept")
        {
            accept = Some(value.trim().to_string());
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = target.split(['?', '#']).next().unwrap_or("");
    if path != "/metrics" {
        return reply(&stream, 404, "text/plain", "not found\n", true);
    }
    match method {
        "GET" | "HEAD" => {
            let (content_type, body) = exporter.render(accept.as_deref());
            reply(&stream, 200, content_type, &body, method == "GET")
        }
        _ => reply(&stream, 405, "text/plain", "method not allowed\n", true),
    }
}

fn reply(
    mut stream: &TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
    send_body: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Request Header Fields Too Large",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if status == 405 {
        response.push_str("Allow: GET, HEAD\r\n");
    }
    response.push_str("\r\n");
    if send_body {
        response.push_str(body);
    }
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Close gracefully after refusing a request that wasn't read in full: stop
/// sending, then discard what the client is still sending, within limits.
fn linger(stream: &TcpStream) -> io::Result<()> {
    stream.shutdown(Shutdown::Write)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    io::copy(&mut stream.take(MAX_DRAIN), &mut io::sink())?;
    Ok(())
}
