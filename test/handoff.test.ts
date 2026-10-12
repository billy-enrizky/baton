import { describe, expect, it } from "vitest";
import { buildHandoff, wrapPromptWithHandoff } from "../src/handoff.ts";

function message(role: "user" | "assistant", text: string) {
  return { role, provider: role === "assistant" ? "claude" : null, model: role === "assistant" ? "m" : null, text };
}

describe("buildHandoff", () => {
  it("includes project, conversation, and git state sections", () => {
    const handoff = buildHandoff({
      cwd: "/work/repo",
      threadTitle: "fix login",
      messages: [message("user", "fix the login bug"), message("assistant", "done, tests pass")],
      gitStatus: "M src/auth.ts",
    });
    expect(handoff).toContain("[baton context handoff]");
    expect(handoff).toContain("Working directory: /work/repo");
    expect(handoff).toContain("Title: fix login");
    expect(handoff).toContain("[user]\nfix the login bug");
    expect(handoff).toContain("[assistant (claude/m)]\ndone, tests pass");
    expect(handoff).toContain("M src/auth.ts");
    expect(handoff).toContain("[end handoff]");
  });

  it("keeps the newest messages within the budget and marks omissions", () => {
    const messages = Array.from({ length: 30 }, (_, index) =>
      message(index % 2 === 0 ? "user" : "assistant", `message number ${index} ${"x".repeat(200)}`),
    );
    const handoff = buildHandoff({
      cwd: "/r",
      threadTitle: "t",
      messages,
      gitStatus: null,
      charBudget: 2000,
    });
    expect(handoff).toContain("earlier messages omitted]");
    expect(handoff).not.toContain("message number 0 ");
    expect(handoff).toContain("message number 29 ");
  });

  it("truncates individual messages", () => {
    const handoff = buildHandoff({
      cwd: "/r",
      threadTitle: "t",
      messages: [message("assistant", "y".repeat(10_000))],
      gitStatus: null,
    });
    expect(handoff).toContain("[...truncated]");
    expect(handoff.length).toBeLessThan(6000);
  });

  it("shows clean tree when git status is missing", () => {
    const handoff = buildHandoff({
      cwd: "/r",
      threadTitle: "t",
      messages: [message("user", "hi")],
      gitStatus: null,
    });
    expect(handoff).toContain("clean");
  });
});

describe("wrapPromptWithHandoff", () => {
  it("prefixes the handoff before the user prompt", () => {
    const wrapped = wrapPromptWithHandoff("HANDOFF", "do the thing");
    expect(wrapped.startsWith("HANDOFF")).toBe(true);
    expect(wrapped.endsWith("do the thing")).toBe(true);
    expect(wrapped).toContain("---");
  });
});
