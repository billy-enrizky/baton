import { execFile } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

export function checkpointRef(threadId: string, turn: number): string {
  return `refs/baton/checkpoints/${threadId}/${turn}`;
}

async function git(cwd: string, args: string[], envExtra: Record<string, string> = {}): Promise<string> {
  const { stdout } = await execFileAsync("git", args, {
    cwd,
    maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, ...envExtra },
  });
  return stdout;
}

async function resolveHead(cwd: string): Promise<string> {
  try {
    return (await git(cwd, ["rev-parse", "--verify", "HEAD"])).trim();
  } catch {
    return EMPTY_TREE;
  }
}

export async function captureCheckpoint(cwd: string, threadId: string, turn: number): Promise<string> {
  const head = await resolveHead(cwd);
  const indexDir = mkdtempSync(join(tmpdir(), "baton-index-"));
  const indexPath = join(indexDir, "index");
  const indexEnv = { GIT_INDEX_FILE: indexPath };
  try {
    await git(cwd, ["read-tree", head], indexEnv);
    await git(cwd, ["add", "-A", "--", "."], indexEnv);
    const tree = (await git(cwd, ["write-tree"], indexEnv)).trim();
    const commitArgs = ["commit-tree", tree, "-m", `baton checkpoint ${threadId} turn ${turn}`];
    if (head !== EMPTY_TREE) {
      commitArgs.push("-p", head);
    }
    const commit = (
      await git(cwd, commitArgs, {
        GIT_AUTHOR_NAME: "baton",
        GIT_AUTHOR_EMAIL: "baton@local",
        GIT_COMMITTER_NAME: "baton",
        GIT_COMMITTER_EMAIL: "baton@local",
      })
    ).trim();
    const ref = checkpointRef(threadId, turn);
    await git(cwd, ["update-ref", ref, commit]);
    return ref;
  } finally {
    rmSync(indexDir, { recursive: true, force: true });
  }
}

export async function diffCheckpointAgainstWorktree(cwd: string, ref: string): Promise<string> {
  return git(cwd, ["--no-pager", "diff", ref]);
}

export async function diffCheckpoints(cwd: string, refA: string, refB: string): Promise<string> {
  return git(cwd, ["--no-pager", "diff", refA, refB]);
}

export async function restoreCheckpoint(cwd: string, ref: string): Promise<void> {
  await git(cwd, ["read-tree", "-u", "--reset", ref]);
}

export async function listCheckpointFiles(cwd: string, ref: string): Promise<string[]> {
  const stdout = await git(cwd, ["ls-tree", "-r", "--name-only", ref]);
  return stdout.split("\n").filter((line) => line.trim().length > 0);
}
