//! Development tasks:
//!
//! - `cargo run -p xtask -- gen-proto` regenerates the OTLP types from the
//!   vendored `.proto` files, with no `protoc` needed.
//! - `cargo run -p xtask -- gen-semconv` regenerates `oxtel-semconv` with
//!   OpenTelemetry Weaver ([`WEAVER_VERSION`], found as `$WEAVER` or `weaver`
//!   on the path) from the pinned conventions releases.
//! - `cargo run -p xtask -- check-examples` builds each example that lists
//!   its dependencies as a program of its own, with only those
//!   dependencies.

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
        Some("check-examples") => match check_examples() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("check-examples failed: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo run -p xtask -- gen-proto | gen-semconv | check-examples");
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

/// Builds each example whose documentation has ```` ```toml ```` blocks of
/// `[dependencies]` as a program of its own, once for each block, with
/// exactly those dependencies, so the lines shown to readers (in the
/// example and on the site) are known to be enough. The oxtel crates come
/// from this checkout.
fn check_examples() -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let work = root.join("target/example-check");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut examples: Vec<PathBuf> = std::fs::read_dir(root.join("examples"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    examples.sort();
    let mut checked = 0;
    for path in examples {
        let source = std::fs::read_to_string(&path)?;
        let example = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("example");
        for (i, dependencies) in documented_dependencies(&source).into_iter().enumerate() {
            // The first list builds as `logs`, a second as `logs-2`, and so on.
            let name = match i {
                0 => example.to_string(),
                _ => format!("{example}-{}", i + 1),
            };
            let dir = work.join(&name);
            std::fs::create_dir_all(dir.join("src"))?;
            let mut manifest = format!(
                "[package]\nname = \"example-{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n{dependencies}\n[patch.crates-io]\n"
            );
            for krate in dependency_names(&dependencies).filter(|n| n.starts_with("oxtel")) {
                let local = root.join("crates").join(krate);
                if !local.is_dir() {
                    return Err(format!("{}: no crate {krate} in crates/", path.display()).into());
                }
                manifest.push_str(&format!(
                    "{krate} = {{ path = {:?} }}\n",
                    local.display().to_string()
                ));
            }
            std::fs::write(dir.join("Cargo.toml"), manifest)?;
            std::fs::write(dir.join("src/main.rs"), &source)?;
            // Start from the workspace's versions rather than the newest.
            std::fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock"))?;
            println!(
                "checking examples/{example}.rs with dependency list {}",
                i + 1
            );
            let status = Command::new(&cargo)
                .current_dir(&dir)
                .args(["check", "--quiet"])
                .env("CARGO_TARGET_DIR", work.join("target"))
                .status()?;
            if !status.success() {
                let list = i + 1;
                return Err(format!(
                    "examples/{example}.rs doesn't build with its dependency list {list}"
                )
                .into());
            }
            checked += 1;
        }
    }
    if checked == 0 {
        return Err("no example documents its dependencies".into());
    }
    println!("{checked} documented dependency lists are enough to build their examples");
    Ok(())
}

/// The `[dependencies]` blocks in a file's `//!` documentation.
fn documented_dependencies(source: &str) -> Vec<String> {
    let doc = source
        .lines()
        .map_while(|line| line.strip_prefix("//!"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line));
    let mut blocks = Vec::new();
    let mut block = None::<String>;
    for line in doc {
        match block.take() {
            None if line.trim() == "```toml" => block = Some(String::new()),
            None => {}
            Some(text) if line.trim() == "```" => blocks.push(text),
            Some(mut text) => {
                text.push_str(line);
                text.push('\n');
                block = Some(text);
            }
        }
    }
    blocks.retain(|text| text.trim_start().starts_with("[dependencies]"));
    blocks
}

fn dependency_names(block: &str) -> impl Iterator<Item = &str> {
    block
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty() && !name.starts_with('['))
}
