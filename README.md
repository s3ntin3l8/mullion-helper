# mullion-helper

Native tray app supervising the [SSH-agent bridge](https://github.com/s3ntin3l8/mullion-session-manager/blob/main/docs/ssh-agent.md)
helper (`mullion-session-manager`'s `mullion helper` Node SEA sidecar).

Generated from [`tauri-app-template`](https://github.com/s3ntin3l8/tauri-app-template) —
see that repo for the underlying tooling conventions (CI, blueprints, pre-commit).

## ✨ What's here today

A minimal, real, buildable Tauri v2 skeleton: a system tray icon with a
single "Quit" menu item and no windows. Everything else — supervising the
Node SEA sidecar, a pairing-payload wizard, a live host-list UI,
auto-update, code signing — is tracked as issues in this repo, not
implemented yet. See the repo's Issues tab for the full list of next steps.

## 📚 Background

`mullion helper run` (in `mullion-session-manager`) already does the real
work: forwarding this laptop's SSH agent to enrolled Mullion agent hosts
through a bridge relay, with a real sign-only filter enforced on both ends.
It emits structured `--json-events` NDJSON for connection lifecycle state.
This app's job is to supervise that process and give it a real UI instead
of a terminal — not to reimplement the bridge protocol itself.

## 🚀 Quick start

```sh
npm ci
npm run build
cd src-tauri && cargo build
```

Requires a stable Rust toolchain and Tauri's platform build dependencies —
see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## 🛠️ Commands

Run `make help` for the full list. The pre-push gate:

```sh
make lint && make test && make build
```

## 🛡️ Security

- `cargo audit` (dependency vulnerability scanning) runs in CI and via `make
  vulncheck` locally. No CodeQL — it has no Rust support.
- `detect-secrets` runs as a pre-commit hook against `.secrets.baseline`.

## License

AGPL-3.0. Copyright (C) 2026 Björn Hansen. See [LICENSE](LICENSE).
