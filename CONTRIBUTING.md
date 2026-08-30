# Contributing

## Setup

```sh
npm ci
npm run build
cd src-tauri && cargo build
```

Requires a stable Rust toolchain and Tauri's platform build dependencies
(on Linux: `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`,
`patchelf`, `build-essential` — see the
[Tauri prerequisites docs](https://v2.tauri.app/start/prerequisites/) for
macOS/Windows).

## Before opening a PR

```sh
make lint && make test && make build
```

## PR title

Use a [Conventional Commits](https://www.conventionalcommits.org/) prefix
(`feat:`, `fix:`, `chore:`, ...). This repo squash-merges PRs and Release
Please parses the PR title, not individual commits — an unprefixed title
silently drops out of the changelog.

## Branch protection

`main` requires a PR (no direct pushes, no admin bypass) and these status
checks: `ci-tauri / lint-and-test`, `dependency-review / dependency-review`.

## Automated review

Request a Hermes review with `make review`, or `@s3ntin3l8-hermes Review` on
the PR directly.
