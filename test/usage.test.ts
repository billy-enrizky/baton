import { describe, expect, it } from "vitest";
import { cacheHitRate, formatTokenCount, formatUsageLine, sumUsage } from "../src/usage.ts";
import type { UsageRow } from "../src/store.ts";

function row(overrides: Partial<UsageRow>): UsageRow {
  return {
    id: 1,
    threadId: "t",
    turn: 1,
    provider: "claude",
    model: null,
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheCreationTokens: 0,
    costUsd: null,
    createdAt: new Date().toISOString(),
    ...overrides,
  };
}

describe("cacheHitRate", () => {
  it("computes cache reads over all input tokens", () => {
    expect(cacheHitRate({ inputTokens: 100, cacheReadTokens: 900, cacheCreationTokens: 0 })).toBeCloseTo(0.9);
    expect(cacheHitRate({ inputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0 })).toBeNull();
  });

  it("counts cache creation tokens in the denominator", () => {
    expect(cacheHitRate({ inputTokens: 0, cacheReadTokens: 300, cacheCreationTokens: 100 })).toBeCloseTo(0.75);
  });
});

describe("sumUsage", () => {
  it("aggregates rows and costs", () => {
    const totals = sumUsage([
      row({ inputTokens: 10, outputTokens: 5, cacheReadTokens: 100, cacheCreationTokens: 20, costUsd: 0.01 }),
      row({ inputTokens: 1, outputTokens: 2, cacheReadTokens: 200, cacheCreationTokens: 30, costUsd: 0.02 }),
    ]);
    expect(totals.turns).toBe(2);
    expect(totals.inputTokens).toBe(11);
    expect(totals.outputTokens).toBe(7);
    expect(totals.cacheReadTokens).toBe(300);
    expect(totals.cacheCreationTokens).toBe(50);
    expect(totals.costUsd).toBeCloseTo(0.03);
  });

  it("leaves cost null when no row reported it", () => {
    expect(sumUsage([row({})]).costUsd).toBeNull();
  });
});

describe("formatting", () => {
  it("formats token counts at human scales", () => {
    expect(formatTokenCount(999)).toBe("999");
    expect(formatTokenCount(1500)).toBe("1.5k");
    expect(formatTokenCount(2_500_000)).toBe("2.5M");
  });

  it("formats a usage line with cache rate and cost", () => {
    const line = formatUsageLine(
      sumUsage([row({ inputTokens: 100, outputTokens: 50, cacheReadTokens: 900, cacheCreationTokens: 0, costUsd: 0.05 })]),
    );
    expect(line).toContain("1.0k");
    expect(line).toContain("cache 90%");
    expect(line).toContain("$0.05");
  });

  it("shows cache n/a when there is no usage", () => {
    expect(formatUsageLine(sumUsage([row({})]))).toContain("cache n/a");
  });
});
