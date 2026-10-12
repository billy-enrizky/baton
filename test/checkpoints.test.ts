import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  captureCheckpoint,
  checkpointRef,
  diffCheckpointAgainstWorktree,
  restoreCheckpoint,
} from "../src/checkpoints.ts";

let repo: string;

function git(args: string[]): string {
  return execFileSync("git", args, { cwd: repo, encoding: "utf8" }).trim();
}

beforeEach(() => {
  repo = mkdtempSync(join(tmpdir(), "baton-cp-"));
  git(["init", "-q"]);
  git(["config", "user.email", "test@test"]);
  git(["config", "user.name", "test"]);
  writeFileSync(join(repo, "a.txt"), "initial\n");
  git(["add", "."]);
  git(["commit", "-q", "-m", "init"]);
});

afterEach(() => {
  rmSync(repo, { recursive: true, force: true });
});

describe("checkpoints", () => {
  it("captures the working tree without touching HEAD or the index", async () => {
    writeFileSync(join(repo, "a.txt"), "changed\n");
    writeFileSync(join(repo, "b.txt"), "new file\n");
    const headBefore = git(["rev-parse", "HEAD"]);
    const statusBefore = git(["status", "--porcelain"]);

    const ref = await captureCheckpoint(repo, "thread-1", 1);
    expect(ref).toBe(checkpointRef("thread-1", 1));
    expect(git(["rev-parse", "HEAD"])).toBe(headBefore);
    expect(git(["status", "--porcelain"])).toBe(statusBefore);
    expect(git(["cat-file", "-p", `${ref}:a.txt`])).toBe("changed");
    expect(git(["cat-file", "-p", `${ref}:b.txt`])).toBe("new file");
  });

  it("diffs a checkpoint against the working tree", async () => {
    writeFileSync(join(repo, "a.txt"), "version 1\n");
    await captureCheckpoint(repo, "thread-1", 1);
    writeFileSync(join(repo, "a.txt"), "version 2\n");

    const diff = await diffCheckpointAgainstWorktree(repo, checkpointRef("thread-1", 1));
    expect(diff).toContain("-version 1");
    expect(diff).toContain("+version 2");
  });

  it("captures checkpoints in an empty repo without commits", async () => {
    const empty = mkdtempSync(join(tmpdir(), "baton-empty-"));
    try {
      execFileSync("git", ["init", "-q"], { cwd: empty });
      writeFileSync(join(empty, "only.txt"), "content\n");
      const ref = await captureCheckpoint(empty, "thread-empty", 1);
      expect(ref).toBe(checkpointRef("thread-empty", 1));
    } finally {
      rmSync(empty, { recursive: true, force: true });
    }
  });

  it("restores the working tree to a checkpoint", async () => {
    writeFileSync(join(repo, "a.txt"), "keep this\n");
    await captureCheckpoint(repo, "thread-1", 1);
    writeFileSync(join(repo, "a.txt"), "discard this\n");
    writeFileSync(join(repo, "extra.txt"), "gone after restore\n");
    await captureCheckpoint(repo, "thread-1", 2);

    await restoreCheckpoint(repo, checkpointRef("thread-1", 1));
    expect(execFileSync("cat", [join(repo, "a.txt")], { encoding: "utf8" })).toBe("keep this\n");
  });
});
