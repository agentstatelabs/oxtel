# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/agentstatelabs/oxtel/security/advisories/new) for this repository. Don't open a public issue.

We aim to acknowledge reports within three working days, and to agree a disclosure date with you once a fix is ready.

## Scope

oxtel parses input from outside the process: trace context headers, configuration files and OpAMP messages. Bugs that let that input crash, stall or exhaust the memory of a program using oxtel are in scope, as is any way telemetry could leak data that oxtel promises not to record.

## Supported versions

Until 1.0, only the latest release receives fixes.
