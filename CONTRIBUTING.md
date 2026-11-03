# Contributing

Thanks for helping. adash is small and boring on purpose: one Rust binary, plain JavaScript, no build step for the web UI.

## Development

```sh
cargo build
cargo test
adash demo        # UI on sample data, no agents needed (target/debug/adash demo)
```

- Rust: stable, edition 2021, MSRV 1.87. `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must pass.
- Web UI: edit `web/` directly; the files are embedded at build time. Check both `?fixture=1` and a live server, light and dark, desktop and 412 px wide, English (`?lang=en`) and Korean.
- Unit tests must not touch real user files: use temp directories (see `cfg!(test)` paths) and never start desktop notifications or send keystrokes.

## Harness formats

Session formats change. When you fix a parser for a new harness version, add a small synthetic sample to the test that covers it and update the tested versions in the README.

## Pull requests

- One topic per pull request, with a short description of the behavior change and how you checked it.
- No personal data in fixtures, tests or screenshots: invent project names, agent names and messages.
- New user-facing strings need both Korean and English (`web/i18n.js`).

By contributing you agree that your contribution is licensed under Apache-2.0.
