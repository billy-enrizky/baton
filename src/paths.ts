import { homedir } from "node:os";
import { join } from "node:path";

export function batonHome(): string {
  const override = process.env.BATON_HOME?.trim();
  if (override) return override;
  return join(homedir(), ".baton");
}

export function batonDbPath(): string {
  return join(batonHome(), "baton.sqlite");
}

export function claudeProjectsDir(env: NodeJS.ProcessEnv = process.env): string {
  const configDir = env.CLAUDE_CONFIG_DIR?.trim();
  if (configDir) return join(configDir, "projects");
  return join(homedir(), ".claude", "projects");
}

export function codexSessionsDir(env: NodeJS.ProcessEnv = process.env): string {
  const codexHome = env.CODEX_HOME?.trim();
  if (codexHome) return join(codexHome, "sessions");
  return join(homedir(), ".codex", "sessions");
}
