# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/agentstatelabs/oxtel/security/advisories/new) for this repository. Don't open a public issue.

We aim to acknowledge reports within three working days, and to agree a disclosure date with you once a fix is ready.

## Scope

oxtel parses input from outside the process: trace context and baggage, from HTTP headers and MCP requests; scrapes of its built-in `/metrics` endpoint; replies from the receivers it sends telemetry to; `OTEL_*` environment variables; and configuration files. OpAMP messages will join them when that feature lands. Bugs that let that input crash, stall or exhaust the memory of a program using oxtel are in scope.

So is any way telemetry could leak data that oxtel promises not to record. [Libraries oxtel instruments](https://agentstatelabs.github.io/oxtel/guide/libraries/) and [Databases](https://agentstatelabs.github.io/oxtel/guide/databases/) say what each instrumentation leaves out, such as query strings, the values in SQL statements, and prompts. [Every span and metric](https://agentstatelabs.github.io/oxtel/reference/telemetry/) lists every attribute and metric they record.

## Supported versions

Until 1.0, only the latest release receives fixes.
