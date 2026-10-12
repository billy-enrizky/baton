import { describe, expect, it, vi } from "vitest";
import { CodexAppServerClient, CodexAdapter, type CodexConnection } from "../src/adapters/codex.ts";

function fakeConnection(): CodexConnection & { linesIn: string[]; emit: (line: string) => void } {
  const state = {
    linesIn: [] as string[],
    onLine: null as ((line: string) => void) | null,
    onClose: null as ((code: number | null) => void) | null,
  };
  const connection: CodexConnection & { linesIn: string[]; emit: (line: string) => void } = {
    write(line) {
      state.linesIn.push(line);
    },
    setOnLine(listener) {
      state.onLine = listener;
    },
    setOnClose(listener) {
      state.onClose = listener;
    },
    kill() {
      state.onClose?.(0);
    },
    linesIn: state.linesIn,
    emit(line) {
      state.onLine?.(line);
    },
  };
  return connection;
}

describe("CodexAppServerClient", () => {
  it("correlates responses to requests", async () => {
    const connection = fakeConnection();
    const client = new CodexAppServerClient(connection);
    const pending = client.request("initialize", { clientInfo: { name: "baton" } });
    const sent = JSON.parse(connection.linesIn[0] ?? "{}") as { id: number; method: string };
    expect(sent.method).toBe("initialize");
    connection.emit(JSON.stringify({ jsonrpc: "2.0", id: sent.id, result: { ok: true } }));
    await expect(pending).resolves.toEqual({ ok: true });
    client.dispose();
  });

  it("rejects on error responses carrying code and message", async () => {
    const connection = fakeConnection();
    const client = new CodexAppServerClient(connection);
    const pending = client.request("turn/start", {});
    const sent = JSON.parse(connection.linesIn[0] ?? "{}") as { id: number };
    connection.emit(
      JSON.stringify({ jsonrpc: "2.0", id: sent.id, error: { code: -32600, message: "Already initialized" } }),
    );
    await expect(pending).rejects.toThrow("Already initialized");
    client.dispose();
  });

  it("routes notifications and answers server requests", async () => {
    const connection = fakeConnection();
    const client = new CodexAppServerClient(connection);
    const notifications: Array<{ method: string; params: Record<string, unknown> }> = [];
    client.onNotification = (method, params) => notifications.push({ method, params });

    let approvalDecision = "deny";
    client.handleServerRequest("item/commandExecution/requestApproval", async (payload) => {
      expect(payload.command).toBe("rm -rf /");
      return { decision: approvalDecision };
    });

    connection.emit(
      JSON.stringify({ jsonrpc: "2.0", method: "item/started", params: { threadId: "t1" } }),
    );
    expect(notifications).toHaveLength(1);

    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 41,
        method: "item/commandExecution/requestApproval",
        params: { threadId: "t1", command: "rm -rf /" },
      }),
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
    const response = JSON.parse(connection.linesIn[connection.linesIn.length - 1] ?? "{}") as {
      id: number;
      result: { decision: string };
    };
    expect(response.id).toBe(41);
    expect(response.result.decision).toBe("deny");

    approvalDecision = "accept";
    client.dispose();
  });

  it("tolerates malformed lines", () => {
    const connection = fakeConnection();
    const client = new CodexAppServerClient(connection);
    expect(() => {
      connection.emit("not json");
      connection.emit(JSON.stringify(null));
    }).not.toThrow();
    client.dispose();
  });
});

