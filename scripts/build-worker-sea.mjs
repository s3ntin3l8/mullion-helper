#!/usr/bin/env node
// Build the app's private bridge worker as a Node Single Executable
// Application. Tauri bundles the result as a sidecar; it is not a public CLI.
//
// Node SEA is still explicitly experimental upstream, and `postject`
// (https://www.npmjs.com/package/postject) is still `1.0.0-alpha.6` as of
// this PR — both are the currently-recommended tools for this regardless,
// there's no stable alternative.
//
// Bundled to CJS, not ESM, even though Node 26's `--experimental-sea-config`
// itself accepts `"mainFormat": "module"`. Deliberately NOT that: with an
// ESM main, `import.meta.url` inside the bundle resolves to
// `pathToFileURL(process.execPath)` — a PLAUSIBLE-LOOKING but wrong value,
// since nothing inside a SEA has "a directory it lives in" the way a real
// file does — so a future accidental `import.meta` reference would silently
// resolve to a bogus path instead of failing anywhere. CJS output makes
// `import.meta` empty instead, and `--log-override:empty-import-meta=error`
// (below) promotes that to a hard BUILD failure — this is exactly the
// mechanism that caught invalid path assumptions in the legacy bundle.
import { build as esbuildBuild } from "esbuild";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(here, "..");
const buildDir = path.join(repoRoot, "build", "worker-sea");
const entryPoint = path.join(repoRoot, "src", "worker", "main.mjs");

// Aligned with package.json's engines.node (>=26) — single source of truth
// for the minimum Node version this bundle targets. Sourced from
// package.json rather than hardcoded so the two can't drift apart: if
// engines.node is ever bumped, esbuild's target follows automatically.
// Issue #1058: a hand-typed `target` here was a maintenance footgun — a
// future contributor who adds a Node 23+ API call would otherwise get a
// silent transpile instead of a build failure.
//
// The parser above intentionally only handles the `>=X` shape (one regex
// strips the prefix). A more permissive parser (ranges like `^X || ^Y`,
// OR-ed version specifiers, etc.) would be unnecessary complexity:
// engines.node is a single value the package itself controls, and an
// invalid target — empty string, garbage, a range form esbuild can't
// lower to a target — fails the build loudly inside esbuild with a clear
// error pointing right at this line, which is a much better signal than
// silently accepting whatever this parser happened to produce.
const pkg = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"));
const enginesNode = String(pkg.engines?.node ?? "").replace(/^>=/, "");
const ESBUILD_TARGET = `node${enginesNode}`;

const bundlePath = path.join(buildDir, "bundle.cjs");
const seaConfigPath = path.join(buildDir, "sea-config.json");
const blobPath = path.join(buildDir, "helper.blob");
const exeName = process.platform === "win32" ? "mullion-bridge-worker.exe" : "mullion-bridge-worker";
const exePath = path.join(buildDir, exeName);

// The Node SEA "sentinel fuse" — a fixed, documented constant (not a secret,
// not versioned per-Node-release), identical for every SEA anyone builds;
// postject looks for this exact byte string in the target binary to find
// where to inject the blob. https://nodejs.org/api/single-executable-
// applications.html#generating-single-executable-preparation-blobs
const SEA_FUSE = "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2";

function log(message) {
  process.stdout.write(`[build-worker-sea] ${message}\n`);
}

async function bundle() {
  fs.rmSync(buildDir, { recursive: true, force: true });
  fs.mkdirSync(buildDir, { recursive: true });
  log(`bundling ${path.relative(repoRoot, entryPoint)} -> ${path.relative(repoRoot, bundlePath)}`);
  await esbuildBuild({
    entryPoints: [entryPoint],
    outfile: bundlePath,
    bundle: true,
    platform: "node",
    format: "cjs",
    target: ESBUILD_TARGET,
    define: { MULLION_WORKER_VERSION: JSON.stringify(pkg.version) },
    // Node's own WebSocket/fetch globals (mux.mjs and helper.mjs's renewal
    // loop) are runtime built-ins, not
    // npm packages — nothing to bundle or externalize for those.
    logOverride: { "empty-import-meta": "error" },
  });
}

