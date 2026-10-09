//! Development tasks. `cargo run -p xtask -- gen-proto` regenerates the OTLP
//! types from the vendored `.proto` files, with no `protoc` needed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const PROTOS: &[&str] = &[
    "opentelemetry/proto/common/v1/common.proto",
    "opentelemetry/proto/resource/v1/resource.proto",
    "opentelemetry/proto/trace/v1/trace.proto",
    "opentelemetry/proto/metrics/v1/metrics.proto",
    "opentelemetry/proto/logs/v1/logs.proto",
    "opentelemetry/proto/collector/trace/v1/trace_service.proto",
    "opentelemetry/proto/collector/metrics/v1/metrics_service.proto",
    "opentelemetry/proto/collector/logs/v1/logs_service.proto",
];

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("gen-proto") => match gen_proto() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("gen-proto failed: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo run -p xtask -- gen-proto");
            ExitCode::from(2)
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn gen_proto() -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let include = root.join("crates/oxtel-otlp/proto");
    let out = root.join("crates/oxtel-otlp/src/proto/gen");
    std::fs::create_dir_all(&out)?;
    let descriptors = protox::compile(PROTOS, [&include])?;
    prost_build::Config::new()
        .out_dir(&out)
        // Proto comments become rustdoc, and their indented examples would
        // run as doctests; the specification is the documentation.
        .disable_comments(["."])
        .compile_fds(descriptors)?;
    println!("generated into {}", out.display());
    Ok(())
}
