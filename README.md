# mullion-helper

Desktop tray app for the [Mullion SSH-agent bridge](https://github.com/s3ntin3l8/mullion-session-manager/blob/main/docs/ssh-agent.md).
While its tray icon is present it supervises a private bridge worker. Closing
the window hides it; **Quit** stops both the worker and app.

Generated from [`tauri-app-template`](https://github.com/s3ntin3l8/tauri-app-template) —
see that repo for the underlying tooling conventions (CI, blueprints, pre-commit).

## Install

- **macOS:** Apple Silicon, macOS 13.5+. Open the DMG and drag Mullion Helper
  to Applications.
- **Windows:** Windows 10/11 x64. Run the NSIS `setup.exe` installer.

Preview builds are ad-hoc signed on macOS and unsigned on Windows. Platform
identity signing and notarization are tracked separately; in-app updates are
still verified using their dedicated Tauri signature.

On first launch, create a payload in Mullion under **Settings → Hosts → SSH
agent bridges**, paste it into Mullion Helper, and choose **Pair and start**.
Launch-at-login is enabled after pairing. This window reports only the local
bridge; remote agent-host status remains in Mullion itself.

The app auto-detects `SSH_AUTH_SOCK`, 1Password's common macOS socket, and the
Windows OpenSSH-compatible pipe. Only identity-list and signature requests
reach the local agent; mutating and unknown SSH-agent operations fail closed.

## 🚀 Quick start

```sh
npm ci
npm run stage:sidecar
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

`npm run stage:sidecar` bundles the extracted Node worker as a Single
Executable Application and stages it under Tauri's target-triple sidecar
name. It is an implementation detail, not a supported CLI or download.

## Updates and migration

The app checks for signed whole-app updates after startup and every 24 hours;
manual checking is always available. Credentials stay outside the webview in
the app data directory with owner-only permissions.

First launch imports a valid credential from the legacy `mullion helper`
location. Only after the bundled worker validates the copy does the app
disable the old launchd job or Windows Run entry. The old binary is left
installed but inactive for rollback and later cleanup.

## 🛡️ Security

- `cargo audit` (dependency vulnerability scanning) runs in CI and via `make
  vulncheck` locally. No CodeQL — it has no Rust support.
- `detect-secrets` runs as a pre-commit hook against `.secrets.baseline`.

## License

AGPL-3.0. Copyright (C) 2026 Björn Hansen. See [LICENSE](LICENSE).
