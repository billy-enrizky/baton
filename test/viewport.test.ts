import { describe, expect, it } from "vitest";
import { clampOffset, messageHeight, windowMessages, wrappedRows } from "../src/tui/viewport.ts";

function item(id: number, text: string, role: "user" | "assistant" = "assistant") {
  return { id, role, text, hasLabel: role === "assistant", hasUsage: false };
}

describe("wrappedRows", () => {
  it("counts wrapped and explicit lines", () => {
    expect(wrappedRows("hello", 80)).toBe(1);
    expect(wrappedRows("x".repeat(78), 80)).toBe(1);
    expect(wrappedRows("x".repeat(79), 80)).toBe(2);
    expect(wrappedRows("a\nb\nc", 80)).toBe(3);
    expect(wrappedRows("", 80)).toBe(1);
    expect(wrappedRows("x".repeat(20), 12)).toBe(2);
  });
});

describe("messageHeight", () => {
  it("includes margin, label, and usage rows", () => {
    expect(messageHeight(item(1, "hi"), 80)).toBe(3);
    expect(messageHeight({ ...item(1, "hi"), hasUsage: true }, 80)).toBe(4);
    expect(messageHeight({ ...item(1, "hi"), hasLabel: false, role: "user" }, 80)).toBe(2);
  });
});

describe("windowMessages", () => {
  it("shows the newest messages that fit the budget", () => {
    const messages = [item(1, "one"), item(2, "two"), item(3, "three"), item(4, "four")];
    const windowed = windowMessages(messages, { budgetRows: 6, cols: 80, scrollOffset: 0 });
    expect(windowed.visible.map((message) => message.id)).toEqual([3, 4]);
    expect(windowed.hiddenAbove).toBe(2);
    expect(windowed.hiddenBelow).toBe(0);
  });

  it("always shows at least one message when budget allows", () => {
    const messages = [item(1, "one"), item(2, "two")];
    const windowed = windowMessages(messages, { budgetRows: 1, cols: 80, scrollOffset: 0 });
    expect(windowed.visible.map((message) => message.id)).toEqual([2]);
  });

  it("respects scroll offset from the bottom", () => {
    const messages = [item(1, "one"), item(2, "two"), item(3, "three"), item(4, "four")];
    const windowed = windowMessages(messages, { budgetRows: 6, cols: 80, scrollOffset: 2 });
    expect(windowed.visible.map((message) => message.id)).toEqual([1, 2]);
    expect(windowed.hiddenBelow).toBe(2);
    expect(windowed.hiddenAbove).toBe(0);
  });

  it("clamps offset beyond the message count", () => {
    const messages = [item(1, "one"), item(2, "two")];
    const windowed = windowMessages(messages, { budgetRows: 4, cols: 80, scrollOffset: 99 });
    expect(windowed.visible.map((message) => message.id)).toEqual([1]);
  });

  it("returns empty for zero budget", () => {
    const messages = [item(1, "one")];
    const windowed = windowMessages(messages, { budgetRows: 0, cols: 80, scrollOffset: 0 });
    expect(windowed.visible).toEqual([]);
    expect(windowed.hiddenAbove).toBe(1);
  });

  it("accounts for wrapped rows in the budget", () => {
    const messages = [item(1, "x".repeat(200)), item(2, "short")];
    const windowed = windowMessages(messages, { budgetRows: 3, cols: 80, scrollOffset: 0 });
    expect(windowed.visible.map((message) => message.id)).toEqual([2]);
  });
});

describe("clampOffset", () => {
  it("keeps offset within message bounds", () => {
    expect(clampOffset(-3, 5)).toBe(0);
    expect(clampOffset(2, 5)).toBe(2);
    expect(clampOffset(99, 5)).toBe(4);
    expect(clampOffset(0, 0)).toBe(0);
    expect(clampOffset(1, 1)).toBe(0);
  });
});
