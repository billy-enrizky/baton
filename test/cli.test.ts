import { describe, expect, it } from "vitest";
import { parseCli } from "../src/cli.ts";

describe("parseCli", () => {
  it("defaults to chat in the current directory", () => {
    const parsed = parseCli([]);
    expect(parsed.kind).toBe("chat");
    if (parsed.kind === "chat") {
      expect(parsed.cwd).toBe(process.cwd());
      expect(parsed.threadId).toBeNull();
      expect(parsed.provider).toBeNull();
    }
  });

  it("parses flags in both forms", () => {
    const a = parseCli(["-C", "/work", "-t", "abc", "-p", "codex", "-m", "gpt-6.1-sol"]);
    const b = parseCli(["--cwd=/work", "--thread=abc", "--provider=codex", "--model=gpt-6.1-sol"]);
    for (const parsed of [a, b]) {
      expect(parsed.kind).toBe("chat");
      if (parsed.kind !== "chat") continue;
      expect(parsed.cwd).toBe("/work");
      expect(parsed.threadId).toBe("abc");
      expect(parsed.provider).toBe("codex");
      expect(parsed.model).toBe("gpt-6.1-sol");
    }
  });

  it("parses subcommands with positionals", () => {
    expect(parseCli(["new", "fix", "the", "login"])).toMatchObject({ kind: "new", title: "fix the login" });
    expect(parseCli(["threads", "-C", "/x"])).toMatchObject({ kind: "threads", cwd: "/x" });
    expect(parseCli(["import", "--limit=5", "--dry-run"])).toMatchObject({
      kind: "import",
      limit: 5,
      dryRun: true,
    });
    expect(parseCli(["checkpoints", "deadbeef"])).toMatchObject({ kind: "checkpoints", threadId: "deadbeef" });
    expect(parseCli(["diff", "deadbeef", "3"])).toMatchObject({ kind: "diff", threadId: "deadbeef", turn: 3 });
    expect(parseCli(["diff", "deadbeef"])).toMatchObject({ kind: "diff", turn: null });
    expect(parseCli(["restore", "deadbeef", "3"])).toMatchObject({ kind: "restore", turn: 3 });
    expect(parseCli(["usage"])).toMatchObject({ kind: "usage", threadId: null });
    expect(parseCli(["auth"])).toMatchObject({ kind: "auth", set: null });
    expect(parseCli(["auth", "codex", "api"])).toMatchObject({
      kind: "auth",
      set: { provider: "codex", mode: "api" },
    });
  });

  it("rejects bad input", () => {
    expect(() => parseCli(["frobnicate"])).toThrow("unknown command");
    expect(() => parseCli(["--provider", "grok"])).toThrow("unknown provider");
    expect(() => parseCli(["auth", "grok", "api"])).toThrow("unknown provider");
    expect(() => parseCli(["auth", "claude", "free"])).toThrow("unknown auth mode");
    expect(() => parseCli(["restore", "id"])).toThrow("turn number");
    expect(() => parseCli(["checkpoints"])).toThrow("thread id");
  });
});
