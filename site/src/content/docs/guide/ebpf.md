---
title: "Telemetry without changing code: eBPF"
description: How eBPF instrumentation such as OpenTelemetry's OBI works, what it can't see, and how it complements oxtel.
---

Languages such as Java and Python have agents that add telemetry to a program while it runs, without changing its code. Rust compiles to native code with no runtime to hook into, so there's no agent like that for Rust. Two things come close.

The first is already in your program. Many Rust crates record what they do with `tracing`, and oxtel picks that up as it is. Ready-made layers such as `oxtel-tower` and `oxtel-rmcp` add the standard spans and metrics for a framework in one line.

The second works from outside the program: eBPF.

## How eBPF instrumentation works

eBPF lets small, verified programs run inside the Linux kernel. OpenTelemetry's eBPF Instrumentation, [OBI](https://opentelemetry.io/docs/zero-code/obi/), loads programs like these and attaches them in two kinds of places:

- **The kernel's network functions**, where it sees the bytes each process sends and receives.
- **Functions inside programs and their TLS libraries**, where it can see data before it's encrypted.

From those bytes it recognises protocols, such as HTTP, HTTP/2, gRPC, PostgreSQL, MySQL, Redis and Kafka, and times each request and its response. That gives spans for requests in and out, metrics for request rate, errors and duration, and network metrics, for every process on the machine, in any language, without touching its code or restarting it.

It needs:

- Linux 5.8 or later with BTF, or a RHEL-family 4.18 kernel with the needed backports;
- amd64 or arm64;
- root, or the Linux capabilities its features use.

It runs as a process on the host, in a container, or on each Kubernetes node.

## What it can't see

OBI sees a program from the outside, at the edges where requests arrive and leave. It can't see:

- **Work inside the program** that isn't a network request: a commit, a cache lookup, a policy decision.
- **What a request means**: which user, which order, which tool an MCP client called, unless the protocol carries it in a form OBI decodes.
- **Your log lines**, beyond adding trace context to them.
- **TLS it doesn't understand.** Many Rust services use rustls, which, unlike OpenSSL, doesn't own the socket: the program moves the encrypted bytes itself. That makes rustls hard for eBPF tools to read; check OBI's documentation for what it supports.

OBI's documentation says the same: it doesn't replace instrumentation inside the program for custom spans, application attributes and business events.

## Using both

They complement each other:

- **OBI for breadth.** Every service on a machine, including ones you can't change, gets request metrics and a place on the service map.
- **oxtel for depth.** Inside your Rust services: spans for the work that matters, the fields you choose, log lines that carry trace ids, and the MCP and GenAI conventions.
- **One collector.** Both send OTLP and use W3C Trace Context, so where OBI carries trace context between services, its spans and oxtel's join into one trace.
- **Don't trace the same server twice.** If `oxtel-tower` already traces a service's HTTP requests and OBI does too, each request appears twice. OBI's discovery settings choose which processes it instruments: leave out the services oxtel covers, or keep only one of the two for them.
