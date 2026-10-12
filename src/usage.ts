import type { TurnUsage } from "./types.ts";
import type { UsageRow } from "./store.ts";

export function cacheHitRate(usage: Pick<TurnUsage, "inputTokens" | "cacheReadTokens" | "cacheCreationTokens">): number | null {
  const total = usage.cacheReadTokens + usage.cacheCreationTokens + usage.inputTokens;
  if (total <= 0) return null;
  return usage.cacheReadTokens / total;
}

export interface UsageTotals {
  turns: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number | null;
}

export function sumUsage(rows: ReadonlyArray<UsageRow>): UsageTotals {
  const totals: UsageTotals = {
    turns: rows.length,
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheCreationTokens: 0,
    costUsd: null,
  };
  let sawCost = false;
  let costSum = 0;
  for (const row of rows) {
    totals.inputTokens += row.inputTokens;
    totals.outputTokens += row.outputTokens;
    totals.cacheReadTokens += row.cacheReadTokens;
    totals.cacheCreationTokens += row.cacheCreationTokens;
    if (row.costUsd !== null) {
      sawCost = true;
      costSum += row.costUsd;
    }
  }
  if (sawCost) totals.costUsd = Math.round(costSum * 1e6) / 1e6;
  return totals;
}

export function formatTokenCount(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return String(tokens);
}

export function formatUsageLine(totals: UsageTotals): string {
  const rate = cacheHitRate(totals);
  const cachePart = rate === null ? "cache n/a" : `cache ${(rate * 100).toFixed(0)}%`;
  const costPart = totals.costUsd === null ? "" : `, $${totals.costUsd.toFixed(2)}`;
  return `in ${formatTokenCount(totals.inputTokens + totals.cacheReadTokens + totals.cacheCreationTokens)}, out ${formatTokenCount(totals.outputTokens)}, ${cachePart}${costPart}`;
}
