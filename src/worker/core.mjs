export class CliUsageError extends Error {}

export function extractFlags(tokens, spec, { strict = true } = {}) {
  const flags = {};
  const rest = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (!token.startsWith("--")) {
      rest.push(token);
      continue;
    }
    let name = token.slice(2);
    let inline;
    const separator = name.indexOf("=");
    if (separator !== -1) {
      inline = name.slice(separator + 1);
      name = name.slice(0, separator);
    }
    const kind = spec[name];
    if (!kind) {
      if (strict) throw new CliUsageError(`unknown flag --${name}`);
      rest.push(token);
      continue;
    }
    if (kind === "boolean") {
      flags[name] = inline === undefined ? true : inline !== "false";
      continue;
    }
    const raw = inline === undefined ? tokens[++i] : inline;
    if (raw === undefined) throw new CliUsageError(`--${name} requires a value`);
    flags[name] = raw;
  }
  return { flags, rest };
}
