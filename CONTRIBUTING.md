# Contributing to oxtel

Thank you for helping. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). oxtel is early, so the most useful contributions right now are issues: use cases, specification questions, and problems with the [design](DESIGN.md). To see how oxtel is meant to be used, start with [the documentation site](https://agentstatelabs.github.io/oxtel/).

## Sign-off (DCO)

Every commit must be signed off under the [Developer Certificate of Origin 1.1](https://developercertificate.org/), which states that you have the right to submit the change under the project's license. Sign off with:

```bash
git commit -s
```

This adds a `Signed-off-by:` line with your name and email. There is no contributor license agreement.

## Code you contribute

- Write it for this project. Don't copy code from other projects, including other OpenTelemetry SDKs, even when their license would allow it. This keeps the project's provenance clean.
- Follow the OpenTelemetry specification. Where oxtel deliberately differs from it, the code and docs say so.
- No `unsafe` code.
- Run `cargo fmt`, `cargo clippy --all-targets` and `cargo test` before opening a pull request.

## The documentation site

[The site](https://agentstatelabs.github.io/oxtel/) is built from `site/` with Astro and Starlight, and published whenever `main` changes. To preview a change to it:

```bash
cd site && npm ci && npm run dev
```

`npm run build`, then `node scripts/check-links.mjs`, checks that every link between pages leads somewhere, as CI does.

Some pages come from the code, and CI fails when they fall behind it:

- **Examples.** Pages show programs from `examples/`, and what some of them print, from `examples/output/`. When an example's output changes on purpose, `cargo run -p xtask -- check-output --bless` rewrites those files.
- **[Every setting](https://agentstatelabs.github.io/oxtel/reference/settings/)** is generated from the table in `xtask/src/settings.rs`. When the code reads a new environment variable, describe it there, then run `cargo run -p xtask -- gen-settings`.
- **[Every span and metric](https://agentstatelabs.github.io/oxtel/reference/telemetry/)** is generated from the names the crates' tests record, kept in `crates/*/tests/recorded/`. When a change renames, adds or drops one on purpose, run the tests with `OXTEL_REWRITE_RECORDED=1` to rewrite those files, then `cargo run -p xtask -- gen-telemetry`.

## Reporting security issues

See [SECURITY.md](SECURITY.md). Please don't open public issues for vulnerabilities.
