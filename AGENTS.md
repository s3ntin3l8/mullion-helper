# Agent briefing

The short, load-bearing version of this repo's workflow rules. See `CLAUDE.md`
for the fuller picture (layout, conventions).

<!-- briefing:start -->
- **Work in a worktree.** Developer worktrees live under `.wt/`, e.g.
  `.wt/<slug>`. Create one with `make wt name=<slug>` (or `git fetch origin &&
  git worktree add .wt/<slug> -b <slug> origin/main`). A fresh worktree does
  **not** inherit `node_modules` or Rust's `target/` — run `npm ci` in the
  worktree before building.
- **Never commit directly to `main`.** Branch protection has no bypass.
  Always branch off the latest `origin/main` and open a PR.
- **PR title needs a Conventional Commits prefix** (`feat:`, `fix:`, `chore:`,
  ...). This repo squash-merges, so the PR title becomes the commit message on
  `main` — an unprefixed title silently drops out of the changelog.
- **Before pushing, run the full gate:** `make lint && make test && make
  build`. No `typecheck`/`format-check` targets — `cargo fmt --check`/`cargo
  clippy` cover both roles for Rust.
- **Get a review, and close the loop on it.** Request a Hermes review
  (`@s3ntin3l8-hermes Review` on the PR, or `make review`). Fixing the code is
  not enough to address feedback — reply to each inline comment via the
  GitHub API, then resolve the thread via the GraphQL `resolveReviewThread`
  mutation.
- **Run a review pass on your own diff before declaring done.**
- **File a GitHub issue for anything a plan defers, blocks, or descopes.**
  Before implementing, open one issue per item
  ([Issue Blueprint](.github/ISSUE_TEMPLATE/issue-blueprint.md) format) — a
  footnote in a plan doc is not a durable record.
- **Post-merge:** delete the local and remote branch, and `git worktree
  remove <path>`.
<!-- briefing:end -->