function writeSeaConfig() {
  // Deliberately NOT setting `mainFormat` (defaults to "commonjs", matching
  // the bundle's own --format=cjs above) — see this file's header comment.
  // `useCodeCache`/`useSnapshot` are both left off: they're startup-latency
  // optimizations, not correctness-relevant, and this binary's own startup
  // cost is irrelevant next to the network round trips `run`/`pair` do.
  const config = {
    main: bundlePath,
    output: blobPath,
    disableExperimentalSEAWarning: true,
  };
  fs.writeFileSync(seaConfigPath, JSON.stringify(config, null, 2));
}

function generateBlob() {
  log("generating SEA blob");
  execFileSync(process.execPath, ["--experimental-sea-config", seaConfigPath], {
    stdio: "inherit",
  });
}

function copyNodeBinary() {
  // process.execPath, NOT `node` resolved through the shell/PATH — a dev
  // machine's `node` can be an nvm shim or another wrapper script, not the
  // real ELF/PE/Mach-O binary postject needs (confirmed empirically:
  // postject rejects a shim with "Executable must be a supported format").
  log(`copying node binary (${process.execPath}) -> ${path.relative(repoRoot, exePath)}`);
  fs.copyFileSync(process.execPath, exePath);
  if (process.platform !== "win32") fs.chmodSync(exePath, 0o755);
}

function removeWindowsSignature() {
  if (process.platform !== "win32") return;
  // The official node.exe is Authenticode-signed; postject's own docs note
  // injecting into a signed PE can fail or produce a binary whose signature
  // no longer verifies (it wasn't Mullion's own signature to begin with —
  // this is a copy of the stock Node build). `signtool remove /s` strips
  // it first. Best-effort: if signtool isn't on PATH or the binary was
  // already unsigned, this is a no-op failure, not a build failure — most
  // of the actual injection step below has its own real error handling.
  try {
    execFileSync("signtool", ["remove", "/s", exePath], { stdio: "inherit" });
  } catch (err) {
    log(`signtool remove skipped (non-fatal): ${err.message}`);
  }
}

// Round 4 (issue #820, macOS SEA support; self-review, PR #916) — the
// macOS counterpart to removeWindowsSignature above, but with a materially
// different consequence on failure. The official Node.org macOS build is
// Developer-ID signed; stripping that signature before injection is the
// same best-effort courtesy as the Windows case (codesign not on PATH, or
// the binary already unsigned, is fine either way).
function removeDarwinSignature() {
  if (process.platform !== "darwin") return;
  try {
    execFileSync("codesign", ["--remove-signature", exePath], { stdio: "inherit" });
  } catch (err) {
    log(`codesign --remove-signature skipped (non-fatal): ${err.message}`);
  }
}

// NOT best-effort, unlike removeDarwinSignature above: postject's injected
// NODE_SEA segment leaves the binary unsigned (or carrying a signature
// that no longer verifies against the now-modified contents) either way,
// and macOS's mandatory code-signing enforcement refuses to exec() ANY
// Mach-O binary without at least a valid ad-hoc signature — unlike an
// unsigned Windows .exe, which still runs fine (SmartScreen just warns).
// A failed re-sign here is a genuinely unrunnable artifact, not a cosmetic
// gap, so this must be allowed to throw and fail the build. Node's own
// Single Executable Applications docs' macOS walkthrough ends with exactly
// this call, in this position (after injection, not before) —
// https://nodejs.org/api/single-executable-applications.html.
function signDarwinBinary() {
  if (process.platform !== "darwin") return;
  log("re-signing (ad-hoc) after SEA blob injection");
  // Issue #1058: `--force` is required when a stale signature is still
  // attached to the binary (e.g. removeDarwinSignature above silently
  // failed because codesign exists but the existing signature is
  // corrupted). Without it, codesign refuses to overwrite the prior
  // signature and the build fails with a confusing "already signed" error
  // rather than the actual root cause.
  execFileSync("codesign", ["--sign", "-", "--force", exePath], { stdio: "inherit" });
}

