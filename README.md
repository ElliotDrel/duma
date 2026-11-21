# Duma

Open-source voice typing for Mac and Windows. Hold a key, speak, release: polished text is pasted where your cursor is. *Duma* is Russian for "thought".

v1 does three things: **dictate**, **spell check** a selection, and **paste last** result from any of your computers. See [docs/architecture.md](docs/architecture.md) for how it works and [future issues](https://github.com/ElliotDrel/duma/issues?q=label%3Afuture) for what comes next.

## Install

Download the latest installer from [Releases](https://github.com/ElliotDrel/duma/releases/latest) (`.dmg` for Mac, `-setup.exe` for Windows). Duma updates itself.

## Develop

Requirements: Rust (stable), Node 26, pnpm.

```sh
cd app && pnpm install && pnpm tauri dev   # run the app (dev channel, separate data folder)
cargo test --workspace                     # tests
cargo clippy --workspace --all-targets -- -D warnings
```

Put `OPENAI_API_KEY=...` in a `.env` at the repo root for local runs (gitignored; debug builds only).

Latency bench (needs the key): `cargo run -p duma-core --example bench -- <24kHz-mono-16bit.wav> [runs]`

## Release

Bump `version` in the root `Cargo.toml`, commit, then tag and push: `git tag v0.2.0 && git push origin v0.2.0`. CI builds the installers, publishes the release, and installed apps update on next launch.

## License

[MIT](LICENSE)
