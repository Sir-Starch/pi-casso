# Contributing to pi-casso

Thanks for your interest! Here's how to contribute.

## Getting Started

1. Fork the repo and clone your fork
2. Make sure you have Rust ≥ 1.85 installed (`rustup update stable`)
3. Build: `cargo build`
4. Run tests: `cargo test --workspace --all-targets --all-features`
5. Run clippy: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
6. Check formatting: `cargo fmt --all -- --check`

## Making Changes

- Create a feature branch from `main`
- Keep commits focused and atomic
- Run `cargo fmt` before committing
- Make sure `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- Add tests for new functionality where reasonable

## Pull Requests

- Open a PR against `main`
- Describe what changed and why
- Link related issues if any
- CI must pass (fmt, clippy, test, build)

## Reporting Issues

- Use [GitHub Issues](https://github.com/Sir-Starch/pi-casso/issues)
- Include your OS, Rust version, and steps to reproduce

## License

By contributing, you agree that your contributions will be dual-licensed
under **MIT OR Apache-2.0**, as described in [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE), without any additional terms or conditions.
