//! Declarative configuration files (`OTEL_CONFIG_FILE`): YAML with
//! environment substitution. Never panics, whatever the file or the
//! environment holds.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Variables whose values look like YAML structure and substitutions.
    let lookup = |name: &str| match name {
        "A" => Some("{a: [1, 2]}".to_string()),
        "B" => Some("${A}".to_string()),
        "C" => Some("true".to_string()),
        _ => None,
    };
    let _ = oxtel_config::parse_with(text, &lookup);
});
