# tauri-app-template

Standard Tauri v2 tray-app template with full CI/CD and security — the same
"clone, rename, build" starting point `python-backend-template`,
`node-backend-template`, and `go-http-template` provide for their own stacks.

## ✨ What's here

- A minimal, real, buildable Tauri v2 app: a system tray icon with a single
  "Quit" menu item and no windows. Not a placeholder for its own sake — the
  smallest thing that gives CI, branch protection, and release automation
  something real to act on from commit one.
- CI via [`ci-tauri.yml`](https://github.com/s3ntin3l8/.github/blob/main/.github/workflows/ci-tauri.yml)
  (fmt, clippy, tests, `cargo audit`, plus a 3-OS build-verification matrix).
- CodeQL is **not** wired up here — it has no Rust support. `cargo audit`
  (dependency vulnerability scanning) is the closest available substitute.
- Release automation via Release Please (`release-type: rust`), Dependabot
  (`github-actions` + `cargo` + `npm`), and the standard issue/PR templates.

## 🚀 Quick start

1. Use this template: `gh repo create <new-repo> --template
   s3ntin3l8/tauri-app-template --public`, or click **Use this template** on
   GitHub.
2. Rename the app: `productName`/`identifier` in `src-tauri/tauri.conf.json`,
   `name` in `package.json` and `src-tauri/Cargo.toml`, and this README.
3. Replace the placeholder icons in `src-tauri/icons/` with real branding
   (see `make help` — there's no icon-generation target here on purpose;
   `cargo tauri icon <path-to-source-image>` generates the full set).
4. `make build` (installs frontend deps, builds the frontend, then the app).

## 📁 Structure

```
src-tauri/       Rust backend (Cargo.toml, tauri.conf.json, src/, icons/)
index.html       Trivial static frontend — replace with a real one (React/Vite,
                 matching this org's other frontends) once there's an actual
                 window/UI to build. tauri.conf.json's "windows" array is
                 empty; this file exists only so frontendDist has something
                 real to embed.
```

## 🛠️ Commands

Run `make help` for the full list. The pre-push gate:

```sh
make lint && make test && make build
```

No `typecheck`/`format-check` targets the way the Node blueprint has — Rust
has neither concept the way tsc/prettier do; `cargo fmt --check` and `cargo
clippy` cover both roles here.

## 🛡️ Security

- `cargo audit` (dependency vulnerability scanning) runs in CI and via `make
  vulncheck` locally.
- `detect-secrets` runs as a pre-commit hook against `.secrets.baseline`.
- CodeQL is not available for Rust — see "What's here" above.

## 🙏 Credits

Follows the conventions established by
[`s3ntin3l8/.github`](https://github.com/s3ntin3l8/.github)'s reusable
workflows and blueprints.
