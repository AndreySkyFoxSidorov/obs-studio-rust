# Contributing to SkyLive

Thank you for your interest in this experiment. Issues and pull requests are welcome, subject to maintainer review and the [Code of Conduct](CODE_OF_CONDUCT.md). SkyLive is independent of OBS Studio; contributions to OBS itself must follow the OBS project's own rules.

## Before opening a pull request

- Describe the problem, the intended behavior, and any platform or device requirements. For bugs, include reproducible steps and relevant versions; remove personal information, credentials, and stream keys from reports.
- Keep changes focused and include appropriate tests for behavior changes. Explain limitations and which checks or device tests you performed.
- Review all code and text you submit, including AI-assisted material. You are responsible for its correctness, security, provenance, and license compatibility. Do not submit material you lack the right to contribute; preserve upstream copyright and license notices.
- Use English for source comments, non-translated documentation, and commit messages. Localized application text is welcome; see [localization/README.md](localization/README.md).

## Development checks

Use Rust 1.88.0 and the platform dependencies in [the workflow](.github/workflows/check.yml). Relevant checks include:

```sh
cargo fmt --all -- --check
cargo xtask layers
cargo check --workspace --all-targets --locked
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

These checks do not replace native device and application testing. The workflow treats warnings as errors; describe any existing failures you encounter. Some research tools need local reference material intentionally excluded from Git, as described in the README.

## Commits and licensing

Write commit subjects and descriptions in English. Prefix the subject and each change-description line with `[*]` for changes, `[+]` for additions, or `[-]` for removals. Use the marker for the main change in the subject and separate markers in the body for mixed changes. Keep commits coherent and explain why when helpful.

Contributions must be compatible with [GPL-2.0-or-later](LICENSE) and any applicable third-party terms. Preserve attribution, notices, and modification information when adapting upstream code or resources. Identify upstream sources and licenses in the pull request. Do not assume that AI-generated output is free of third-party obligations.
