import fs from "node:fs";

function json(path) {
  return JSON.parse(fs.readFileSync(path, "utf8"));
}

function tomlVersion(path, packageName) {
  const source = fs.readFileSync(path, "utf8");
  const packageBlock = packageName
    ? source.match(new RegExp(`\\[\\[package\\]\\]\\nname = "${packageName}"[\\s\\S]*?(?=\\n\\[\\[|$)`))?.[0]
    : source.match(/\[package\][\s\S]*?(?=\n\[|$)/)?.[0];
  const version = packageBlock?.match(/^version = "([^"]+)"$/m)?.[1];
  if (!version) throw new Error(`Could not read ${packageName ?? "package"} version from ${path}`);
  return version;
}

const packageJson = json("package.json");
const packageLock = json("package-lock.json");
const tauriConfig = json("src-tauri/tauri.conf.json");
const manifest = json(".release-please-manifest.json");
const versions = new Map([
  ["package.json", packageJson.version],
  ["package-lock.json", packageLock.version],
  ["package-lock.json root package", packageLock.packages?.[""]?.version],
  ["src-tauri/tauri.conf.json", tauriConfig.version],
  ["src-tauri/Cargo.toml", tomlVersion("src-tauri/Cargo.toml")],
  ["src-tauri/Cargo.lock", tomlVersion("src-tauri/Cargo.lock", "mullion-helper")],
  [".release-please-manifest.json", manifest["src-tauri"]],
]);
const expected = packageJson.version;
const mismatches = [...versions].filter(([, version]) => version !== expected);
if (mismatches.length > 0) {
  const detail = [...versions].map(([name, version]) => `${name}=${version ?? "missing"}`).join(", ");
  throw new Error(`Release versions are inconsistent: ${detail}`);
}
console.log(`Release versions agree at ${expected}.`);
