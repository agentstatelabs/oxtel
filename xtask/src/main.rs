//! Development tasks:
//!
//! - `cargo run -p xtask -- gen-proto` regenerates the OTLP types from the
//!   vendored `.proto` files, with no `protoc` needed.
//! - `cargo run -p xtask -- gen-semconv` regenerates `oxtel-semconv` with
//!   OpenTelemetry Weaver ([`WEAVER_VERSION`], found as `$WEAVER` or `weaver`
//!   on the path) from the pinned conventions releases.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The Weaver release the templates are written for.
const WEAVER_VERSION: &str = "0.26.1";
/// The semantic conventions release `oxtel_semconv::{attribute, metric}`
/// come from. Update `SCHEMA_URL` in `oxtel-semconv` with it.
const SEMCONV_RELEASE: &str = "v1.44.0";
/// The semantic-conventions-genai commit `oxtel_semconv::genai` comes from;
/// that repository has no releases yet.
const GENAI_COMMIT: &str = "6fd0d763a092db245a54104fc1124031eb3c51d2";

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
        Some("gen-semconv") => match gen_semconv() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("gen-semconv failed: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo run -p xtask -- gen-proto | gen-semconv");
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

fn gen_semconv() -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let crate_dir = root.join("crates/oxtel-semconv");
    let weaver = std::env::var("WEAVER").unwrap_or_else(|_| "weaver".into());
    let version = Command::new(&weaver)
        .arg("--version")
        .output()
        .map_err(|e| format!("run `{weaver}` (set WEAVER to its path): {e}"))?;
    let version = String::from_utf8_lossy(&version.stdout);
    if !version.contains(WEAVER_VERSION) {
        return Err(format!(
            "the templates are written for Weaver {WEAVER_VERSION}, not `{}`",
            version.trim()
        )
        .into());
    }
    let lib = std::fs::read_to_string(crate_dir.join("src/lib.rs"))?;
    let schema = format!("schemas/{}", SEMCONV_RELEASE.trim_start_matches('v'));
    if !lib.contains(&schema) {
        return Err(format!("update SCHEMA_URL in oxtel-semconv to {schema}").into());
    }
    let runs = [
        (
            format!(
                "https://github.com/open-telemetry/semantic-conventions/archive/refs/tags/{SEMCONV_RELEASE}.zip[model]"
            ),
            format!("open-telemetry/semantic-conventions {SEMCONV_RELEASE}"),
            "src/generated",
        ),
        (
            format!(
                "https://github.com/open-telemetry/semantic-conventions-genai/archive/{GENAI_COMMIT}.zip[model]"
            ),
            format!(
                "open-telemetry/semantic-conventions-genai at {}",
                &GENAI_COMMIT[..12]
            ),
            "src/generated/genai",
        ),
    ];
    for (registry, label, out) in &runs {
        let status = Command::new(&weaver)
            .current_dir(&crate_dir)
            .args(["registry", "generate", "--quiet", "--registry", registry])
            .args(["--templates", "templates", "--param"])
            .arg(format!("registry={label}"))
            .args(["rust", out])
            .status()?;
        if !status.success() {
            return Err(format!("weaver failed for {label}").into());
        }
        for file in ["attribute.rs", "metric.rs"] {
            let path = crate_dir.join(out).join(file);
            let status = Command::new("rustfmt")
                .args(["--edition", "2024"])
                .arg(&path)
                .status()?;
            if !status.success() {
                return Err(format!("rustfmt failed on {}", path.display()).into());
            }
        }
        println!("generated {label} into {}", crate_dir.join(out).display());
    }
    Ok(())
}
