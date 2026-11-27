---
title: Sending to a collector
description: OTLP over HTTP or gRPC, headers and TLS with your own certificates, and OTLP/JSON lines to standard output or a file.
---

Set `OTEL_EXPORTER_OTLP_ENDPOINT`, and oxtel sends traces, metrics and logs there with OTLP, the OpenTelemetry protocol, as [Get started](/start/) shows. This page covers the choices around that: which protocol, the headers and certificates a receiver may ask for, and writing OTLP to standard output or a file when nothing receives it over the network.

Each setting here applies to every kind of telemetry. Each also has a variable for one kind, with `TRACES_`, `METRICS_` or `LOGS_` after `OTEL_EXPORTER_OTLP_`, such as `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. That one takes precedence.

## HTTP or gRPC

OTLP runs over HTTP or over gRPC. Both carry the same data, and the OpenTelemetry Collector accepts both. Use what your collector or backend listens for.

| `OTEL_EXPORTER_OTLP_PROTOCOL` | Usual port | |
|---|---|---|
| `http/protobuf` | 4318 | The default. Protobuf bodies over HTTP. |
| `http/json` | 4318 | JSON bodies, for receivers that ask for them. |
| `grpc` | 4317 | Needs oxtel's `grpc` feature. |

For gRPC, turn on the feature where you add oxtel, `oxtel = { version = "0.4", features = ["grpc"] }`, and set `OTEL_EXPORTER_OTLP_PROTOCOL=grpc`.

- **The endpoint is used as it is,** such as `http://collector:4317`, with no path added. With nothing set, it's `http://localhost:4317`.
- **It works in any program.** gRPC uses tonic's HTTP/2 client, on a small tokio runtime of its own, so a program doesn't need to be async to use it.
- **Failures read the same.** A gRPC `UNAUTHENTICATED` is reported as HTTP's status 401 would be, with the hint to check the credentials.
- **The feature is needed.** Without it, `grpc` falls back to `http/protobuf`, with a warning at startup.

Pointing OTLP over HTTP at port 4317 is a common mistake, and oxtel warns about it at startup.

## Paths

Over HTTP, oxtel adds `/v1/traces`, `/v1/metrics` and `/v1/logs` to the general endpoint. A per-signal endpoint, such as `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, is used exactly as given, so it needs the full path. The startup line shows each URL in full.

## Headers, compression and timeouts

- **Headers:** `OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <token>`, for a backend that asks for credentials. Separate several headers with commas. Over gRPC they're sent as metadata.
- **Compression:** `OTEL_EXPORTER_OTLP_COMPRESSION=gzip`.
- **Timeout:** `OTEL_EXPORTER_OTLP_TIMEOUT`, in milliseconds, for each export.

## TLS with your own certificates

An `https://` endpoint is verified against the root certificates bundled with oxtel, as a browser verifies a web site. Three settings change that, each naming a PEM file:

| Variable | What it does |
|---|---|
| `OTEL_EXPORTER_OTLP_CERTIFICATE` | The certificates to verify the receiver with, in place of the bundled roots: for a collector whose certificate your own authority signed. |
| `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` | A certificate oxtel presents to the receiver, for mutual TLS. |
| `OTEL_EXPORTER_OTLP_CLIENT_KEY` | That certificate's private key. |

They work over HTTP and gRPC alike. A file that can't be read, or a certificate without its key, is a warning at startup that names the file. That kind of telemetry then isn't sent: oxtel won't fall back to sending it unverified.

In a configuration file, an exporter's `tls` section, with `ca_file`, `cert_file` and `key_file`, does the same.

## OTLP/JSON lines, to standard output or a file

Sometimes nothing receives telemetry over the network: a batch job whose files are collected afterwards, or a container platform that only gathers standard output. oxtel can write OTLP there instead. Each export becomes one line of OTLP/JSON, the encoding OTLP over HTTP uses, so the Collector's `otlpjsonfile` receiver reads the lines back without loss. CI checks that it does, for traces, metrics and logs alike.

- **In a configuration file**, use the `otlp_file/development` exporter. Its `output_stream` is `stdout`, the default, or `file://` and a path, such as `file:///var/log/app/traces.jsonl`.
- **In code**, use `oxtel_otlp::OtlpFileExporter::stdout()` or `OtlpFileExporter::file(path)`, for any pipeline; its [documentation](https://docs.rs/oxtel-otlp/latest/oxtel_otlp/struct.OtlpFileExporter.html) has an example.
- **Give each kind of telemetry a file of its own:** the receiver reads a file as one kind. Exporters that name the same stream share it, so their lines never interleave.
- **The startup line names it,** such as `traces: /var/log/app/traces.jsonl (OTLP/JSON lines)`. A file that can't be opened is a warning, and that exporter is left out.
- **Keep log lines apart.** On standard output, oxtel's own log lines would mix with the OTLP. They go to standard error unless `OXTEL_LOG_OUTPUT` says otherwise, so leave it at that.
