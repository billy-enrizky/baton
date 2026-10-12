import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";
import type { AuthMode, PermissionMode, ProviderId } from "./types.ts";
export interface HarnessProviderConfig {
  model: string | null;
  authMode: AuthMode | null;
  apiKey: string | null;
  apiKeyEnvName: string | null;
  permissionMode: PermissionMode | null;
  modelProvider: string | null;
}

const EMPTY_HARNESS_CONFIG: HarnessProviderConfig = {
  model: null,
  authMode: null,
  apiKey: null,
  apiKeyEnvName: null,
  permissionMode: null,
  modelProvider: null,
};

export function parseTomlMinimal(contents: string): Record<string, unknown> {
  const root: Record<string, unknown> = {};
  let current = root;
  for (const rawLine of contents.split("\n")) {
    const line = stripTomlComment(rawLine).trim();
    if (line.length === 0) continue;
    if (line.startsWith("[") && line.endsWith("]")) {
      const sectionPath = line.slice(1, -1).trim();
      if (sectionPath.length === 0) continue;
      current = navigateSection(root, sectionPath.split("."));
      continue;
    }
    const equalsIndex = line.indexOf("=");
    if (equalsIndex === -1) continue;
    const key = line.slice(0, equalsIndex).trim().replace(/^["']|["']$/g, "");
    if (key.length === 0) continue;
    const value = parseTomlValue(line.slice(equalsIndex + 1).trim());
    if (value !== undefined) current[key] = value;
  }
  return root;
}

function navigateSection(root: Record<string, unknown>, path: string[]): Record<string, unknown> {
  let current = root;
  for (const segment of path) {
    const name = segment.trim().replace(/^["']|["']$/g, "");
    let next = current[name];
    if (typeof next !== "object" || next === null || Array.isArray(next)) {
      next = {};
      current[name] = next;
    }
    current = next as Record<string, unknown>;
  }
  return current;
}

function stripTomlComment(line: string): string {
  let inBasicString = false;
  let inLiteralString = false;
  for (let index = 0; index < line.length; index++) {
    const char = line[index];
    if (char === '"' && !inLiteralString) inBasicString = !inBasicString;
    else if (char === "'" && !inBasicString) inLiteralString = !inLiteralString;
    else if (char === "#" && !inBasicString && !inLiteralString) return line.slice(0, index);
  }
  return line;
}

const BASIC_STRING_ESCAPES: Record<string, string> = {
  n: "\n",
  t: "\t",
  r: "\r",
  '"': '"',
  "\\": "\\",
};

function unescapeBasicString(value: string): string {
  return value.replace(/\\(n|t|r|"|\\)/g, (_, escape: string) => BASIC_STRING_ESCAPES[escape] ?? escape);
}

function parseTomlValue(raw: string): unknown {
  if (raw.length === 0) return undefined;
  if (raw.startsWith('"')) {
    const match = /^"((?:[^"\\]|\\.)*)"/.exec(raw);
    if (!match) return undefined;
    return unescapeBasicString(match[1] ?? "");
  }
  if (raw.startsWith("'")) {
    const match = /^'([^']*)'/.exec(raw);
    if (!match) return undefined;
    return match[1] ?? "";
  }
  const bare = raw.split(/\s+/)[0] ?? "";
  if (bare === "true") return true;
  if (bare === "false") return false;
  if (/^-?\d+$/.test(bare)) return Number(bare);
  if (/^-?\d+\.\d+$/.test(bare)) return Number(bare);
  return undefined;
}

function readJsonIfExists(path: string): Record<string, unknown> | null {
  if (!existsSync(path)) return null;
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as unknown;
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

function readTomlIfExists(path: string): Record<string, unknown> | null {
  if (!existsSync(path)) return null;
  try {
    return parseTomlMinimal(readFileSync(path, "utf8"));
  } catch {
    return null;
  }
}

const CLAUDE_PERMISSION_MODE_MAP: Record<string, PermissionMode> = {
  default: "default",
  acceptEdits: "acceptEdits",
  plan: "plan",
  bypassPermissions: "bypassPermissions",
  "dangerously-skip-permissions": "bypassPermissions",
  dontAsk: "acceptEdits",
};

const CODEX_APPROVAL_POLICY_MAP: Record<string, PermissionMode> = {
  untrusted: "default",
  "on-request": "default",
  "on-failure": "acceptEdits",
  never: "bypassPermissions",
};

export function claudeGlobalSettingsDir(env: NodeJS.ProcessEnv = process.env): string {
  const configDir = env.CLAUDE_CONFIG_DIR?.trim();
  if (configDir) return configDir;
  return join(homedir(), ".claude");
}

export function readClaudeHarnessConfig(
  globalDir: string,
  projectRoot: string | null,
): HarnessProviderConfig {
  const layers = [
    readJsonIfExists(join(globalDir, "settings.json")),
    projectRoot ? readJsonIfExists(join(projectRoot, ".claude", "settings.json")) : null,
    projectRoot ? readJsonIfExists(join(projectRoot, ".claude", "settings.local.json")) : null,
  ].filter((layer): layer is Record<string, unknown> => layer !== null);
  if (layers.length === 0) return { ...EMPTY_HARNESS_CONFIG };

  const merged: Record<string, unknown> = {};
  const mergedEnv: Record<string, unknown> = {};
  let defaultMode: unknown;
  for (const layer of layers) {
    Object.assign(merged, layer);
    const env = layer.env;
    if (typeof env === "object" && env !== null && !Array.isArray(env)) {
      Object.assign(mergedEnv, env as Record<string, unknown>);
    }
    const permissions = layer.permissions;
    if (typeof permissions === "object" && permissions !== null && !Array.isArray(permissions)) {
      const mode = (permissions as Record<string, unknown>).defaultMode;
      if (typeof mode === "string" && mode.length > 0) defaultMode = mode;
    }
  }

  const envModel =
    typeof mergedEnv.ANTHROPIC_MODEL === "string" && mergedEnv.ANTHROPIC_MODEL.trim().length > 0
      ? mergedEnv.ANTHROPIC_MODEL.trim()
      : null;
  const settingsModel =
    typeof merged.model === "string" && merged.model.trim().length > 0 ? merged.model.trim() : null;
  const model = envModel ?? settingsModel;
  const apiKey =
    typeof mergedEnv.ANTHROPIC_API_KEY === "string" && mergedEnv.ANTHROPIC_API_KEY.trim().length > 0
      ? mergedEnv.ANTHROPIC_API_KEY.trim()
      : null;
  const permissionMode =
    typeof defaultMode === "string" ? (CLAUDE_PERMISSION_MODE_MAP[defaultMode] ?? null) : null;

  return {
    ...EMPTY_HARNESS_CONFIG,
    model,
    apiKey,
    authMode: apiKey !== null ? "api" : null,
    permissionMode,
  };
}

export function codexGlobalHome(env: NodeJS.ProcessEnv = process.env): string {
  const codexHome = env.CODEX_HOME?.trim();
  if (codexHome) return codexHome;
  return join(homedir(), ".codex");
}

export function readCodexHarnessConfig(
  globalHome: string,
  projectRoot: string | null,
): HarnessProviderConfig {
  const globalLayer = readTomlIfExists(join(globalHome, "config.toml"));
  const projectLayer = projectRoot ? readTomlIfExists(join(projectRoot, ".codex", "config.toml")) : null;
  if (!globalLayer && !projectLayer) return { ...EMPTY_HARNESS_CONFIG };

  const merged: Record<string, unknown> = { ...(globalLayer ?? {}) };
  if (projectLayer) {
    for (const [key, value] of Object.entries(projectLayer)) {
      if (typeof value === "object" && value !== null && !Array.isArray(value)) {
        merged[key] = { ...((merged[key] as Record<string, unknown>) ?? {}), ...(value as Record<string, unknown>) };
      } else {
        merged[key] = value;
      }
    }
  }

  const model = typeof merged.model === "string" && merged.model.trim().length > 0 ? merged.model.trim() : null;
  const modelProvider = typeof merged.model_provider === "string" ? merged.model_provider : "openai";
  const providers =
    typeof merged.model_providers === "object" && merged.model_providers !== null
      ? (merged.model_providers as Record<string, unknown>)
      : {};
  const providerEntry =
    typeof providers[modelProvider] === "object" && providers[modelProvider] !== null
      ? (providers[modelProvider] as Record<string, unknown>)
      : null;
  const envKeyName =
    typeof providerEntry?.env_key === "string" && providerEntry.env_key.trim().length > 0
      ? providerEntry.env_key.trim()
      : null;
  const preferredAuth = typeof merged.preferred_auth_method === "string" ? merged.preferred_auth_method : null;

  let authMode: AuthMode | null = null;
  if (preferredAuth === "apikey") authMode = "api";
  else if (preferredAuth === "chatgpt") authMode = "subscription";
  else if (modelProvider !== "openai" && envKeyName !== null) authMode = "api";

  const approvalPolicy = typeof merged.approval_policy === "string" ? merged.approval_policy : null;
  const permissionMode = approvalPolicy !== null ? (CODEX_APPROVAL_POLICY_MAP[approvalPolicy] ?? null) : null;

  return {
    ...EMPTY_HARNESS_CONFIG,
    model,
    modelProvider,
    authMode,
    apiKeyEnvName: envKeyName,
    permissionMode,
  };
}

export function readHarnessConfig(
  provider: ProviderId,
  input: {
    env?: NodeJS.ProcessEnv;
    projectRoot: string | null;
    claudeDir?: string;
    codexHome?: string;
  },
): HarnessProviderConfig {
  const env = input.env ?? process.env;
  if (provider === "claude") {
    return readClaudeHarnessConfig(input.claudeDir ?? claudeGlobalSettingsDir(env), input.projectRoot);
  }
  return readCodexHarnessConfig(input.codexHome ?? codexGlobalHome(env), input.projectRoot);
}
