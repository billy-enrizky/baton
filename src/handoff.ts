import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { MessageRow } from "./store.ts";

const execFileAsync = promisify(execFile);

export const DEFAULT_HANDOFF_CHAR_BUDGET = 24000;
const PER_MESSAGE_CHAR_LIMIT = 4000;

export interface HandoffInput {
  cwd: string;
  threadTitle: string;
  messages: ReadonlyArray<Pick<MessageRow, "role" | "provider" | "model" | "text">>;
  gitStatus: string | null;
  charBudget?: number;
}

export function buildHandoff(input: HandoffInput): string {
  const budget = input.charBudget ?? DEFAULT_HANDOFF_CHAR_BUDGET;
  const sections: string[] = [];
  sections.push("[baton context handoff]");
  sections.push(
    "You are continuing an existing task in a new harness. The prior conversation below is your context. Pick up where it left off.",
  );
  sections.push("");
  sections.push("## Project");
  sections.push(`Title: ${input.threadTitle}`);
  sections.push(`Working directory: ${input.cwd}`);
  sections.push("");
  sections.push("## Prior conversation (oldest first)");
  const rendered = renderMessages(input.messages, budget - estimateRenderedLength(sections, input));
  if (rendered.length === 0) {
    sections.push("(no prior messages)");
  } else {
    sections.push(rendered);
  }
  sections.push("");
  sections.push("## Working tree state");
  if (input.gitStatus && input.gitStatus.trim().length > 0) {
    sections.push(input.gitStatus.trim());
  } else {
    sections.push("clean");
  }
  sections.push("");
  sections.push("[end handoff]");
  return sections.join("\n");
}

export function wrapPromptWithHandoff(handoff: string, prompt: string): string {
  return `${handoff}\n\n---\n\n${prompt}`;
}

function estimateRenderedLength(sections: string[], input: HandoffInput): number {
  const fixed = sections.join("\n").length + 200;
  const gitStatusLength = input.gitStatus?.length ?? 0;
  return fixed + gitStatusLength;
}

function renderMessages(
  messages: ReadonlyArray<Pick<MessageRow, "role" | "provider" | "model" | "text">>,
  budget: number,
): string {
  if (budget <= 0) return "";
  const lines: string[] = [];
  let used = 0;
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (!message) continue;
    let text = message.text;
    if (text.length > PER_MESSAGE_CHAR_LIMIT) {
      text = `${text.slice(0, PER_MESSAGE_CHAR_LIMIT)}\n[...truncated]`;
    }
    const speaker =
      message.role === "user"
        ? "user"
        : `assistant (${message.provider ?? "?"}/${message.model ?? "?"})`;
    const block = `[${speaker}]\n${text}`;
    if (used + block.length > budget && lines.length > 0) break;
    lines.unshift(block);
    used += block.length + 1;
  }
  if (lines.length < messages.length) {
    lines.unshift(`[...${messages.length - lines.length} earlier messages omitted]`);
  }
  return lines.join("\n\n");
}

export async function gitStatusSummary(cwd: string, maxLines = 50): Promise<string | null> {
  try {
    const { stdout } = await execFileAsync("git", ["status", "--porcelain", "--branch"], {
      cwd,
      maxBuffer: 1024 * 1024,
    });
    const lines = stdout.split("\n").filter((line) => line.trim().length > 0);
    return lines.slice(0, maxLines).join("\n");
  } catch {
    return null;
  }
}
