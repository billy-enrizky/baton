import { describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  loadConfigFile,
  saveConfigFile,
  resolveConfig,
  providerAuthEnv,
  authSignature,
} from "../src/config.ts";

function tempRoot(): string {
  return mkdtempSync(join(tmpdir(), "baton-config-"));
}

function writeHarnessConfigs(root: string, claude: object | null, codex: string[] | null, project?: object) {
  if (claude !== null) {
    const dir = join(root, "claude-home");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "settings.json"), JSON.stringify(claude));
  }
  if (codex !== null) {
    const dir = join(root, "codex-home");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "config.toml"), codex.join("\n"));
  }
}

describe("loadConfigFile", () => {
  it("returns null when no file exists and round-trips saves", () => {
    const home = tempRoot();
    try {
      expect(loadConfigFile(home)).toBeNull();
      saveConfigFile(home, { codex: { mode: "api" } });
      const file = loadConfigFile(home);
      expect(file?.codex?.mode).toBe("api");
      expect(file?.claude).toBeUndefined();
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  it("survives corrupt files", () => {
    const home = tempRoot();
    try {
      writeFileSync(join(home, "config.json"), "{not json");
      expect(loadConfigFile(home)).toBeNull();
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });
});

describe("resolveConfig", () => {
  it("falls back to built-in defaults with empty env and no harness configs", () => {
    const resolved = resolveConfig({
      home: "/nonexistent",
      cwd: "/nowhere",
      env: {} as NodeJS.ProcessEnv,
      claudeDir: "/nonexistent",
      codexHome: "/nonexistent",
    });
    expect(resolved.claude.mode).toBe("subscription");
    expect(resolved.claude.model).toBe("claude-sonnet-4-5");
    expect(resolved.codex.model).toBe("gpt-6.1-sol");
    expect(resolved.claude.permissionMode).toBe("default");
  });

  it("adopts harness configs for models, auth, and permission modes", () => {
    const root = tempRoot();
    try {
      writeHarnessConfigs(
        root,
        { model: "claude-opus-5-5", permissions: { defaultMode: "acceptEdits" } },
        ['model = "gpt-5.6-luna"', 'approval_policy = "never"'],
      );
      const resolved = resolveConfig({
        home: "/nonexistent",
        cwd: "/nowhere",
        env: {} as NodeJS.ProcessEnv,
        claudeDir: join(root, "claude-home"),
        codexHome: join(root, "codex-home"),
      });
      expect(resolved.claude.model).toBe("claude-opus-5-5");
      expect(resolved.claude.permissionMode).toBe("acceptEdits");
      expect(resolved.codex.model).toBe("gpt-5.6-luna");
      expect(resolved.codex.permissionMode).toBe("bypassPermissions");
      expect(resolved.codex.mode).toBe("subscription");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("prefers claude env ANTHROPIC_MODEL over the settings model field", () => {
    const root = tempRoot();
    try {
      writeHarnessConfigs(
        root,
        { model: "claude-sonnet-5-5", env: { ANTHROPIC_MODEL: "claude-opus-5-5[1m]" } },
        null,
      );
      const resolved = resolveConfig({
        home: "/nonexistent",
        cwd: "/nowhere",
        env: {} as NodeJS.ProcessEnv,
        claudeDir: join(root, "claude-home"),
        codexHome: "/nonexistent",
      });
      expect(resolved.claude.model).toBe("claude-opus-5-5[1m]");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("baton config file overrides harness config", () => {
    const root = tempRoot();
    const home = tempRoot();
    try {
      writeHarnessConfigs(root, { model: "claude-opus-5-5" }, ['model = "gpt-5.6-luna"']);
      saveConfigFile(home, {
        claude: { mode: "api" },
        defaultModels: { codex: "gpt-6.1-sol" },
      });
      const resolved = resolveConfig({
        home,
        cwd: "/nowhere",
        env: { ANTHROPIC_API_KEY: "sk-ant-ambient" } as NodeJS.ProcessEnv,
        claudeDir: join(root, "claude-home"),
        codexHome: join(root, "codex-home"),
      });
      expect(resolved.claude.mode).toBe("api");
      expect(resolved.claude.apiKey).toBe("sk-ant-ambient");
      expect(resolved.claude.model).toBe("claude-opus-5-5");
      expect(resolved.codex.model).toBe("gpt-6.1-sol");
    } finally {
      rmSync(root, { recursive: true, force: true });
      rmSync(home, { recursive: true, force: true });
    }
  });

  it("detects api mode from a codex custom provider env key", () => {
    const root = tempRoot();
    try {
      writeHarnessConfigs(root, null, [
        'model_provider = "ica"',
        "[model_providers.ica]",
        'env_key = "CODEX_ICA_API_KEY"',
      ]);
      const resolved = resolveConfig({
        home: "/nonexistent",
        cwd: "/nowhere",
        env: { CODEX_ICA_API_KEY: "ica-key" } as NodeJS.ProcessEnv,
        claudeDir: "/nonexistent",
        codexHome: join(root, "codex-home"),
      });
      expect(resolved.codex.mode).toBe("api");
      expect(resolved.codex.apiKeyEnvName).toBe("CODEX_ICA_API_KEY");
      expect(resolved.codex.apiKey).toBe("ica-key");
      const authEnv = providerAuthEnv(resolved.codex, "codex", {} as NodeJS.ProcessEnv);
      expect(authEnv).toEqual({ CODEX_ICA_API_KEY: "ica-key" });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("providerAuthEnv", () => {
  it("returns no env in subscription mode", () => {
    expect(
      providerAuthEnv({ mode: "subscription", apiKey: "sk-x", apiKeyEnvName: null }, "claude"),
    ).toEqual({});
  });

  it("injects under the resolved env key name", () => {
    expect(
      providerAuthEnv({ mode: "api", apiKey: "sk-ant-1", apiKeyEnvName: null }, "claude"),
    ).toEqual({ ANTHROPIC_API_KEY: "sk-ant-1" });
    expect(
      providerAuthEnv({ mode: "api", apiKey: "ica-key", apiKeyEnvName: "CODEX_ICA_API_KEY" }, "codex"),
    ).toEqual({ CODEX_ICA_API_KEY: "ica-key" });
  });

  it("falls back to ambient env by key name", () => {
    expect(
      providerAuthEnv({ mode: "api", apiKey: null, apiKeyEnvName: "OPENAI_API_KEY" }, "codex", {
        OPENAI_API_KEY: "ambient",
      } as NodeJS.ProcessEnv),
    ).toEqual({ OPENAI_API_KEY: "ambient" });
    expect(
      providerAuthEnv({ mode: "api", apiKey: null, apiKeyEnvName: null }, "codex"),
    ).toEqual({});
  });
});

describe("authSignature", () => {
  it("is order independent", () => {
    expect(authSignature({ A: "1", B: "2" })).toBe(authSignature({ B: "2", A: "1" }));
    expect(authSignature({})).toBe("");
  });
});
