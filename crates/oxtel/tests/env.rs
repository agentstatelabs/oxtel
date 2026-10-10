//! `oxtel::init()` driven by real environment variables, each case in a
//! child process: the environment is process-wide, setting it is `unsafe`,
//! and `init` installs global state.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};

const CHILD: &str = "OXTEL_TEST_CHILD";

/// Run test `name` in a child process with `vars` set.
fn child(name: &str, vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
    for var in [
        "RUST_LOG",
        "OTEL_SDK_DISABLED",
        "OTEL_TRACES_EXPORTER",
        "OTEL_METRICS_EXPORTER",
        "OTEL_LOGS_EXPORTER",
        "OTEL_SERVICE_NAME",
        "OTEL_RESOURCE_ATTRIBUTES",
        "OXTEL_LOG_FORMAT",
        "OTEL_CONFIG_FILE",
    ] {
        command.env_remove(var);
    }
    for (k, v) in vars {
        command.env(k, v);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "child {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn in_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

fn scrape(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn the_environment_chooses_the_exporters() {
    if in_child() {
        let guard = oxtel::init().expect("install");
        let port = guard.prometheus_addr().expect("the endpoint runs").port();
        tracing::info_span!("request", otel.kind = "server").in_scope(|| {
            tracing::warn!(key = "items", "slow lookup");
            metrics::counter!("requests.handled").increment(2);
        });
        let response = scrape(port);
        assert!(response.contains("requests_handled_total"), "{response}");
        assert!(response.contains("service_name=\"env-test\""), "{response}");
        guard.shutdown().unwrap();
        return;
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let output = child(
        "the_environment_chooses_the_exporters",
        &[
            ("OTEL_TRACES_EXPORTER", "console,zipkin"),
            ("OTEL_LOGS_EXPORTER", "console"),
            ("OTEL_METRICS_EXPORTER", "prometheus"),
            ("OTEL_EXPORTER_PROMETHEUS_HOST", "127.0.0.1"),
            ("OTEL_EXPORTER_PROMETHEUS_PORT", &port),
            ("OTEL_SERVICE_NAME", "env-test"),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(" span \"request\" kind=server"),
        "the span on the console:\n{stdout}"
    );
    assert!(
        stdout.contains(" WARN env: slow lookup key=items trace_id="),
        "the event as a log record with its span's ids:\n{stdout}"
    );
    assert!(
        stdout.contains(" WARN oxtel: OTEL_TRACES_EXPORTER=zipkin isn't supported; ignored"),
        "the unsupported exporter reported through the pipeline:\n{stdout}"
    );
}

#[test]
fn otel_sdk_disabled_turns_everything_off() {
    if in_child() {
        let guard = oxtel::init().expect("install");
        assert!(guard.tracer_provider().is_none());
        assert!(guard.logger_provider().is_none());
        assert!(guard.meter_provider().is_none());
        tracing::info_span!("request").in_scope(|| tracing::warn!("nobody hears this"));
        metrics::counter!("requests.handled").increment(1);
        return;
    }
    let output = child(
        "otel_sdk_disabled_turns_everything_off",
        &[
            ("OTEL_SDK_DISABLED", "true"),
            ("OTEL_TRACES_EXPORTER", "console"),
            ("OTEL_LOGS_EXPORTER", "console"),
        ],
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("nobody hears this"));
}

#[test]
fn init_refuses_to_replace_another_subscriber() {
    if in_child() {
        tracing::subscriber::set_global_default(tracing_subscriber::registry()).unwrap();
        assert_eq!(
            oxtel::init().err(),
            Some(oxtel::Error::SubscriberAlreadySet)
        );
        return;
    }
    child(
        "init_refuses_to_replace_another_subscriber",
        &[
            ("OTEL_TRACES_EXPORTER", "none"),
            ("OTEL_METRICS_EXPORTER", "none"),
            ("OTEL_LOGS_EXPORTER", "none"),
        ],
    );
}

#[test]
fn local_output_survives_a_disabled_sdk_and_follows_oxtel_log_format() {
    if in_child() {
        // The program asks for text; the environment says JSON.
        let _guard = oxtel::builder()
            .with_local_logs(oxtel::sdk::logs::LocalFormat::Text)
            .install()
            .expect("install");
        tracing::warn!(key = "items", "still visible");
        return;
    }
    let output = child(
        "local_output_survives_a_disabled_sdk_and_follows_oxtel_log_format",
        &[("OTEL_SDK_DISABLED", "true"), ("OXTEL_LOG_FORMAT", "json")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(r#""level":"WARN","target":"env","message":"still visible","key":"items""#),
        "{stderr}"
    );
}

/// A configuration file in the temporary directory, unique to this run.
fn config_file(name: &str, text: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "oxtel-{name}-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn a_configuration_file_replaces_the_environment() {
    if in_child() {
        let guard = oxtel::init().expect("install");
        let port: u16 = std::env::var("PROMETHEUS_PORT").unwrap().parse().unwrap();
        assert_eq!(guard.prometheus_addr().map(|a| a.port()), Some(port));
        tracing::info_span!("request").in_scope(|| {
            tracing::warn!("slow lookup");
            metrics::counter!("requests.handled").increment(1);
        });
        let response = scrape(port);
        assert!(response.contains("requests_handled_total"), "{response}");
        assert!(
            response.contains("service_name=\"from-the-file\""),
            "{response}"
        );
        assert!(!response.contains("otel_scope_name"), "without_scope_info");
        guard.shutdown().unwrap();
        return;
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let path = config_file(
        "file",
        r#"
file_format: "1.2"
resource:
  attributes:
    - name: service.name
      value: ${CHECKOUT_NAME:-unnamed}
propagator:
  composite: [{tracecontext: }]
tracer_provider:
  sampler:
    always_on:
  processors:
    - simple:
        exporter:
          console:
logger_provider:
  processors:
    - simple:
        exporter:
          console:
meter_provider:
  views:
    - selector: {instrument_name: x}
  readers:
    - pull:
        exporter:
          prometheus/development:
            host: 127.0.0.1
            port: ${PROMETHEUS_PORT}
            without_scope_info: true
"#,
    );
    let output = child(
        "a_configuration_file_replaces_the_environment",
        &[
            ("OTEL_CONFIG_FILE", &path),
            ("CHECKOUT_NAME", "from-the-file"),
            ("PROMETHEUS_PORT", &port),
            // Ignored: the file is the configuration.
            ("OTEL_TRACES_EXPORTER", "none"),
            ("OTEL_LOGS_EXPORTER", "none"),
            ("OTEL_SERVICE_NAME", "from-the-environment"),
        ],
    );
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(" span \"request\""), "{stdout}");
    assert!(stdout.contains(" WARN env: slow lookup"), "{stdout}");
    assert!(
        stdout.contains("`meter_provider.views` isn't supported yet"),
        "the file's unsupported parts are reported: {stdout}"
    );
}

#[test]
fn a_configuration_file_that_cant_be_read_is_an_error() {
    if in_child() {
        let error = oxtel::init().err().expect("an error");
        let oxtel::Error::Config(message) = error else {
            panic!("{error:?}")
        };
        assert!(message.contains("file_format"), "{message}");
        return;
    }
    let path = config_file("bad", "file_format: \"2.0\"\n");
    child(
        "a_configuration_file_that_cant_be_read_is_an_error",
        &[("OTEL_CONFIG_FILE", &path)],
    );
    let _ = std::fs::remove_file(&path);
}