// Issue #1058: pre-flight check for node_modules/.bin/postject. The
// `.bin/postject` entry is a SYMLINK on POSIX (npm's own shim scheme) and a
// `.cmd` shim on Windows — if it's broken (a stale `npm install` left it
// dangling, an nvm/fnm switch moved the underlying Node install out from
// under it, or `node_modules` was wiped without a fresh `npm ci`), spawn
// fails with an opaque ENOENT deep inside injectBlob. Catching it here
// surfaces a clear, actionable error at build start instead.
function verifyPostject() {
  const postjectBin = path.join(
    repoRoot,
    "node_modules",
    ".bin",
    process.platform === "win32" ? "postject.cmd" : "postject",
  );
  if (!fs.existsSync(postjectBin)) {
    throw new Error(
      `postject not found at ${postjectBin} — run \`npm ci\` at the repo root, then re-run \`npm run build:worker-sea\`.`,
    );
  }
  try {
    fs.accessSync(postjectBin, fs.constants.X_OK);
  } catch {
    throw new Error(
      `postject at ${postjectBin} is not executable — re-run \`npm ci\` to repair the shim, then re-run \`npm run build:worker-sea\`.`,
    );
  }
  return postjectBin;
}

function injectBlob() {
  log("injecting SEA blob via postject");
  const postjectBin = verifyPostject();
  const args = [exePath, "NODE_SEA_BLOB", blobPath, "--sentinel-fuse", SEA_FUSE];
  // Round 4 (issue #820, macOS SEA support) — macOS needs
  // --macho-segment-name NODE_SEA (postject's own docs: without it, the
  // blob isn't injected into a Mach-O segment node:sea's runtime looks for
  // it in). This flag was coded speculatively in round 3 but never
  // exercised anywhere — no macOS CI job existed, and it can only run on
  // real macOS hardware (postject operates on the actual binary format of
  // whatever `copyNodeBinary()` just copied; there's no cross-build path).
  // `.github/workflows/ci-cd.yml`'s `test-macos` job is the first real
  // exercise of this branch.
  if (process.platform === "darwin") {
    args.push("--macho-segment-name", "NODE_SEA");
  }
  // `shell: true` on win32 ONLY (confirmed necessary against the real
  // windows-latest CI runner, not guessed): npm's own `.bin/` wrapper for
  // a package's bin entry is a `.cmd` shim on Windows, not a real PE
  // executable, and Node's `spawnSync`/`execFileSync` cannot invoke a
  // `.cmd`/`.bat` file directly without a shell in between — it fails with
  // EINVAL (this exact error, on this exact line, is what CI surfaced
  // before this fix). POSIX's `postject` bin is a real file with a `#!`
  // shebang, which spawns directly just fine — `shell: true` there would
  // be an unnecessary (if likely harmless) behavior change, so this stays
  // platform-scoped rather than unconditional.
  execFileSync(postjectBin, args, { stdio: "inherit", shell: process.platform === "win32" });
}

async function main() {
  await bundle();
  writeSeaConfig();
  generateBlob();
  copyNodeBinary();
  removeWindowsSignature();
  removeDarwinSignature();
  injectBlob();
  signDarwinBinary();
  log(`built ${path.relative(repoRoot, exePath)}`);
  // The test and release workflows probe the staged executable separately.
}

main().catch((err) => {
  process.stderr.write(`[build-worker-sea] ${err instanceof Error ? err.stack : String(err)}\n`);
  process.exit(1);
});
