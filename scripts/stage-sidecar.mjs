#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const hosts = {
  "linux-x64": "x86_64-unknown-linux-gnu",
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc",
};
const host = hosts[`${process.platform}-${process.arch}`];
if (!host) throw new Error(`unsupported build host: ${process.platform}-${process.arch}`);
const extension = process.platform === "win32" ? ".exe" : "";
const source = path.join(root, "build", "worker-sea", `mullion-bridge-worker${extension}`);
const destination = path.join(root, "src-tauri", "binaries", `mullion-bridge-worker-${host}${extension}`);
fs.mkdirSync(path.dirname(destination), { recursive: true });
fs.copyFileSync(source, destination);
if (process.platform !== "win32") fs.chmodSync(destination, 0o755);
process.stdout.write(`staged ${path.relative(root, destination)}\n`);
