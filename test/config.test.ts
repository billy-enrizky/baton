import { describe, expect, it } from "vitest";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadConfig, saveConfig, providerAuthEnv, authSignature } from "../src/config.ts";

describe("config", () => {
  it("falls back to subscription mode and env keys", () => {
    const config = loadConfig("/nonexistent-baton-home", {
      ANTHROPIC_API_KEY: "sk-ant-test",
    } as NodeJS.ProcessEnv);
    expect(config.claude.mode).toBe("subscription");
    expect(config.claude.apiKey).toBe("sk-ant-test");
    expect(config.codex.apiKey).toBeNull();
    expect(config.defaultModels.claude).toBeTypeOf("string");
    expect(config.defaultModels.codex).toBeTypeOf("string");
  });

  it("round-trips through config.json", () => {
    const home = mkdtempSync(join(tmpdir(), "baton-config-"));
    try {
      const config = loadConfig(home);
      config.codex.mode = "api";
      config.codex.apiKey = "sk-openai-test";
      config.defaultModels.claude = "claude-opus-5-5";
      saveConfig(home, config);
      const reloaded = loadConfig(home);
      expect(reloaded.codex.mode).toBe("api");
      expect(reloaded.codex.apiKey).toBe("sk-openai-test");
      expect(reloaded.defaultModels.claude).toBe("claude-opus-5-5");
      expect(reloaded.claude.mode).toBe("subscription");
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  it("survives a corrupt config file", () => {
    const home = mkdtempSync(join(tmpdir(), "baton-config-"));
    try {
      writeFileSync(join(home, "config.json"), "{not json");
      const config = loadConfig(home);
      expect(config.claude.mode).toBe("subscription");
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });
});

describe("providerAuthEnv", () => {
  it("returns no env in subscription mode", () => {
    expect(providerAuthEnv({ mode: "subscription", apiKey: "sk-x" }, "claude")).toEqual({});
  });

  it("injects the provider key env in api mode", () => {
    expect(providerAuthEnv({ mode: "api", apiKey: "sk-ant-1" }, "claude")).toEqual({
      ANTHROPIC_API_KEY: "sk-ant-1",
    });
    expect(providerAuthEnv({ mode: "api", apiKey: "sk-oai-1" }, "codex")).toEqual({
      OPENAI_API_KEY: "sk-oai-1",
    });
  });

  it("falls back to the ambient env key", () => {
    expect(
      providerAuthEnv({ mode: "api", apiKey: null }, "codex", { OPENAI_API_KEY: "ambient" } as NodeJS.ProcessEnv),
    ).toEqual({ OPENAI_API_KEY: "ambient" });
    expect(providerAuthEnv({ mode: "api", apiKey: null }, "codex")).toEqual({});
  });
});

describe("authSignature", () => {
  it("is order independent", () => {
    expect(authSignature({ A: "1", B: "2" })).toBe(authSignature({ B: "2", A: "1" }));
    expect(authSignature({})).toBe("");
  });
});

describe("scanner file discovery", () => {
  it("finds claude and codex transcripts on disk", async () => {
    const { scanClaudeSessions, scanCodexSessions } = await import("../src/scanner.ts");
    const root = mkdtempSync(join(tmpdir(), "baton-scan-"));
    try {
      const claudeProject = join(root, "claude", "projects", "-work-repo");
      mkdirSync(claudeProject, { recursive: true });
      const claudeTranscript = [
        JSON.stringify({ type: "user", sessionId: "sess-1", cwd: "/work/repo", message: { role: "user", content: "hi there" } }),
        JSON.stringify({ type: "assistant", message: { model: "claude-opus-5-5", content: "hello!" } }),
      ].join("\n");
      writeFileSync(join(claudeProject, "sess-1.jsonl"), claudeTranscript);

      const codexDay = join(root, "codex", "sessions", "2026", "10", "12");
      mkdirSync(codexDay, { recursive: true });
      const codexTranscript = [
        JSON.stringify({ type: "session_meta", payload: { id: "codex-sess-1", cwd: "/work/repo" } }),
        JSON.stringify({ type: "event_msg", payload: { type: "user_message", message: "build it" } }),
        JSON.stringify({
          type: "response_item",
          payload: { type: "message", role: "assistant", content: [{ type: "output_text", text: "built" }] },
        }),
      ].join("\n");
      writeFileSync(join(codexDay, "rollout-2026-10-12T10-00-00-codex-sess-1.jsonl"), codexTranscript);

      const claudeSessions = scanClaudeSessions(join(root, "claude", "projects"));
      const codexSessions = scanCodexSessions(join(root, "codex", "sessions"));

      expect(claudeSessions).toHaveLength(1);
      expect(claudeSessions[0]?.nativeSessionId).toBe("sess-1");
      expect(claudeSessions[0]?.messages).toHaveLength(2);
      expect(codexSessions).toHaveLength(1);
      expect(codexSessions[0]?.nativeSessionId).toBe("codex-sess-1");
      expect(codexSessions[0]?.messages.map((message) => message.text)).toEqual(["build it", "built"]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
