import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import type { ProviderId } from "./types.ts";

export type AuthMode = "subscription" | "api";

export interface ProviderAuth {
  mode: AuthMode;
  apiKey: string | null;
}

export interface BatonConfig {
  claude: ProviderAuth;
  codex: ProviderAuth;
  defaultModels: Record<ProviderId, string>;
}

const API_KEY_ENV: Record<ProviderId, string> = {
  claude: "ANTHROPIC_API_KEY",
  codex: "OPENAI_API_KEY",
};

export const DEFAULT_MODELS: Record<ProviderId, string> = {
  claude: "claude-sonnet-4-5",
  codex: "gpt-6.1-sol",
};

export function defaultConfig(env: NodeJS.ProcessEnv = process.env): BatonConfig {
  return {
    claude: { mode: "subscription", apiKey: env.ANTHROPIC_API_KEY?.trim() || null },
    codex: { mode: "subscription", apiKey: env.OPENAI_API_KEY?.trim() || null },
    defaultModels: { ...DEFAULT_MODELS },
  };
}

export function configPath(home: string): string {
  return join(home, "config.json");
}

export function loadConfig(home: string, env: NodeJS.ProcessEnv = process.env): BatonConfig {
  const fallback = defaultConfig(env);
  const path = configPath(home);
  if (!existsSync(path)) return fallback;
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as {
      claude?: { mode?: unknown; apiKey?: unknown };
      codex?: { mode?: unknown; apiKey?: unknown };
      defaultModels?: { claude?: unknown; codex?: unknown };
    };
    return {
      claude: mergeProviderAuth(parsed.claude, fallback.claude),
      codex: mergeProviderAuth(parsed.codex, fallback.codex),
      defaultModels: {
        claude: typeof parsed.defaultModels?.claude === "string" ? parsed.defaultModels.claude : fallback.defaultModels.claude,
        codex: typeof parsed.defaultModels?.codex === "string" ? parsed.defaultModels.codex : fallback.defaultModels.codex,
      },
    };
  } catch {
    return fallback;
  }
}

function mergeProviderAuth(
  stored: { mode?: unknown; apiKey?: unknown } | undefined,
  fallback: ProviderAuth,
): ProviderAuth {
  if (!stored) return fallback;
  const mode = stored.mode === "api" || stored.mode === "subscription" ? stored.mode : fallback.mode;
  const apiKey =
    typeof stored.apiKey === "string" && stored.apiKey.trim().length > 0 ? stored.apiKey.trim() : fallback.apiKey;
  return { mode, apiKey };
}

export function saveConfig(home: string, config: BatonConfig): void {
  mkdirSync(home, { recursive: true });
  writeFileSync(configPath(home), `${JSON.stringify(config, null, 2)}\n`, "utf8");
}

export function providerAuthEnv(
  auth: ProviderAuth,
  provider: ProviderId,
  env: NodeJS.ProcessEnv = process.env,
): Record<string, string> {
  if (auth.mode !== "api") return {};
  const apiKey = auth.apiKey?.trim() || env[API_KEY_ENV[provider]]?.trim();
  if (!apiKey) return {};
  return { [API_KEY_ENV[provider]]: apiKey };
}

export function authSignature(envAdditions: Record<string, string>): string {
  const entries = Object.entries(envAdditions).map(([key, value]) => `${key}=${value}`);
  entries.sort();
  return entries.join(";");
}

export function describeAuth(auth: ProviderAuth, provider: ProviderId, env: NodeJS.ProcessEnv = process.env): string {
  if (auth.mode === "subscription") return "subscription (harness login)";
  const hasKey = Boolean(auth.apiKey?.trim() || env[API_KEY_ENV[provider]]?.trim());
  return hasKey ? "api (key configured)" : "api (MISSING KEY)";
}
