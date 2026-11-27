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
//! - `cargo run -p xtask -- check-output` runs the examples that have a
//!   file in `examples/output/` and checks they still print what it shows,
//!   since the site shows those files as their output. `--bless` rewrites
//!   the files from a fresh run.

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
        Some("check-output") => {
            let bless = std::env::args().nth(2).as_deref() == Some("--bless");
            match check_output(bless) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("check-output failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: cargo run -p xtask -- gen-proto | gen-semconv | check-examples | check-output [--bless]"
            );
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
/// example and on the site) are known to be enough. Its tests are built
/// too, so a block's `[dev-dependencies]` are checked as well. The oxtel
/// crates come from this checkout, and a `path` dependency is found from
/// `examples/`, as `examples/Cargo.toml` finds it.
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
            let dependencies = with_paths_from(&dependencies, &root.join("examples"));
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
                .args(["check", "--quiet", "--all-targets"])
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

/// `block` with each `path = "…"` found from `base`.
fn with_paths_from(block: &str, base: &Path) -> String {
    const KEY: &str = "path = \"";
    let mut out = String::with_capacity(block.len());
    let mut rest = block;
    while let Some(at) = rest.find(KEY) {
        out.push_str(&rest[..at]);
        let after = &rest[at + KEY.len()..];
        let end = after.find('"').unwrap_or(after.len());
        let path = base.join(&after[..end]).display().to_string();
        out.push_str(&format!("path = {path:?}"));
        rest = after.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

fn dependency_names(block: &str) -> impl Iterator<Item = &str> {
    block
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty() && !name.starts_with('['))
}

/// Runs each example that has a file in `examples/output/` and compares
/// what it prints with the file. `examples/output/<example>.<format>` is
/// what it writes to standard error with `OXTEL_LOG_FORMAT=<format>`
/// (`text`, `json` or `ecs`) and nothing else set; `<example>.spans` is
/// what it writes to standard output with `OTEL_TRACES_EXPORTER=console`
/// and no log lines, its spans. Timestamps, trace and span ids and span
/// durations change on every run, so they're compared by their shape alone.
fn check_output(bless: bool) -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("examples/output"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    files.sort();
    let mut failed = Vec::new();
    for path in &files {
        let (Some(example), Some(format)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        if !matches!(format, "text" | "json" | "ecs" | "spans") {
            return Err(format!(
                "{}: the extension must be text, json, ecs or spans",
                path.display()
            )
            .into());
        }
        // Build, then run the program itself, so what it writes isn't mixed
        // with cargo's own output.
        let built = Command::new(&cargo)
            .current_dir(&root)
            .args(["build", "-q", "-p", "oxtel-examples", "--example", example])
            .args(["--message-format", "json-render-diagnostics"])
            .stderr(std::process::Stdio::inherit())
            .output()?;
        if !built.status.success() {
            return Err(format!("examples/{example}.rs doesn't build").into());
        }
        let executable = String::from_utf8(built.stdout)?
            .lines()
            .filter(|line| line.contains(r#""reason":"compiler-artifact""#))
            .find_map(|line| {
                let (_, rest) = line.split_once(r#""executable":""#)?;
                Some(rest[..rest.find('"')?].to_string())
            })
            .ok_or_else(|| format!("cargo didn't say where it built examples/{example}.rs"))?;
        let mut run = Command::new(executable);
        run.current_dir(&root);
        for (key, _) in std::env::vars() {
            if key.starts_with("OTEL_") || key.starts_with("OXTEL_") || key == "RUST_LOG" {
                run.env_remove(key);
            }
        }
        let spans = format == "spans";
        if spans {
            run.env("OXTEL_LOG_FORMAT", "none")
                .env("OTEL_TRACES_EXPORTER", "console");
        } else {
            run.env("OXTEL_LOG_FORMAT", format);
        }
        let output = run.output()?;
        if !output.status.success() {
            return Err(format!("examples/{example}.rs failed: {}", output.status).into());
        }
        let got = String::from_utf8(if spans { output.stdout } else { output.stderr })?;
        let shown = path.strip_prefix(&root).unwrap_or(path).display();
        if bless {
            std::fs::write(path, &got)?;
            println!("wrote {shown}");
        } else if shape(&got) == shape(&std::fs::read_to_string(path)?) {
            println!("{shown} matches what examples/{example}.rs prints");
        } else {
            eprintln!("{shown} doesn't match what examples/{example}.rs prints:\n{got}");
            failed.push(shown.to_string());
        }
    }
    if files.is_empty() {
        return Err("examples/output/ has no files".into());
    }
    if !failed.is_empty() {
        return Err(format!(
            "{} out of date; `cargo run -p xtask -- check-output --bless` rewrites them",
            failed.join(", ")
        )
        .into());
    }
    Ok(())
}

/// `text` with each timestamp (`2026-10-09T12:00:00.123456Z`) replaced by
/// `<time>`, each trace or span id (32 or 16 lowercase hex digits) by
/// `<id>`, and each span's duration (`duration=0.042ms`) by
/// `duration=<duration>`.
fn shape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let after_word = i > 0 && bytes[i - 1].is_ascii_alphanumeric();
        if !after_word {
            if let Some(len) = timestamp_len(&bytes[i..]) {
                out.push_str("<time>");
                i += len;
                continue;
            }
            if let Some(len) = duration_len(&bytes[i..]) {
                out.push_str("duration=<duration>");
                i += len;
                continue;
            }
            let hex = bytes[i..]
                .iter()
                .take_while(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
                .count();
            let ends_word = bytes.get(i + hex).is_some_and(u8::is_ascii_alphanumeric);
            if (hex == 16 || hex == 32) && !ends_word {
                out.push_str("<id>");
                i += hex;
                continue;
            }
        }
        let ch = text[i..]
            .chars()
            .next()
            .expect("i is on a character boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The length of the RFC 3339 UTC timestamp `bytes` starts with, if any.
fn timestamp_len(bytes: &[u8]) -> Option<usize> {
    const PATTERN: &[u8] = b"dddd-dd-ddTdd:dd:dd";
    if bytes.len() < PATTERN.len() {
        return None;
    }
    let fits = PATTERN.iter().zip(bytes).all(|(p, b)| match p {
        b'd' => b.is_ascii_digit(),
        p => p == b,
    });
    if !fits {
        return None;
    }
    let mut len = PATTERN.len();
    if bytes.get(len) == Some(&b'.') {
        len += 1 + bytes[len + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
    }
    (bytes.get(len) == Some(&b'Z')).then_some(len + 1)
}

/// The length of the console exporter's `duration=<milliseconds>ms` that
/// `bytes` starts with, if any.
fn duration_len(bytes: &[u8]) -> Option<usize> {
    let rest = bytes.strip_prefix(b"duration=")?;
    let number = rest
        .iter()
        .take_while(|b| b.is_ascii_digit() || **b == b'.')
        .count();
    let len = b"duration=".len() + number;
    (number > 0 && bytes[len..].starts_with(b"ms")).then_some(len + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_hides_what_changes_between_runs() {
        assert_eq!(
            shape(
                r#"{"timestamp":"2026-11-20T20:24:56.619735Z","trace_id":"7ecfce50f920491ee1086c2912891261","span_id":"855293a13f688a21","order.id":7}"#
            ),
            r#"{"timestamp":"<time>","trace_id":"<id>","span_id":"<id>","order.id":7}"#
        );
        assert_eq!(
            shape("2026-11-20T20:25:17Z  WARN request: retrying span_id=1be284c0a8537bcc"),
            "<time>  WARN request: retrying span_id=<id>"
        );
        // Not ids: too short, too long, or part of a longer word.
        assert_eq!(
            shape("deadbeef x1be284c0a8537bcc 1be284c0a8537bcc0"),
            "deadbeef x1be284c0a8537bcc 1be284c0a8537bcc0"
        );
        // A span from the console exporter.
        assert_eq!(
            shape(
                r#"2026-11-20T20:25:17.000001Z span "SELECT orders" kind=client status=unset duration=0.042ms trace_id=7ecfce50f920491ee1086c2912891261"#
            ),
            r#"<time> span "SELECT orders" kind=client status=unset duration=<duration> trace_id=<id>"#
        );
        // Not a span's duration: a field of the program's own.
        assert_eq!(
            shape("job.duration=3 duration=fast"),
            "job.duration=3 duration=fast"
        );
    }

    #[test]
    fn paths_are_found_from_the_examples() {
        let base = Path::new("/repo/examples");
        assert_eq!(
            with_paths_from(
                "[dependencies]\nshipping = { path = \"shipping\" }\noxtel = \"0.3\"\n",
                base
            ),
            "[dependencies]\nshipping = { path = \"/repo/examples/shipping\" }\noxtel = \"0.3\"\n"
        );
    }
}
