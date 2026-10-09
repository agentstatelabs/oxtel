# OTLP protocol definitions

These `.proto` files are the OpenTelemetry Protocol (OTLP) definitions from
[open-telemetry/opentelemetry-proto](https://github.com/open-telemetry/opentelemetry-proto),
release **v1.11.1**, unmodified. They are licensed under the Apache License 2.0
(see [LICENSE](LICENSE)); copyright the OpenTelemetry Authors.

The Rust types in `../src/proto/gen/` are generated from them. To regenerate after
changing the pinned release:

```bash
cargo run -p xtask -- gen-proto
```