describe("CodexAdapter turn lifecycle", () => {
  it("runs a full turn: thread/start, turn/start, events, completion with usage", async () => {
    const connection = fakeConnection();
    const adapter = new CodexAdapter(() => connection);
    const events: Array<{ type: string; [key: string]: unknown }> = [];

    const turnPromise = adapter.startTurn(
      {
        threadId: "baton-thread",
        cwd: "/work",
        model: "gpt-6.1-sol",
        prompt: "hello",
        permissionMode: "default",
        authEnv: {},
      },
      null,
      (event) => {
        events.push(event as { type: string; [key: string]: unknown });
      },
    );

    await vi.waitFor(() => {
      const initialize = connection.linesIn.find((line) => line.includes('"initialize"'));
      expect(initialize).toBeDefined();
    });
    const initializeFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"initialize"')) ?? "{}",
    ) as { id: number };
    connection.emit(JSON.stringify({ jsonrpc: "2.0", id: initializeFrame.id, result: {} }));
    connection.emit(JSON.stringify({ jsonrpc: "2.0", method: "initialized" }));

    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"thread/start"'))).toBe(true);
    });
    const threadStartFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"thread/start"')) ?? "{}",
    ) as { id: number; params: { cwd: string; model: string } };
    expect(threadStartFrame.params.cwd).toBe("/work");
    expect(threadStartFrame.params.model).toBe("gpt-6.1-sol");
    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        id: threadStartFrame.id,
        result: { thread: { id: "native-t1", path: "/home/.codex/sessions/rollout.jsonl" } },
      }),
    );

    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"turn/start"'))).toBe(true);
    });
    const turnStartFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"turn/start"')) ?? "{}",
    ) as { id: number; params: { threadId: string; input: Array<{ text: string }> } };
    expect(turnStartFrame.params.threadId).toBe("native-t1");
    expect(turnStartFrame.params.input[0]?.text).toBe("hello");

    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "thread/tokenUsage/updated",
        params: { threadId: "native-t1", tokenUsage: { inputTokens: 50, cachedInputTokens: 500, cacheWriteInputTokens: 100, outputTokens: 20 } },
      }),
    );
    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "item/agentMessage/delta",
        params: { threadId: "native-t1", delta: "hel" },
      }),
    );
    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "item/agentMessage/delta",
        params: { threadId: "native-t1", delta: "lo" },
      }),
    );
    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "turn/completed",
        params: {
          threadId: "native-t1",
          turn: { items: [{ type: "agentMessage", text: "hello there" }] },
        },
      }),
    );

    await turnPromise;
    const completed = events.find((event) => event.type === "turn-completed") as
      | { type: string; finalText: string; nativeSessionId: string; rolloutPath: string; usage: { cacheReadTokens: number } }
      | undefined;
    expect(completed?.finalText).toBe("hello there");
    expect(completed?.nativeSessionId).toBe("native-t1");
    expect(completed?.rolloutPath).toBe("/home/.codex/sessions/rollout.jsonl");
    expect(completed?.usage.cacheReadTokens).toBe(500);
    const deltas = events.filter((event) => event.type === "text-delta") as Array<{ text: string }>;
    expect(deltas.map((delta) => delta.text).join("")).toBe("hello");
  });

  it("resumes an existing native session instead of starting a thread", async () => {
    const connection = fakeConnection();
    const adapter = new CodexAdapter(() => connection);
    const turnPromise = adapter.startTurn(
      {
        threadId: "bt",
        cwd: "/work",
        model: "m",
        prompt: "continue",
        permissionMode: "bypassPermissions",
        authEnv: {},
      },
      { nativeSessionId: "native-existing", rolloutPath: null },
      () => undefined,
    );

    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"initialize"'))).toBe(true);
    });
    const initializeFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"initialize"')) ?? "{}",
    ) as { id: number };
    connection.emit(JSON.stringify({ jsonrpc: "2.0", id: initializeFrame.id, result: {} }));

    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"thread/resume"'))).toBe(true);
    });
    const resumeFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"thread/resume"')) ?? "{}",
    ) as { id: number; params: { threadId: string; excludeTurns: boolean } };
    expect(resumeFrame.params.threadId).toBe("native-existing");
    expect(resumeFrame.params.excludeTurns).toBe(true);
    connection.emit(JSON.stringify({ jsonrpc: "2.0", id: resumeFrame.id, result: {} }));

    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"turn/start"'))).toBe(true);
    });
    const turnStartFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"turn/start"')) ?? "{}",
    ) as { params: { approvalPolicy: string; sandboxPolicy: { type: string } } };
    expect(turnStartFrame.params.approvalPolicy).toBe("never");
    expect(turnStartFrame.params.sandboxPolicy.type).toBe("dangerFullAccess");

    connection.emit(
      JSON.stringify({ jsonrpc: "2.0", method: "turn/completed", params: { threadId: "native-existing", turn: { items: [] } } }),
    );
    await turnPromise;
  });

  it("surfaces approval requests and maps acceptAlways to acceptForSession", async () => {
    const connection = fakeConnection();
    const adapter = new CodexAdapter(() => connection);
    const turnPromise = adapter.startTurn(
      { threadId: "bt", cwd: "/w", model: "m", prompt: "p", permissionMode: "default", authEnv: {} },
      { nativeSessionId: "native-x", rolloutPath: null },
      (event) => {
        if (event.type === "approval-request") {
          void event.respond("acceptAlways");
        }
      },
    );
    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"initialize"'))).toBe(true);
    });
    const initializeFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"initialize"')) ?? "{}",
    ) as { id: number };
    connection.emit(JSON.stringify({ jsonrpc: "2.0", id: initializeFrame.id, result: {} }));
    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"thread/resume"'))).toBe(true);
    });
    const resumeFrame = JSON.parse(
      connection.linesIn.find((line) => line.includes('"thread/resume"')) ?? "{}",
    ) as { id: number };
    connection.emit(JSON.stringify({ jsonrpc: "2.0", id: resumeFrame.id, result: {} }));
    await vi.waitFor(() => {
      expect(connection.linesIn.some((line) => line.includes('"turn/start"'))).toBe(true);
    });

    connection.emit(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 77,
        method: "item/commandExecution/requestApproval",
        params: { threadId: "native-x", command: "cargo test" },
      }),
    );

    await vi.waitFor(() => {
      const response = connection.linesIn
        .map((line) => JSON.parse(line) as { id?: number; result?: { decision?: string } })
        .find((frame) => frame.id === 77);
      expect(response?.result?.decision).toBe("acceptForSession");
    });

    connection.emit(
      JSON.stringify({ jsonrpc: "2.0", method: "turn/completed", params: { threadId: "native-x", turn: { items: [] } } }),
    );
    await turnPromise;
  });
});
