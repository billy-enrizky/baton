import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import type { AuthMode, PermissionMode, ProviderId } from "./types.ts";
import { readHarnessConfig } from "./harnessConfig.ts";

export type { AuthMode };
export interface ProviderAuth {
  mode: AuthMode;
  apiKey: string | null;
  apiKeyEnvName: string | null;
}

export interface EffectiveProviderConfig extends ProviderAuth {
  model: string;
  permissionMode: PermissionMode;
  harnessAuthMode: AuthMode | null;
}

export interface EffectiveConfig {
  claude: EffectiveProviderConfig;
  codex: EffectiveProviderConfig;
}

export interface BatonConfigFile {
  claude?: { mode?: AuthMode; apiKey?: string } | null;
  codex?: { mode?: AuthMode; apiKey?: string } | null;
  defaultModels?: Partial<Record<ProviderId, string>> | null;
}

const API_KEY_ENV: Record<ProviderId, string> = {
  claude: "ANTHROPIC_API_KEY",
  codex: "OPENAI_API_KEY",
};

export const DEFAULT_MODELS: Record<ProviderId, string> = {
  claude: "claude-sonnet-4-5",
  codex: "gpt-6.1-sol",
};

export function configPath(home: string): string {
  return join(home, "config.json");
}

export function loadConfigFile(home: string): BatonConfigFile | null {
  const path = configPath(home);
  if (!existsSync(path)) return null;
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as BatonConfigFile;
    if (typeof parsed !== "object" || parsed === null) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function saveConfigFile(home: string, file: BatonConfigFile): void {
  mkdirSync(home, { recursive: true });
  writeFileSync(configPath(home), `${JSON.stringify(file, null, 2)}\n`, "utf8");
}

export function resolveProviderConfig(
  provider: ProviderId,
  input: { home: string; cwd: string; env?: NodeJS.ProcessEnv; claudeDir?: string; codexHome?: string },
): EffectiveProviderConfig {
  const env = input.env ?? process.env;
  const file = loadConfigFile(input.home);
  const harness = readHarnessConfig(provider, {
    env,
    projectRoot: input.cwd,
    claudeDir: input.claudeDir,
    codexHome: input.codexHome,
  });
  const apiKeyEnvName = harness.apiKeyEnvName ?? API_KEY_ENV[provider];
  const fileProvider = file?.[provider] ?? null;
  const fileModel = file?.defaultModels?.[provider] ?? null;
  const mode = fileProvider?.mode ?? harness.authMode ?? "subscription";
  const apiKey =
    fileProvider?.apiKey?.trim() ||
    harness.apiKey?.trim() ||
    env[apiKeyEnvName]?.trim() ||
    null;
  return {
    mode,
    apiKey,
    apiKeyEnvName,
    model: fileModel ?? harness.model ?? DEFAULT_MODELS[provider],
    permissionMode: harness.permissionMode ?? "default",
    harnessAuthMode: harness.authMode,
  };
}

export function resolveConfig(input: {
  home: string;
  cwd: string;
  env?: NodeJS.ProcessEnv;
  claudeDir?: string;
  codexHome?: string;
}): EffectiveConfig {
  return {
    claude: resolveProviderConfig("claude", input),
    codex: resolveProviderConfig("codex", input),
  };
}

export function providerAuthEnv(
  auth: ProviderAuth,
  provider: ProviderId,
  env: NodeJS.ProcessEnv = process.env,
): Record<string, string> {
  if (auth.mode !== "api") return {};
  const envName = auth.apiKeyEnvName ?? API_KEY_ENV[provider];
  const apiKey = auth.apiKey?.trim() || env[envName]?.trim();
  if (!apiKey) return {};
  return { [envName]: apiKey };
}

export function authSignature(envAdditions: Record<string, string>): string {
  const entries = Object.entries(envAdditions).map(([key, value]) => `${key}=${value}`);
  entries.sort();
  return entries.join(";");
}

export function describeAuth(auth: ProviderAuth, env: NodeJS.ProcessEnv = process.env): string {
  if (auth.mode === "subscription") return "subscription (harness login)";
  const envName = auth.apiKeyEnvName ?? "API key";
  const hasKey = Boolean(auth.apiKey?.trim() || env[envName]?.trim());
  return hasKey ? `api (${envName})` : `api (MISSING ${envName})`;
}
