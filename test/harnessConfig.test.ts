import { describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseTomlMinimal, readClaudeHarnessConfig, readCodexHarnessConfig } from "../src/harnessConfig.ts";

function tempRoot(): string {
  return mkdtempSync(join(tmpdir(), "baton-harness-"));
}

describe("parseTomlMinimal", () => {
  it("parses scalars, sections, and ignores comments and arrays", () => {
    const parsed = parseTomlMinimal(`
# comment line
model = "gpt-6.1-sol"
check_for_update_on_startup = false
model_context_window = 1000000

[model_providers.ica]
name = "ICA"
env_key = "CODEX_ICA_API_KEY"
stream_idle_timeout_ms = 300000

[sandbox_workspace_write]
network_access = true

notify = ["/some/app", "turn-ended"]
query_params = { api-version = "2025-03-01-preview" }
`);
    expect(parsed.model).toBe("gpt-6.1-sol");
    expect(parsed.check_for_update_on_startup).toBe(false);
    expect(parsed.model_context_window).toBe(1000000);
    const providers = parsed.model_providers as Record<string, Record<string, unknown>>;
    expect(providers.ica?.env_key).toBe("CODEX_ICA_API_KEY");
    expect(providers.ica?.name).toBe("ICA");
    const sandbox = parsed.sandbox_workspace_write as Record<string, unknown>;
    expect(sandbox.network_access).toBe(true);
    expect(parsed.notify).toBeUndefined();
    expect(parsed.query_params).toBeUndefined();
  });

  it("parses literal strings and escapes in basic strings", () => {
    const parsed = parseTomlMinimal(`
literal = 'C:\\path'
basic = "line\\nbreak"
`);
    expect(parsed.literal).toBe("C:\\path");
    expect(parsed.basic).toBe("line\nbreak");
  });

  it("does not treat # inside strings as comments", () => {
    const parsed = parseTomlMinimal('model = "gpt#not-comment"');
    expect(parsed.model).toBe("gpt#not-comment");
  });
});

describe("readClaudeHarnessConfig", () => {
  it("layers global, project, and local settings", () => {
    const root = tempRoot();
    try {
      const globalDir = join(root, "claude-global");
      const project = join(root, "project");
      mkdirSync(globalDir, { recursive: true });
      mkdirSync(join(project, ".claude"), { recursive: true });
      writeFileSync(
        join(globalDir, "settings.json"),
        JSON.stringify({
          model: "claude-sonnet-4-5",
          permissions: { defaultMode: "default" },
        }),
      );
      writeFileSync(
        join(project, ".claude", "settings.json"),
        JSON.stringify({ permissions: { defaultMode: "plan" } }),
      );
      writeFileSync(
        join(project, ".claude", "settings.local.json"),
        JSON.stringify({ model: "claude-opus-5-5" }),
      );

      const config = readClaudeHarnessConfig(globalDir, project);
      expect(config.model).toBe("claude-opus-5-5");
      expect(config.permissionMode).toBe("plan");
      expect(config.apiKey).toBeNull();
      expect(config.authMode).toBeNull();
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("detects api mode from settings env ANTHROPIC_API_KEY and maps dontAsk", () => {
    const root = tempRoot();
    try {
      const globalDir = join(root, "claude-global");
      mkdirSync(globalDir, { recursive: true });
      writeFileSync(
        join(globalDir, "settings.json"),
        JSON.stringify({
          env: { ANTHROPIC_API_KEY: "sk-ant-from-settings" },
          permissions: { defaultMode: "dontAsk" },
        }),
      );
      const config = readClaudeHarnessConfig(globalDir, null);
      expect(config.authMode).toBe("api");
      expect(config.apiKey).toBe("sk-ant-from-settings");
      expect(config.permissionMode).toBe("acceptEdits");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("maps bypassPermissions and dangerously-skip-permissions", () => {
    const root = tempRoot();
    try {
      const globalDir = join(root, "claude-global");
      mkdirSync(globalDir, { recursive: true });
      writeFileSync(
        join(globalDir, "settings.json"),
        JSON.stringify({ permissions: { defaultMode: "bypassPermissions" } }),
      );
      expect(readClaudeHarnessConfig(globalDir, null).permissionMode).toBe("bypassPermissions");
      writeFileSync(
        join(globalDir, "settings.json"),
        JSON.stringify({ permissions: { defaultMode: "dangerously-skip-permissions" } }),
      );
      expect(readClaudeHarnessConfig(globalDir, null).permissionMode).toBe("bypassPermissions");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("returns empty config when no settings exist", () => {
    const config = readClaudeHarnessConfig(join(tempRoot(), "missing"), null);
    expect(config.model).toBeNull();
    expect(config.permissionMode).toBeNull();
  });
});

describe("readCodexHarnessConfig", () => {
  it("reads model, approval policy, and custom provider env key", () => {
    const root = tempRoot();
    try {
      const home = join(root, "codex-home");
      mkdirSync(home, { recursive: true });
      writeFileSync(
        join(home, "config.toml"),
        [
          'model = "gpt-5.6-luna"',
          'model_provider = "ica"',
          'approval_policy = "never"',
          'sandbox_mode = "danger-full-access"',
          "",
          "[model_providers.ica]",
          'name = "ICA"',
          'env_key = "CODEX_ICA_API_KEY"',
        ].join("\n"),
      );
      const config = readCodexHarnessConfig(home, null);
      expect(config.model).toBe("gpt-5.6-luna");
      expect(config.modelProvider).toBe("ica");
      expect(config.authMode).toBe("api");
      expect(config.apiKeyEnvName).toBe("CODEX_ICA_API_KEY");
      expect(config.permissionMode).toBe("bypassPermissions");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("project config overrides global, deep-merging sections", () => {
    const root = tempRoot();
    try {
      const home = join(root, "codex-home");
      const project = join(root, "project");
      mkdirSync(home, { recursive: true });
      mkdirSync(join(project, ".codex"), { recursive: true });
      writeFileSync(
        join(home, "config.toml"),
        [
          'model = "gpt-5.6-luna"',
          'model_provider = "ica"',
          'approval_policy = "on-request"',
          "",
          "[model_providers.ica]",
          'env_key = "KEY_A"',
        ].join("\n"),
      );
      writeFileSync(join(project, ".codex", "config.toml"), ['model = "gpt-6.1-sol"'].join("\n"));
      const config = readCodexHarnessConfig(home, project);
      expect(config.model).toBe("gpt-6.1-sol");
      expect(config.permissionMode).toBe("default");
      const mergedProviders = config as unknown as { apiKeyEnvName: string | null };
      expect(mergedProviders.apiKeyEnvName).toBe("KEY_A");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("respects preferred_auth_method chatgpt as subscription", () => {
    const root = tempRoot();
    try {
      const home = join(root, "codex-home");
      mkdirSync(home, { recursive: true });
      writeFileSync(
        join(home, "config.toml"),
        ['preferred_auth_method = "chatgpt"', 'approval_policy = "on-failure"'].join("\n"),
      );
      const config = readCodexHarnessConfig(home, null);
      expect(config.authMode).toBe("subscription");
      expect(config.permissionMode).toBe("acceptEdits");
      expect(config.apiKeyEnvName).toBeNull();
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("respects preferred_auth_method apikey", () => {
    const root = tempRoot();
    try {
      const home = join(root, "codex-home");
      mkdirSync(home, { recursive: true });
      writeFileSync(join(home, "config.toml"), ['preferred_auth_method = "apikey"'].join("\n"));
      const config = readCodexHarnessConfig(home, null);
      expect(config.authMode).toBe("api");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
