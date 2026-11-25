# Baton

Baton is a terminal workspace for developers who use command-line coding agents and want one conversation that can switch between them, split work across them, and keep going when one hits a wall.

A harness is the coding agent CLI you already use, such as Codex, Claude Code, Cursor, Grok, OpenCode, or Antigravity. Baton drives the harness and keeps the thread, tools, and git history around it. It does not replace the harness or its subscription.

## What it can do

- Keep one conversation while switching harness or model.
- Split a goal into independent tasks and run each on the harness that fits.
- Give every worker its own git worktree and branch.
- Review each worker's commit before it can be merged.
- Rewind a turn, restore a checkpoint, retry failed work, and inspect usage.
- Commit, push, open, review, and merge pull requests from the terminal.
- Read project files, search contents, and attach file mentions with `@`.
- Use Claude and Codex memory files, skills, MCP servers, and hooks.
- Expose durable state as JSON and over MCP for other agents.
- Run on a harness login or an API key, with no Baton account.

## Table of contents

1. [What Baton is](#1-what-baton-is)
   1. [The problem it solves](#11-the-problem-it-solves)
   2. [Who it is for](#12-who-it-is-for)
   3. [What a harness is](#13-what-a-harness-is)
   4. [What Baton is not](#14-what-baton-is-not)
2. [What you can do](#2-what-you-can-do)
   1. [One conversation, many harnesses](#21-one-conversation-many-harnesses)
   2. [Parallel work with a reviewer gate](#22-parallel-work-with-a-reviewer-gate)
   3. [Recovery and review](#23-recovery-and-review)
   4. [A complete git loop](#24-a-complete-git-loop)
   5. [Agent-readable state](#25-agent-readable-state)
3. [Install](#3-install)
   1. [One-line install](#31-one-line-install)
   2. [Homebrew](#32-homebrew)
   3. [From source](#33-from-source)
   4. [What else you need](#34-what-else-you-need)
   5. [First-run checklist](#35-first-run-checklist)
4. [Start a session](#4-start-a-session)
   1. [Fresh session](#41-fresh-session)
   2. [Single-harness mode](#42-single-harness-mode)
   3. [Orchestrate mode](#43-orchestrate-mode)
   4. [Open an existing thread](#44-open-an-existing-thread)
   5. [Your first prompt](#45-your-first-prompt)
   6. [Help inside the app](#46-help-inside-the-app)
5. [Core concepts](#5-core-concepts)
   1. [Harness](#51-harness)
   2. [Thread](#52-thread)
   3. [Turn](#53-turn)
   4. [Session mode](#54-session-mode)
   5. [Worktree](#55-worktree)
   6. [Run](#56-run)
   7. [Role](#57-role)
   8. [Checkpoint](#58-checkpoint)
   9. [Native session and handoff](#59-native-session-and-handoff)
6. [The terminal interface](#6-the-terminal-interface)
   1. [Status bar](#61-status-bar)
   2. [Composer](#62-composer)
   3. [Pickers and previews](#63-pickers-and-previews)
   4. [Transcript](#64-transcript)
   5. [Keyboard](#65-keyboard)
      1. [Send and newline](#651-send-and-newline)
      2. [Interrupt and exit](#652-interrupt-and-exit)
      3. [Select and copy](#653-select-and-copy)
      4. [Steering and queueing](#654-steering-and-queueing)
7. [Slash commands](#7-slash-commands)
   1. [How commands behave](#71-how-commands-behave)
   2. [Session and model](#72-session-and-model)
   3. [Navigation and project](#73-navigation-and-project)
   4. [Review and recovery](#74-review-and-recovery)
   5. [Orchestration](#75-orchestration)
   6. [Git](#76-git)
   7. [Memory, skills, and settings](#77-memory-skills-and-settings)
8. [Orchestration](#8-orchestration)
   1. [Plan](#81-plan)
   2. [Workers](#82-workers)
   3. [Reviewer](#83-reviewer)
   4. [Integrate](#84-integrate)
   5. [Ship](#85-ship)
   6. [Follow-ups](#86-follow-ups)
   7. [Roles and hooks](#87-roles-and-hooks)
   8. [Shared directories](#88-shared-directories)
   9. [CLI examples](#89-cli-examples)
9. [Git delivery loop](#9-git-delivery-loop)
   1. [Status, commit, push](#91-status-commit-push)
   2. [Checks](#92-checks)
   3. [Open a pull request](#93-open-a-pull-request)
   4. [Review a pull request](#94-review-a-pull-request)
   5. [Merge a pull request](#95-merge-a-pull-request)
   6. [Safety rules](#96-safety-rules)
10. [Memory and skills](#10-memory-and-skills)
    1. [Memory files](#101-memory-files)
    2. [Skills](#102-skills)
    3. [Governed memories](#103-governed-memories)
11. [MCP for other agents](#11-mcp-for-other-agents)
    1. [Configure](#111-configure)
    2. [Tools](#112-tools)
    3. [JSON streams](#113-json-streams)
12. [Command-line reference](#12-command-line-reference)
    1. [Sessions and inspection](#121-sessions-and-inspection)
    2. [Turns and execution](#122-turns-and-execution)
    3. [Orchestration](#123-orchestration)
    4. [Data and maintenance](#124-data-and-maintenance)
13. [Configuration and storage](#13-configuration-and-storage)
    1. [Files](#131-files)
    2. [BATON_HOME](#132-baton_home)
    3. [Permission modes](#133-permission-modes)
    4. [Roles](#134-roles)
    5. [Harness config](#135-harness-config)
14. [Safety and recovery](#14-safety-and-recovery)
    1. [Approvals and permissions](#141-approvals-and-permissions)
    2. [Processes](#142-processes)
    3. [Worktrees](#143-worktrees)
    4. [Privacy](#144-privacy)
15. [Troubleshooting](#15-troubleshooting)
    1. [doctor](#151-doctor)
    2. [Common problems](#152-common-problems)
    3. [Getting more detail](#153-getting-more-detail)
16. [Development and verification](#16-development-and-verification)
    1. [Build and test](#161-build-and-test)
    2. [Verification](#162-verification)
17. [License](#17-license)

## 1. What Baton is

### 1.1 The problem it solves

Coding agents are useful alone and awkward together. Each CLI has its own session, its own model list, its own authentication, and its own idea of a working directory. When one agent gets stuck, you copy context to another. When several could work in parallel, you manage branches and terminals by hand.

Baton puts one terminal in front of all of them. The conversation, the repository, and the work history stay in one place while you change which harness is doing the work.

### 1.2 Who it is for

- Developers who already use a coding agent CLI and want a faster way to switch models or harnesses.
- People who want to run several agents on one goal without merging terminal tabs and branches by hand.
- Teams that want a reviewer pass and a normal git flow before agent work lands.
- Anyone who wants durable, inspectable state instead of a chat that disappears with the terminal.

### 1.3 What a harness is

In this documentation, a harness is a command-line coding agent. The supported harnesses are:

| Harness | What it is |
|---|---|
| Codex | OpenAI's coding agent CLI |
| Claude Code | Anthropic's coding agent CLI |
| Cursor | Cursor's terminal coding agent |
| Grok | xAI's Grok coding agent |
| OpenCode | The open-source coding agent |
| Antigravity | Google's Antigravity agent |

Baton starts the harness, sends the prompt, reads its structured events, and records the turn. The harness still uses its own subscription or API key.

### 1.4 What Baton is not

- Not a hosted service. There is no Baton account, no telemetry, and no Baton model.
- Not a replacement for a harness. You install and authenticate the CLIs you want to use.
- Not a wrapper that hides the provider. Every turn is recorded, and failures are shown with the harness error.
- Not a force-push tool. Baton never force-pushes, rewrites your source history, or deletes work without an explicit command.

## 2. What you can do

### 2.1 One conversation, many harnesses

Use `/switch` to change harness, then pick a model. The thread stays the same. The first turn on a new harness gets a bounded handoff built from the conversation and the working tree; after that, the harness resumes its own native session.

### 2.2 Parallel work with a reviewer gate

In orchestrate mode, one prompt becomes a plan. Independent tasks run in separate git worktrees and branches, each on the harness that fits. A reviewer reads every completed commit. Only an approved commit can be integrated or shipped.

### 2.3 Recovery and review

Every editing turn leaves a checkpoint. `/rewind` restores the conversation and optionally the code. `/retry` retries a failed or unverified turn on the same harness. `/review` shows the turn diff and sends line notes back as a steer or the next prompt.

### 2.4 A complete git loop

`/git` covers status, commit, push, pull request checks, opening a pull request, approving or requesting changes, and squash, merge-commit, or rebase merges. Every remote action has a preview and a recheck before it runs.

### 2.5 Agent-readable state

`baton status`, `threads`, `turns`, `queue`, `activity`, `agents`, `exec`, and `orchestrate` offer JSON output. `exec --stream-json` and `orchestrate --stream-json` emit one event per line. `baton mcp` exposes the same store over MCP stdio for other agents.

## 3. Install

### 3.1 One-line install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/billy-enrizky/baton/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/billy-enrizky/baton/main/install.ps1 | iex
```

The installer downloads the release for your OS and checks its SHA-256. If no release asset matches, it falls back to a source build when Cargo is installed.

### 3.2 Homebrew

```sh
brew install billy-enrizky/baton/baton
```

### 3.3 From source

```sh
git clone https://github.com/billy-enrizky/baton
cd baton
cargo install --path . --locked
baton
```

### 3.4 What else you need

- At least one harness CLI on `PATH`.
- Git, for worktrees and the git commands.
- `gh`, for pull request checks, creation, review, and merge.
- Optional: `rg` for faster file search, and `VISUAL` or `EDITOR` for opening files.
- Optional: Node or any other runtime your harness or MCP servers need.

See [Install Baton](install.md) for supported platforms, checksums, upgrades, and uninstall.

### 3.5 First-run checklist

1. Install Baton.
2. Install and log in to one harness, for example `codex login` or `baton login codex`.
3. Run `baton doctor` to confirm the harness is ready.
4. Run `baton` inside a git repository and send a prompt.

## 4. Start a session

### 4.1 Fresh session

```sh
baton
```

The workspace opens in the configured default session mode. Orchestrate is the default until you change it with `/settings`, `/orchestrate off`, or `"defaultMode": "single"` in `config.json`.

### 4.2 Single-harness mode

```sh
baton --mode single
```

One harness handles every turn. Use `/switch` to change it and `/model` to change the model.

### 4.3 Orchestrate mode

Orchestrate is the default. A composer prompt becomes a goal, a planner splits it, and workers run in separate worktrees. Use a bare `/orchestrate` to switch modes or open a plan-only goal.

### 4.4 Open an existing thread

```sh
baton -t <thread-id>
baton threads
```

`baton threads` lists thread ids, phases, goals, and queue counts. `baton -t` opens one and keeps its saved session mode.

### 4.5 Your first prompt

Type what you want in plain language. Examples:

- "Add a retry to the upload client and test it."
- "Audit the billing settings, add the missing tests, and update the docs."
- "Explain how the cache invalidation works."

The harness choice and task split are visible before work starts. Press Enter to accept a preview or Esc to edit.

### 4.6 Help inside the app

- `/help` shows the common paths.
- `/help all` opens a scrollable list of every command and its syntax.
- `baton doctor` checks what is installed and what to do next.
- `/status` shows the current thread, phase, goal, queue, and usage.

## 5. Core concepts

### 5.1 Harness

The coding agent CLI that performs a turn. Baton can switch harnesses inside one thread.

### 5.2 Thread

A durable conversation with an id, a working directory, a harness, a model, a session mode, and a transcript. Threads survive restarts.

### 5.3 Turn

One prompt and the agent's response, including tool calls, a diff, usage, and a phase such as running, completed, failed, or unverified.

### 5.4 Session mode

`single` sends each prompt to one harness. `orchestrate` sends a goal to a planner first. The mode belongs to the thread.

### 5.5 Worktree

A separate checkout of the repository on its own branch. Orchestration workers each get one, so parallel edits do not collide.

### 5.6 Run

One orchestration attempt. A run has a plan, workers, commits, review verdicts, and a final status such as completed, partial, failed, cancelled, needs_input, or needs_review.

### 5.7 Role

A named harness, model, effort, markdown prompt, and optional hooks. Baton has four roles: orchestrator, planner, implementor, and reviewer.

### 5.8 Checkpoint

A hidden git ref that captures the working tree at a turn boundary. It powers `/rewind`, `/review`, and `/diff`. A turn that changes nothing does not write one.

### 5.9 Native session and handoff

Each harness keeps its own native session per Baton thread. Later turns send only the new message, which keeps provider prompt caches warm. The first turn on a harness with no session gets a handoff built from prior messages and the working tree, capped at 24,000 bytes.

## 6. The terminal interface

### 6.1 Status bar

The top of the screen shows the Baton version, working directory, session mode, current harness and model, reasoning effort when set, permission mode, and billing source. A queue, goal, or stale-turn warning appears when relevant.

### 6.2 Composer

The composer is where prompts, slash commands, and file mentions are typed. `@` followed by part of a project path offers ranked files. Tab or Enter replaces only the mention. Ordinary text and addresses such as `a@b` never start a file search.

### 6.3 Pickers and previews

Most commands open a picker instead of requiring memorized syntax. Pickers show their keys, the current selection, and the relevant detail. Remote actions open a preview first and recheck the live state before they run.

### 6.4 Transcript

The transcript streams Markdown with tables, aligned columns, tinted diffs, and one row per tool call. Running tools show a status; completed and failed tools keep their result.

### 6.5 Keyboard

#### 6.5.1 Send and newline

Enter sends. Shift+Enter or Alt+Enter adds a newline. Pasting a multi-line prompt keeps it in the composer.

#### 6.5.2 Interrupt and exit

Ctrl+C interrupts a running turn or cancels an orchestration, including its worker process groups. Press it twice when idle to exit. Esc closes a picker or preview.

#### 6.5.3 Select and copy

Drag in the transcript to select text across wrapped rows. `/copy` copies the latest assistant reply. Pickers and previews copy their selection with `c`.

#### 6.5.4 Steering and queueing

While a turn is running, a new composer message is sent as a steer on Codex, Claude, and OpenCode. Other harnesses queue it durably. A bare `/queue` opens the queue: Enter sends, `e` edits, `d` deletes, `c` twice clears, and Ctrl+Up/Ctrl+Down reorders.

## 7. Slash commands

### 7.1 How commands behave

Type `/` to see commands. Typing a command followed by a space always answers: it lists valid choices, shows the argument syntax, or says the command takes no arguments. Partial text filters the choices; Tab completes; Enter runs a complete command. The direct forms remain available for scripts.

### 7.2 Session and model

| Command | What it does |
|---|---|
| `/switch` | Pick a harness, then a model, for this thread |
| `/model` | Pick a model for the current harness |
| `/effort` | Pick a reasoning effort; `/effort default` clears it |
| `/mode` | Pick `default`, `acceptEdits`, `bypassPermissions`, or `plan` |
| `/auth` | Pick a billing mode for any harness; `/auth codex` scopes it |
| `/orchestrate` | Switch modes, start a plan-only goal, or open recent runs |
| `/goal` | Set, pause, resume, or clear the thread goal |
| `/settings` | Set the default session mode and terminal bell |
| `/roles` | Inspect the four roles and their prompts |

### 7.3 Navigation and project

| Command | What it does |
|---|---|
| `/threads`, `/open`, `/pin`, `/inbox` | List, open, pin, or triage threads |
| `/new`, `/fork`, `/worktree`, `/task` | Start a thread, fork one, or start from a git worktree or GitHub issue |
| `/files` | Pick a project file and insert its path, optionally with `:line` |
| `/grep` | Search project contents and insert `path:line` |
| `/find` | Search the conversation |
| `/activity`, `/commands`, `/agents` | Inspect turns, tool calls, and subagent calls |
| `/usage`, `/context`, `/status`, `/turns` | Inspect usage, context, status, and turn history |
| `/memory` | Open Claude and Codex instruction files and project memory |

### 7.4 Review and recovery

| Command | What it does |
|---|---|
| `/checkpoints` | List checkpoints for this thread |
| `/review` | Review the latest turn diff and keep line notes |
| `/diff` | Diff a checkpoint against the working tree |
| `/rewind` | Restore a turn and optionally the code; `--native` uses a harness rollback |
| `/retry` | Retry a failed or unverified turn on the same harness |
| `/queue` | Inspect, send, edit, delete, reorder, or clear queued follow-ups |
| `/compact` | Compact the conversation |

### 7.5 Orchestration

| Command | What it does |
|---|---|
| `/orchestrate on`, `/orchestrate off` | Switch this thread between orchestrate and single mode |
| `/orchestrate --plan-only <goal>` | Preview the plan before workers start |
| `/multi` | Fan one prompt across providers, or use isolated worktrees |
| `/runs` | List, reopen, or include archived runs |
| `/agents` | Inspect subagent calls, live and persisted |

### 7.6 Git

| Command | What it does |
|---|---|
| `/git` | Open the git picker |
| `/git status` | Show branch, upstream, changes, and operation state |
| `/git commit [--staged] [message]` | Confirm and commit the repository or the staged index |
| `/git push` | Pick a remote and push without force |
| `/git checks` | List the current pull request's checks |
| `/git pr [title]` | Preview and open a pull request |
| `/git review [action] [body]` | Approve, request changes, or comment |
| `/git merge [method]` | Squash, merge-commit, or rebase the pull request |

### 7.7 Memory, skills, and settings

| Command | What it does |
|---|---|
| `/memory` | Open instruction files, rules, and auto-memory topics |
| `/skills` | Pick a Claude or Codex skill and insert its invocation |
| `/mcp` | List, add, remove, or sync MCP servers |
| `/init` | Create a project memory file |
| `/export` | Export the conversation |
| `/copy` | Copy the latest assistant reply |
| `/clear`, `/quit`, `/exit` | Clear or leave the session |

## 8. Orchestration

### 8.1 Plan

A coordinator receives the goal and returns a small set of independent tasks. Each task has a title, a provider, an optional model, an optional effort, and a self-contained prompt. Plans are capped at 16 tasks. If the coordinator cannot produce valid JSON, Baton retries once and then falls back to one direct worker.

### 8.2 Workers

A worker starts in its own git worktree under `$BATON_HOME/worktrees/<run>/<task>` on branch `baton/<run>/<task>`. Workers run in parallel through the normal `baton exec` path. A successful worker commits as `baton worker: <title>`.

If the source checkout is dirty, Baton asks first. Enter snapshots tracked and untracked non-ignored files into each worktree; `s` runs one harness in the live checkout; Esc cancels.

### 8.3 Reviewer

After each completed worker, the reviewer reads the commit diff and the worker report, then answers `APPROVE` or `REQUEST_CHANGES`. An unreadable verdict fails closed as `request_changes`. A blocked task makes the run `needs_review`, and integration or shipping names the task and refuses.

Set `"review": false` in `roles.json` to skip the reviewer. The `before_review` and `after_review` hooks run around the turn.

### 8.4 Integrate

```sh
baton orchestrate --integrate <run-id>
```

Integration merges approved worker branches in plan order into a clean source checkout. It refuses a dirty checkout, a worker waiting for input, an unapproved commit, or a conflict, and reports the affected files.

### 8.5 Ship

```sh
baton orchestrate --open-prs <run-id>
```

Shipping pushes each approved worker branch and opens one pull request against the source checkout's current branch. It is idempotent: merged tasks and recorded PRs are skipped, and an existing PR for the same branch is linked.

### 8.6 Follow-ups

Open a worker from the run report, send a follow-up, and the successful turn commits that worktree and refreshes the run. A new commit clears the merge and PR markers and marks the review `pending`. In the TUI, a completed follow-up with a new commit starts the reviewer automatically when the reviewer role is enabled; press `r` in the report or run `baton orchestrate --review <run-id>` to re-review manually.

If a worker ends with `BATON_QUESTION: <question>`, the task and run become `needs_input`. Integration and shipping are refused until the worker thread is answered.

### 8.7 Roles and hooks

The four roles are orchestrator, planner, implementor, and reviewer. Each has a provider, model, optional effort, and a markdown prompt under `~/.baton/roles/<role>.md`. Override models in `~/.baton/roles.json`.

Declared hooks run with a timeout: `before_plan`, `after_plan`, `before_implement`, `after_implement`, `before_review`, and `after_review`. A failing plan hook aborts the run. A failing implement hook fails the task before the commit. A failing review hook marks the review failed.

See [Set up Baton](setup.md) for defaults and complete `roles.json` examples.

### 8.8 Shared directories

Large gitignored directories such as `node_modules` or `target` can be shared from the primary checkout:

```json
{
  "worktree": {
    "sharedDirectories": ["node_modules", "target"]
  }
}
```

Each entry becomes a symlink in the new worktree. Edits inside a shared directory affect the primary checkout, so use it only for build output and dependency caches.

### 8.9 CLI examples

```sh
baton orchestrate --max-tasks 3 "Audit billing, add tests, update docs"
baton orchestrate --list
baton orchestrate --run <run-id> --json
baton orchestrate --integrate <run-id>
baton orchestrate --open-prs <run-id>
baton orchestrate --review <run-id> --task 2
baton orchestrate --archive <run-id>
```

## 9. Git delivery loop

### 9.1 Status, commit, push

`/git status` shows the branch, upstream, ahead and behind counts, operation state, conflicts, and every changed path. `/git commit` previews the exact paths and message before it commits. `/git push` picks a remote, names the ref and counts, and never forces.

### 9.2 Checks

`/git checks` reads the current pull request with `gh` and lists every check with its bucket, state, and description.

### 9.3 Open a pull request

`/git pr [title]` previews the repository, base, branch, title, body, and HEAD. It rereads both remote branches and runs `gh pr create` only when they still match the preview.

### 9.4 Review a pull request

`/git review` opens Approve, Request changes, and Comment. Approve confirms and runs `gh pr review --approve` only when the local HEAD still matches the pull request head. Request changes and Comment put `/git review <action> ` in the composer for the body.

### 9.5 Merge a pull request

`/git merge` opens Squash and merge, Create a merge commit, Rebase and merge, and Cancel. The confirmation names the method and HEAD. The merge rereads the pull request and refuses a changed number, a non-open state, a conflicting state, or a moved head.

### 9.6 Safety rules

- No force-push.
- No automatic upstream change.
- No PR without a preview.
- No merge or review after the head moved.
- No commit while another thread has open work in the same worktree.

## 10. Memory and skills

### 10.1 Memory files

`/memory` lists Claude user, project, local, and rule files; Codex user instructions and overrides; project `AGENTS.md` and `AGENTS.override.md`; and the project auto-memory index and topics. Enter opens the selected file or creates it first. Auto-memory toggles Claude's `autoMemoryEnabled` setting.

### 10.2 Skills

`/skills` lists skills discovered in the Claude and Codex skill directories. Enter inserts `$name` into the composer and reports the source. A Claude thread rewrites the final payload to `/name`; the stored transcript keeps `$name`.

### 10.3 Governed memories

`$memories` in a prompt expands Baton's governed project and global notes. `baton memories` lists, adds, removes, or clears them for scripts.

## 11. MCP for other agents

### 11.1 Configure

`baton mcp` serves the local store over MCP stdio:

```json
{
  "mcpServers": {
    "baton": {
      "command": "baton",
      "args": ["mcp"]
    }
  }
}
```

### 11.2 Tools

The server exposes 15 tools: `baton_doctor`, `baton_status`, `baton_threads`, `baton_turns`, `baton_agents`, `baton_messages`, `baton_queue`, `baton_search`, `baton_latest_diff`, `baton_processes`, `baton_orchestration_runs`, `baton_orchestration_report`, `baton_activity`, `baton_memories`, and `baton_fork`.

It is read-only except for `baton_queue` clear and `baton_fork`, which creates a new thread and leaves the source unchanged.

### 11.3 JSON streams

`baton exec --stream-json` and `baton orchestrate --stream-json` emit one compact JSON object per line. Events include provider and turn ids, text deltas, tool start and finish, usage, turn completion, subagent start and finish, and a final `type: "result"` row.

## 12. Command-line reference

### 12.1 Sessions and inspection

```sh
baton                              # fresh session
baton --mode single                # one harness
baton -t <id>                      # open a thread
baton threads [--json]
baton inbox [--json]
baton status [-t id] [--json]
baton activity [-t id] [--json]
baton turns [-t id] [--json]
baton agents -t <thread-id> [--json]
baton usage
baton search <query> [--all] [--json]
baton doctor [--json]
baton ps [--json]
```

### 12.2 Turns and execution

```sh
baton exec -p <provider> [--effort level] [--json | --stream-json] "prompt"
baton exec -t <worker-thread> [--json] "answer"
baton spawn [-p provider] [--json] "task"
baton resume [-t id] [--fork] [--print] [--force]
baton fork [-t id] [--turn n] [--native] [--json] [title]
baton rewind [-t id] --turn N [--native] [--json]
```

### 12.3 Orchestration

```sh
baton orchestrate [--max-tasks N] [--plan-file path] [--plan-only] [--progress | --stream-json | --json] "goal"
baton orchestrate --list [--all] [--json]
baton orchestrate --run <run-id> [--json]
baton orchestrate --review <run-id> [--task N] [--json]
baton orchestrate --integrate <run-id> [--task N] [--json]
baton orchestrate --open-prs <run-id> [--task N] [--json]
baton orchestrate --archive <run-id> [--json]
baton worktrees [--json] [--run <id>]
baton worktrees --clean [--force] [--run <id>]
```

### 12.4 Data and maintenance

```sh
baton files [query...] [--ignored] [--json]
baton open <path[:line]> [--print]
baton grep <query> [--case-sensitive] [--word] [--regex] [--json]
baton memories [-C path] [--json]
baton queue [-t id] [--clear] [--json]
baton auth [provider] [subscription|api]
baton roles [--json]
baton diff <id> [turn]
baton restore <id> <turn>
baton import [--dry-run]
baton mcp
baton bench startup [--json] [--repeat N]
baton completions <shell>
```

## 13. Configuration and storage

### 13.1 Files

- `~/.baton/baton.sqlite`: threads, provider sessions, message mirror, turns, queued follow-ups, review notes, checkpoints, usage, subagents, and verified process records.
- `~/.baton/config.json`: explicit overrides for providers, default mode, and settings. Written with mode 0600.
- `~/.baton/roles.json` and `~/.baton/roles/<role>.md`: role models and prompts.
- Harness configs are read from `~/.claude`, `~/.codex`, and their project-level equivalents.

### 13.2 BATON_HOME

Set `BATON_HOME` to relocate every Baton file. This is useful for isolated tests and for keeping work and personal state apart.

### 13.3 Permission modes

| Mode | What it allows |
|---|---|
| `default` | The harness asks before sensitive actions |
| `acceptEdits` | File edits are accepted; shell commands still ask |
| `bypassPermissions` | No permission checks; use only in trusted work |
| `plan` | Read-only; the harness proposes a plan |

### 13.4 Roles

Roles choose the harness, model, effort, prompt, and hooks for planning, implementation, and review. `/roles` shows the resolved rows. `baton roles --json` exposes the full registry.

### 13.5 Harness config

Baton reads each harness's own config for the model, permission mode, API key, and endpoint. An explicit `config.json` override wins. `/auth` shows which key variable Baton will read.

## 14. Safety and recovery

### 14.1 Approvals and permissions

Approval prompts are scoped to the thread that requested them. Answer with `y` to allow once, `a` to always allow, or `n` to deny. `c` copies the prompt.

### 14.2 Processes

Each provider child has a recorded owner, PID, command line, and start time. Baton reaps a child left by a killed process only when all of those still match. A watchdog terminates the child within about two seconds when Baton disappears.

### 14.3 Worktrees

`baton worktrees` lists managed worktrees. `baton worktrees --clean` removes clean worktrees whose branch is merged or shipped; dirty or unmerged work needs `--force`.

### 14.4 Privacy

Baton sends no telemetry and has no hosted service. Prompts go to the harness you chose. API keys stay in your harness config, your environment, or `~/.baton/config.json`. The MCP server reads the same local database and honors `BATON_HOME`.

## 15. Troubleshooting

### 15.1 doctor

`baton doctor` is the first stop. It lists each harness binary, its effective model and auth, the config files, database size, thread count, tracked processes, and a next action.

### 15.2 Common problems

| Symptom | What to try |
|---|---|
| `no harness` or a missing binary | Install the harness CLI and put it on `PATH` |
| `/auth` says `missing <VARIABLE>` | Set that variable, then reopen `/auth` |
| `/auth` says `cannot read config` | Fix `~/.baton/config.json`; the picker did not overwrite it |
| A turn fails immediately | Run `baton doctor` and read the harness error |
| A worktree cannot be removed | It has unmerged or dirty work; use `--force` only after checking |
| `gh` is missing | Install GitHub CLI and run `gh auth login` |
| A source checkout is dirty | Commit, stash, or accept the snapshot prompt |
| No pull request is found | Push the branch and open a PR first |

### 15.3 Getting more detail

- `/status` for the current thread.
- `/activity` for turn and tool history.
- `baton doctor --json` for a machine-readable diagnosis.
- `/agents` for subagent calls.
- `/commands` for every tool call in the thread.

## 16. Development and verification

### 16.1 Build and test

```sh
cargo build --locked
cargo test --locked
```

### 16.2 Verification

The repository keeps a CI contract, installed-binary probes, and formal state models under `docs/` and `.agents/`. See [docs/ci.md](docs/ci.md) for the CI design and [docs/ci-acceptance.md](docs/ci-acceptance.md) for the measured feedback windows.

### 16.3 User guides

- [Install Baton](install.md)
- [Set up Baton](setup.md)

## 17. License

MIT. See [NOTICE](NOTICE) for third-party license terms.
