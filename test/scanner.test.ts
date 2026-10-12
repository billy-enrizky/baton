import { describe, expect, it } from "vitest";
import { parseClaudeTranscript, parseCodexTranscript } from "../src/scanner.ts";

describe("parseClaudeTranscript", () => {
  it("extracts user and assistant messages and session metadata", () => {
    const contents = [
      JSON.stringify({
        type: "user",
        sessionId: "session-123",
        cwd: "/work/project",
        timestamp: "2026-10-11T10:00:00.000Z",
        message: { role: "user", content: "fix the login bug" },
      }),
      JSON.stringify({
        type: "assistant",
        sessionId: "session-123",
        timestamp: "2026-10-11T10:00:05.000Z",
        message: {
          model: "claude-opus-5-5",
          content: [
            { type: "text", text: "fixed it" },
            { type: "tool_use", id: "t1", name: "Bash", input: {} },
          ],
        },
      }),
    ].join("\n");

    const session = parseClaudeTranscript(contents, {
      path: "/home/.claude/projects/-work-project/session-123.jsonl",
      lastActiveAtMs: Date.parse("2026-10-11T10:00:05.000Z"),
      fallbackSessionId: "session-123",
    });
    expect(session).not.toBeNull();
    expect(session?.nativeSessionId).toBe("session-123");
    expect(session?.cwd).toBe("/work/project");
    expect(session?.model).toBe("claude-opus-5-5");
    expect(session?.messages).toHaveLength(2);
    expect(session?.messages[0]).toMatchObject({ role: "user", text: "fix the login bug" });
    expect(session?.messages[1]).toMatchObject({ role: "assistant", text: "fixed it" });
  });

  it("skips sidechain, meta, compact records and synthetic models", () => {
    const contents = [
      JSON.stringify({
        type: "user",
        isSidechain: true,
        message: { role: "user", content: "subagent chatter" },
      }),
      JSON.stringify({ type: "summary", isCompactSummary: true, summary: "compact" }),
      JSON.stringify({
        type: "user",
        isMeta: true,
        message: { role: "user", content: "meta" },
      }),
      JSON.stringify({
        type: "user",
        sessionId: "s1",
        message: { role: "user", content: "real prompt" },
      }),
      JSON.stringify({
        type: "assistant",
        message: { model: "<synthetic>", content: "local error" },
      }),
      JSON.stringify({
        type: "assistant",
        message: { model: "claude-opus-5-5", content: "real answer" },
      }),
    ].join("\n");

    const session = parseClaudeTranscript(contents, {
      path: "/x/s1.jsonl",
      lastActiveAtMs: 1000,
      fallbackSessionId: "s1",
    });
    expect(session?.messages.map((message) => message.text)).toEqual([
      "real prompt",
      "local error",
      "real answer",
    ]);
    expect(session?.model).toBe("claude-opus-5-5");
  });

  it("returns null without a session id or user message", () => {
    const contents = JSON.stringify({
      type: "assistant",
      message: { role: "assistant", content: "only assistant" },
    });
    expect(
      parseClaudeTranscript(contents, { path: "/x/s.jsonl", lastActiveAtMs: 1, fallbackSessionId: "" }),
    ).toBeNull();
  });
});

describe("parseCodexTranscript", () => {
  const base = {
    path: "/home/.codex/sessions/2026/10/11/rollout-x.jsonl",
    lastActiveAtMs: Date.parse("2026-10-11T11:00:00.000Z"),
  };

  function rollout(record: Record<string, unknown>): string {
    return JSON.stringify(record);
  }

  it("extracts messages, session id, model, and cwd", () => {
    const contents = [
      rollout({ type: "session_meta", payload: { id: "native-thread-1", cwd: "/work/repo" } }),
      rollout({ type: "turn_context", payload: { model: "gpt-6.1-sol", cwd: "/work/repo" } }),
      rollout({
        type: "event_msg",
        payload: { type: "user_message", message: "add the endpoint" },
        timestamp: "2026-10-11T10:59:00.000Z",
      }),
      rollout({
        type: "response_item",
        payload: {
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "endpoint added" }],
        },
        timestamp: "2026-10-11T11:00:00.000Z",
      }),
    ].join("\n");

    const session = parseCodexTranscript(contents, base);
    expect(session?.nativeSessionId).toBe("native-thread-1");
    expect(session?.model).toBe("gpt-6.1-sol");
    expect(session?.cwd).toBe("/work/repo");
    expect(session?.messages).toHaveLength(2);
    expect(session?.messages[0]).toMatchObject({ role: "user", text: "add the endpoint" });
    expect(session?.messages[1]).toMatchObject({ role: "assistant", text: "endpoint added" });
  });

  it("drops response_item user copies that duplicate event messages", () => {
    const contents = [
      rollout({ type: "session_meta", payload: { id: "t1", cwd: "/r" } }),
      rollout({
        type: "event_msg",
        payload: { type: "user_message", message: "the real prompt" },
        timestamp: "2026-10-11T10:00:00.000Z",
      }),
      rollout({
        type: "response_item",
        payload: {
          type: "message",
          role: "user",
          content: [{ type: "input_text", text: "the real prompt" }],
          internal_chat_message_metadata_passthrough: { turn_id: "turn-9" },
        },
        timestamp: "2026-10-11T10:00:00.000Z",
      }),
      rollout({
        type: "response_item",
        payload: {
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "ok" }],
        },
        timestamp: "2026-10-11T10:00:01.000Z",
      }),
    ].join("\n");

    const session = parseCodexTranscript(contents, base);
    expect(session?.messages.map((message) => message.text)).toEqual(["the real prompt", "ok"]);
  });

  it("returns null without a session id", () => {
    const contents = [
      rollout({
        type: "event_msg",
        payload: { type: "user_message", message: "hello" },
      }),
    ].join("\n");
    expect(parseCodexTranscript(contents, base)).toBeNull();
  });
});
