# CLAUDE.md — tauri-app-template

Read `AGENTS.md` first — it's the short, load-bearing version of this repo's
workflow rules. This file has the fuller picture.

## Commands (Makefile)

Run `make help` for the full list. Key targets: `lint` (`cargo fmt --check` +
`cargo clippy`), `test` (`cargo test --all-features`), `build` (npm build,
then `cargo build --release`), `dev` (`cargo tauri dev`), `vulncheck` (`cargo
audit`).

## Layout

- `src-tauri/` — the Rust backend. `Cargo.toml` (crate `app`, lib name
  `app_lib`), `tauri.conf.json` (v2 schema), `src/main.rs` + `src/lib.rs`
  (the actual app — a tray icon, one "Quit" menu item, no windows),
  `icons/` (placeholder — replace via `cargo tauri icon <source-image>`).
- `index.html` / `package.json` — a trivial static frontend whose only job
  today is giving `tauri.conf.json`'s `frontendDist` something real to embed
  at build time (`tauri::generate_context!()` reads it at compile time, so
  both `cargo build` and `cargo test` need it built first — see the Makefile's
  own step ordering). Replace with a real frontend once there's an actual
  window/UI to build.

## CI/CD — uses a centralized reusable workflow

`.github/workflows/ci-cd.yml` calls
`s3ntin3l8/.github/.github/workflows/ci-tauri.yml@main` — same "thin caller"
convention every other repo in this org uses for its reusable workflows. See
that workflow's own docs (`s3ntin3l8/.github`'s README) for its full input
surface. Two jobs: `lint-and-test` (fmt/clippy/test/audit, `ubuntu-latest`)
and `build` (a 3-OS compile-verification matrix — `ubuntu-latest`,
`windows-latest`, `macos-latest` — since Tauri, unlike Go, cannot
cross-compile a GUI app from one host; each target OS's webview linkage is
genuinely different). The `build` matrix is **not** a required branch
protection check yet — it needs a stretch of real runs to prove non-flaky
first, same reasoning `mullion-session-manager` applied to its own
`test-windows`/`test-macos` jobs.

**No CodeQL.** CodeQL has no Rust language support at all (Go, Python,
JS/TS, C/C++, C#, Java/Kotlin, Swift — not Rust). `cargo audit` (dependency
vulnerability scanning, wired into `ci-tauri.yml`) is the closest available
substitute — it is not the same kind of coverage.

## Git workflow

Same as every other repo in this org: branch off `origin/main`, PR required,
Conventional Commits PR title (Release Please parses the title, this repo
squash-merges), full gate before pushing, Hermes review + reply/resolve
every thread before merge.

## Conventions

- Rust: `cargo fmt`/`cargo clippy -- -D warnings` are the formatting and lint
  gates — there's no separate typecheck step the way TypeScript has one.
- The frontend stays deliberately minimal (plain HTML, no framework) until a
  real window/UI exists — don't add a React/Vite toolchain speculatively.
- `working-directory: src-tauri` is `ci-tauri.yml`'s default assumption for
  where `Cargo.toml` lives — keep it there rather than restructuring the repo
  layout, or override the input explicitly if you do.
