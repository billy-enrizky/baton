# Baton

Baton is a terminal workspace for developers who use command-line coding agents and want one conversation that can switch between them, split work across them, and keep going when one hits a wall.

A harness is the coding agent CLI you already use, such as Codex, Claude Code, Cursor, Grok, OpenCode, or Antigravity. Baton drives the harness and keeps the thread, tools, and git history around it. It does not replace the harness or its subscription.

Site: https://baton.openbrowser.me · Docs: https://baton.openbrowser.me/docs

## What it can do

- Keep one conversation while switching harness or model.
- Split a goal into independent tasks and run each on the harness that fits.
- Give every worker its own git worktree and branch.
- Review each worker's commit before it can be merged.
- Rewind a turn, restore a checkpoint, retry failed work, and inspect usage.
- Commit, push, open, review, and merge pull requests from the terminal.
- Read project files, search contents, and attach file mentions with `@`.
- Use Claude and Codex memory and skill files, sync MCP servers across harnesses, and run role hooks.
- Expose durable state as JSON and over MCP for other agents.
- Run on a harness login or an API key, with no Baton account.

## Table of contents

- 1\. [What Baton is](#1-what-baton-is)
  - 1.1 [The problem it solves](#11-the-problem-it-solves)
  - 1.2 [Who it is for](#12-who-it-is-for)
  - 1.3 [What a harness is](#13-what-a-harness-is)
  - 1.4 [What Baton is not](#14-what-baton-is-not)
- 2\. [What you can do](#2-what-you-can-do)
  - 2.1 [One conversation, many harnesses](#21-one-conversation-many-harnesses)
  - 2.2 [Parallel work with a reviewer gate](#22-parallel-work-with-a-reviewer-gate)
  - 2.3 [Recovery and review](#23-recovery-and-review)
  - 2.4 [A complete git loop](#24-a-complete-git-loop)
  - 2.5 [Agent-readable state](#25-agent-readable-state)
- 3\. [Install](#3-install)
  - 3.1 [One-line install](#31-one-line-install)
  - 3.2 [Homebrew](#32-homebrew)
  - 3.3 [From source](#33-from-source)
  - 3.4 [What else you need](#34-what-else-you-need)
  - 3.5 [First-run checklist](#35-first-run-checklist)
- 4\. [Start a session](#4-start-a-session)
  - 4.1 [Fresh session](#41-fresh-session)
  - 4.2 [Single-harness mode](#42-single-harness-mode)
  - 4.3 [Orchestrate mode](#43-orchestrate-mode)
  - 4.4 [Open an existing thread](#44-open-an-existing-thread)
  - 4.5 [Keep several threads open](#45-keep-several-threads-open)
  - 4.6 [Your first prompt](#46-your-first-prompt)
  - 4.7 [Help inside the app](#47-help-inside-the-app)
- 5\. [Core concepts](#5-core-concepts)
  - 5.1 [Harness](#51-harness)
  - 5.2 [Thread](#52-thread)
  - 5.3 [Turn](#53-turn)
  - 5.4 [Session mode](#54-session-mode)
  - 5.5 [Worktree](#55-worktree)
  - 5.6 [Run](#56-run)
  - 5.7 [Role](#57-role)
  - 5.8 [Checkpoint](#58-checkpoint)
  - 5.9 [Native session and handoff](#59-native-session-and-handoff)
- 6\. [The terminal interface](#6-the-terminal-interface)
  - 6.1 [Status bar](#61-status-bar)
  - 6.2 [Composer](#62-composer)
  - 6.3 [Pickers and previews](#63-pickers-and-previews)
  - 6.4 [Transcript](#64-transcript)
  - 6.5 [Keyboard](#65-keyboard)
    - 6.5.1 [Send and newline](#651-send-and-newline)
    - 6.5.2 [Interrupt and exit](#652-interrupt-and-exit)
    - 6.5.3 [Select and copy](#653-select-and-copy)
    - 6.5.4 [Steering and queueing](#654-steering-and-queueing)
    - 6.5.5 [Palette](#655-palette)
    - 6.5.6 [Thread tabs](#656-thread-tabs)
- 7\. [Slash commands](#7-slash-commands)
  - 7.1 [How commands behave](#71-how-commands-behave)
  - 7.2 [Session and model](#72-session-and-model)
  - 7.3 [Navigation and project](#73-navigation-and-project)
  - 7.4 [Review and recovery](#74-review-and-recovery)
  - 7.5 [Orchestration](#75-orchestration)
  - 7.6 [Git](#76-git)
  - 7.7 [Memory, skills, and settings](#77-memory-skills-and-settings)
- 8\. [Orchestration](#8-orchestration)
  - 8.1 [Plan](#81-plan)
  - 8.2 [Workers](#82-workers)
  - 8.3 [Reviewer](#83-reviewer)
  - 8.4 [Integrate](#84-integrate)
  - 8.5 [Ship](#85-ship)
  - 8.6 [Follow-ups](#86-follow-ups)
  - 8.7 [Roles and hooks](#87-roles-and-hooks)
  - 8.8 [Settle a stalled run](#88-settle-a-stalled-run)
  - 8.9 [Shared directories](#89-shared-directories)
  - 8.10 [CLI examples](#810-cli-examples)
  - 8.11 [Schedules](#811-schedules)
- 9\. [Git delivery loop](#9-git-delivery-loop)
  - 9.1 [Status, commit, push](#91-status-commit-push)
  - 9.2 [Checks](#92-checks)
  - 9.3 [Open a pull request](#93-open-a-pull-request)
  - 9.4 [Review a pull request](#94-review-a-pull-request)
  - 9.5 [Merge a pull request](#95-merge-a-pull-request)
  - 9.6 [Safety rules](#96-safety-rules)
- 10\. [Memory and skills](#10-memory-and-skills)
  - 10.1 [Memory files](#101-memory-files)
  - 10.2 [Skills](#102-skills)
  - 10.3 [Governed memories](#103-governed-memories)
- 11\. [MCP for other agents](#11-mcp-for-other-agents)
  - 11.1 [Configure](#111-configure)
  - 11.2 [Tools](#112-tools)
  - 11.3 [JSON streams](#113-json-streams)
- 12\. [Command-line reference](#12-command-line-reference)
  - 12.1 [Sessions and inspection](#121-sessions-and-inspection)
  - 12.2 [Turns and execution](#122-turns-and-execution)
  - 12.3 [Orchestration](#123-orchestration)
  - 12.4 [Data and maintenance](#124-data-and-maintenance)
  - 12.5 [Exit codes](#125-exit-codes)
- 13\. [Configuration and storage](#13-configuration-and-storage)
  - 13.1 [Files](#131-files)
  - 13.2 [BATON_HOME](#132-baton_home)
  - 13.3 [Permission modes](#133-permission-modes)
  - 13.4 [Roles](#134-roles)
  - 13.5 [Harness config](#135-harness-config)
  - 13.6 [Terminal capabilities](#136-terminal-capabilities)
- 14\. [Safety and recovery](#14-safety-and-recovery)
  - 14.1 [Approvals and permissions](#141-approvals-and-permissions)
  - 14.2 [Processes](#142-processes)
  - 14.3 [Worktrees](#143-worktrees)
  - 14.4 [Privacy](#144-privacy)
- 15\. [Troubleshooting](#15-troubleshooting)
  - 15.1 [doctor](#151-doctor)
  - 15.2 [Common problems](#152-common-problems)
  - 15.3 [Getting more detail](#153-getting-more-detail)
- 16\. [Development and verification](#16-development-and-verification)
  - 16.1 [Build and test](#161-build-and-test)
  - 16.2 [Verification](#162-verification)
  - 16.3 [User guides](#163-user-guides)
- 17\. [License](#17-license)

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

Every editing turn leaves a checkpoint. `/rewind` restores the conversation and optionally the code. A file restore refuses the repository's main checkout, where another thread can share the same files, and refuses any checkout with open work from another thread. The conversation rewind still completes, so you can continue without restoring files. `/retry` retries a failed or unverified turn on the same harness. `/review` shows the turn diff and sends line notes back as a steer or the next prompt.

### 2.4 A complete git loop

`/git` covers status, commit, push, pull request checks, opening a pull request, approving or requesting changes, and squash, merge-commit, or rebase merges. Every remote action has a preview and a recheck before it runs.

### 2.5 Agent-readable state

`baton status`, `threads`, `turns`, `queue`, `activity`, `agents`, `exec`, and `orchestrate` offer JSON output. An `exec --json` result carries the last plan the harness reported, when there is one. `exec --stream-json` and `orchestrate --stream-json` emit one event per line. `baton mcp` exposes the same store over MCP stdio, or over HTTP with a bearer token for remote agents.

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

The installer downloads the release for your OS and checks its SHA-256. When the release archive is unavailable, it falls back to a source build when Cargo and git are installed.

### 3.2 Homebrew

```sh
brew install billy-enrizky/baton/baton
```

Upgrade an existing install with `brew upgrade billy-enrizky/baton/baton`. The formula is maintained in the `billy-enrizky/baton` tap, outside this repository.

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
- `gh`, for pull request checks, creation, review, merge, and `/task`.
- Optional: `rg` for faster file and content search. Without it, `/files` uses `git ls-files` and `/grep` uses `git grep` in a repository.
- Optional: `VISUAL` or `EDITOR` for opening files.
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

The workspace opens in the configured default session mode. Orchestrate is the default until you change the global default with `/settings` or `"defaultMode": "single"` in `config.json`. `/orchestrate off` changes only the current thread.

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

`baton threads` lists each thread's short id, title, harness, model, mode, phase, turn count, and queue count. `--json` also includes the goal. `baton -t` opens one and keeps its saved session mode.

### 4.5 Keep several threads open

Each thread you open stays in a tab strip above the transcript. The active tab is highlighted and shows its position, from `1:` to `9:`. Press Alt plus a number to jump to that tab, so you can move between an implementation thread, a review thread, and a side chat without going back to the picker.

The strip appears once a second tab is open and shows up to nine tabs, then a `...` marker when more are open. A running turn keeps running when you switch away; its tab carries a `*` marker and keeps streaming, and the next visit shows everything it produced. A queued follow-up starts automatically when its background turn finishes, unless the thread's harness changed and a handoff preview needs your review; that one stays queued for the next visit. Archiving a thread closes its tab. A tab switch is refused while a request waits for your answer or a picker or full-screen view is open, so the current surface is never torn in the middle. Starting a thread that has not been sent yet keeps it in its tab; send one message to save it.

`/pane` shows another open tab beside the active one, with its own transcript, running marker, and live tool or stream tail. Alt+W swaps focus between the panes, so the composer follows the thread you are reading. Alt+[ and Alt+] resize the split, and `/pane wider`, `/pane narrower`, and `/pane reset` do the same from the command line. `/pane close` hides the second pane. A narrow terminal stacks the panes instead of placing them side by side.

### 4.6 Your first prompt

Type what you want in plain language. Examples:

- "Add a retry to the upload client and test it."
- "Audit the billing settings, add the missing tests, and update the docs."
- "Explain how the cache invalidation works."

In orchestrate mode the planner runs first and the run report shows the task split as workers start. Use `/orchestrate --plan-only <goal>` to review a plan before anything runs; Enter starts it, and Esc returns the goal to the composer.

### 4.7 Help inside the app

- `/help` shows the common paths.
- `/help all` opens a scrollable list of every command and its description; type a command and a space for its syntax.
- `baton doctor` checks what is installed and what to do next.
- `/status` shows the current thread, phase, goal, queue, and adapter state.

## 5. Core concepts

### 5.1 Harness

The coding agent CLI that performs a turn. Baton can switch harnesses inside one thread.

### 5.2 Thread

A durable conversation with an id, a working directory, a harness, a model, a session mode, and a transcript. Threads survive restarts. A side chat is a fork marked as belonging to its source thread; `/btw` opens one and leaves the source unchanged. An archived thread stays out of the default lists and `/inbox` until `/unarchive`, and a running turn blocks the archive.

### 5.3 Turn

One prompt and the agent's response, including tool calls, a diff, usage, and a phase such as running, completed, failed, or unverified.

### 5.4 Session mode

`single` sends each prompt to one harness. `orchestrate` sends a goal to a planner first. The mode belongs to the thread.

### 5.5 Worktree

A separate checkout of the repository on its own branch. Orchestration workers each get one, so parallel edits do not collide.

### 5.6 Run

One orchestration attempt. A run has a plan, workers, commits, review verdicts, and a final status such as completed, partial, failed, cancelled, needs_input, or needs_review.

### 5.7 Role

A named provider, model, effort, and markdown prompt. Hooks are declared globally in the same roles file. Baton has four roles: orchestrator, planner, implementor, and reviewer.

### 5.8 Checkpoint

A hidden git ref that captures the working tree at a turn boundary. It powers `/rewind`, `/review`, and `/diff`. A turn that changes nothing does not write one.

### 5.9 Native session and handoff

Each harness keeps its own native session per Baton thread. Later turns send only the new message, which keeps provider prompt caches warm. The first turn on a harness with no session gets a handoff built from prior messages and the working tree, with a 24,000-character budget.

## 6. The terminal interface

### 6.1 Status bar

The top of the screen shows the Baton version, working directory, session mode, current harness and model, reasoning effort when set, permission mode, and billing source. A queue, goal, or stale-turn warning appears when relevant.

### 6.2 Composer

The composer is where prompts, slash commands, and file mentions are typed. `@` followed by part of a project path offers matching directories with a trailing slash, then ranked files. Tab or Enter replaces only the mention. Ordinary text and addresses such as `a@b` never start a file search. Start a line with `!` to run a shell command in the thread working directory. Baton captures stdout and stderr, sanitizes and caps the output, and shows it with the exit code. The command is not sent to the harness, and it is not an interactive terminal: one command runs at a time with a 60-second limit.

### 6.3 Pickers and previews

An approval request shows the provider's own choices. Up and Down select a row, Enter sends it, and y, a, and n keep the standard allow once, allow for session, and deny shortcuts. A request from a child thread names that thread in the header.

Some providers also ask a question before continuing. The same overlay lists the choices, walks multiple questions in order, and allows a custom answer when the provider permits one. A secret answer is masked on screen.

Most commands open a picker instead of requiring memorized syntax. Pickers show their keys, the current selection, and the relevant detail. Remote actions open a preview first and recheck the live state before they run. When the terminal bell is enabled in settings, Baton rings once when a turn finishes, an approval arrives, or a provider asks a question. A two-second limiter mutes repeats.

`/plan` opens a scrollable overlay of the last plan the current harness reported, with `✓`, `▸`, and `·` markers. A markdown plan renders its markdown, and a file plan shows its URI. Long steps, markdown, and explanations wrap instead of being cut off, and the header names the turn that reported the plan when the harness supplies one. The overlay never edits the plan. The last plan for each thread and harness is saved with the thread, so `/plan` still works after a restart; new work, `/multi`, orchestration, and a rewind clear it. An orchestration worker's plan is stored on that worker's thread once the worker settles. In plan mode (`/mode plan`) Enter accepts the plan, switches the thread to the default permission mode, and sends nothing. Esc closes the view and pre-fills an empty composer with `revise the plan: `. `r` rejects the plan and pre-fills `reject this plan and propose a different approach: ` while leaving the thread in plan mode. Outside plan mode Enter and `r` say the view is read-only. Up and Down, PageUp and PageDown, Home, and End scroll; `q` always closes.

### 6.4 Transcript

The transcript streams Markdown with tables, aligned columns, tinted diffs, and one row per tool call. Running tools show a status; completed and failed tools keep their result. A diff longer than twelve rows collapses to a preview with `… N more lines · ctrl+t expand`, and a multi-file change keeps every file's path and counts in that preview; Ctrl+T expands every long diff inline and changes the hint to `ctrl+t collapse`. Long diff rows wrap at the terminal width.

### 6.5 Keyboard

#### 6.5.1 Send and newline

Enter sends. Shift+Enter or Alt+Enter adds a newline. Pasting a multi-line prompt keeps it in the composer.

#### 6.5.2 Interrupt and exit

Ctrl+C interrupts a running turn or cancels an orchestration, including its worker process groups. Press it twice when idle to exit. Esc closes a picker or preview.

#### 6.5.3 Select and copy

Drag in the transcript to select text across wrapped rows. `/copy` opens a list of assistant replies; choose one, or run `/copy 1` for the latest. Copy keys are shown in each overlay; the activity, review, and tool-call views use `y`.

#### 6.5.4 Steering and queueing

While a turn is running, a new composer message is sent as a steer on Codex, Claude, and OpenCode 2 or newer. Other harnesses queue it durably. A bare `/queue` opens the queue: Enter sends, `e` edits, `d` deletes, `c` twice clears, and Ctrl+Up/Ctrl+Down reorders.

#### 6.5.5 Palette

Ctrl+K opens the command palette. It filters built-in actions, threads, and recent files as you type. Enter runs an action, opens a thread, or inserts a file path; Esc closes it. Ctrl+K is ignored while a picker is open, so it cannot replace work in progress.

#### 6.5.6 Thread tabs

Alt+1 through Alt+9 switch to the tab in that position. Alt+0 does nothing. The strip only appears when more than one thread is open, and every open thread keeps its own transcript, composer draft, prompt history, goal, permission mode, and live turn state. A tab in another working directory also reloads that directory's custom commands. A tab whose turn is still running shows a `*` after its title until that turn settles.

`/pane` opens a picker for the second pane. Alt+W swaps the active and secondary threads, Alt+[ and Alt+] resize the split, and `/pane close` closes the second pane. The second pane is read-only; its header names the tab, and its body shows the transcript tail plus any live tool or stream rows for that thread.

## 7. Slash commands

### 7.1 How commands behave

Type `/` to see commands. Typing a command followed by a space always answers: it lists valid choices, shows the argument syntax, or says the command takes no arguments. Partial text filters the choices; Tab completes; Enter runs a complete command. If you move through the choices with Up or Down, Enter completes the highlighted one instead. The direct forms remain available for scripts. Ctrl+K opens the same actions as a palette, with thread and recent-file choices.

### 7.2 Session and model

| Command | What it does |
|---|---|
| `/help` | Show common paths; `/help all` opens the full command list |
| `/switch` | Pick a harness, then a model, for this thread |
| `/model` | Pick a model for the current harness |
| `/effort` | Pick a reasoning effort on Codex or Claude; `/effort default` clears it |
| `/mode` | Pick a permission mode for harnesses that support it: `default`, `acceptEdits`, `bypassPermissions`, or `plan` |
| `/auth` | Pick a billing mode for any harness; `/auth codex` scopes it. Queries stay available during work; writes wait until it ends |
| `/login` | Sign in to a harness; pick one from the list or run `/login codex` |
| `/orchestrate` | Switch modes, start a plan-only goal, or open recent runs |
| `/goal` | Set, pause, resume, or clear the thread goal |
| `/settings` | Change the global default session mode and terminal bell, and inspect harness settings |
| `/roles` | Inspect the four roles, models, and prompt sources |

### 7.3 Navigation and project

| Command | What it does |
|---|---|
| `/threads`, `/open`, `/pin`, `/inbox` | List, open, pin, or triage threads; `/threads all` includes archived threads |
| `/snooze` | Hide this thread from `/inbox` for an hour, four hours, a day, a week, or clear the snooze with `off`; a new failure or question after the snooze wakes it early |
| `/snippets` | List, add, or remove prompt commands under `.baton/commands` or `--global` under `~/.baton/commands`; a template expands `{{args}}` and `{{cwd}}`, a project command shadows a user command with the same name, and a template that starts with `!` runs as a shell command |
| `/schedule` | Open the schedule view; `p` pauses or resumes the selected row, `d` removes it after a second press, and Enter opens its thread. `/schedule add` starts a thread, cron, and prompt form; a blank thread uses the current one |
| `/archive`, `/unarchive` | Hide a thread from the default lists and `/inbox`, or restore it; a running turn blocks the archive |
| `/new`, `/fork`, `/worktree`, `/task` | Start a thread, fork one, or start from a git worktree or GitHub issue; `/task` without a number loads open issues and falls back to a clear notice when none can be read |
| `/btw` | Open a side chat from this thread; the source thread is unchanged |
| `/merge-back` | Append the side chat's new messages to its source as a redacted delta |
| `/files` | Pick a project file, insert its path or `path:line`, or open it in your editor |
| `/grep` | Search project contents, insert `path:line`, or open the match |
| `/find` | Search the conversation |
| `/activity`, `/commands`, `/agents` | Inspect turns, tool calls, and subagent calls |
| `/usage`, `/context`, `/status`, `/turns`, `/limits` | Inspect usage with a scope and grouping picker, plus context (including percent of the model-reported window, or an explicit unknown), status, turn history, and live or stored rate-limit windows; the composer warns at 80 percent with `/compact`, and the window persists with its model across restarts |
| `/memory` | Open Claude and Codex instruction files, project memory, any parent `CLAUDE.md` or `AGENTS.md` between the repository root and the working directory, and Codex fallback filenames from `config.toml` at the working directory and its parents |

`/files` and `/grep` use `rg` when it is installed and fall back to `git ls-files` and `git grep` inside a repository. Ctrl+O opens the selected file in `VISUAL` or `EDITOR`.

### 7.4 Review and recovery

| Command | What it does |
|---|---|
| `/checkpoints` | List checkpoints for this thread |
| `/review` | Review the latest turn diff, search it with `/`, and keep line notes |
| `/changes` | Every diff in this thread in one scrollable view; `/changes <turn>` filters one turn |
| `/plan` | Show the plan the current harness reported, with status and priority |
| `/diff` | Diff a checkpoint against the working tree |
| `/rewind` | Restore a turn and optionally the code; `--native` uses a Codex or OpenCode native rollback |
| `/retry` | Retry a failed or unverified turn on the same harness |
| `/queue` | Inspect, send, edit, delete, reorder, or clear queued follow-ups |
| `/compact` | Compact the conversation |

### 7.5 Orchestration

| Command | What it does |
|---|---|
| `/orchestrate on`, `/orchestrate off` | Switch this thread between orchestrate and single mode |
| `/orchestrate --plan-only <goal>` | Preview the plan before workers start |
| `/multi` | Fan one prompt across providers, or use isolated worktrees |
| `/questions` | List open worker questions and open the worker thread to answer |
| `/runs` | List runs and open a report to settle, review, merge, ship, or archive |
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
| `/git review [action] [body]` | Use `approve`, `request-changes`, or `comment` |
| `/git merge [method]` | Use `squash`, `merge`, or `rebase` on the pull request |

### 7.7 Memory, skills, and settings

| Command | What it does |
|---|---|
| `/memory` | Open instruction files, rules, auto-memory topics, parent `CLAUDE.md` or `AGENTS.md` files from the repository root down to the working directory, and Codex `project_doc_fallback_filenames` files at the working directory and its parents |
| `/skills` | Pick a Claude or Codex skill, insert its invocation, or install, update, or remove its project copy |
| `/mcp` | List, add, remove, or sync MCP servers |
| `/init` | Create a starter `CLAUDE.md` project memory file |
| `/export` | Export the conversation |
| `/copy` | Choose an assistant reply to copy; `/copy 1` copies the latest |
| `/clear`, `/quit`, `/exit` | Clear or leave the session |

## 8. Orchestration

### 8.1 Plan

A coordinator receives the goal and returns tasks. Each task has a title, a provider, an optional model, an optional `id`, an optional `effort`, a `dependsOn` list of task ids or 1-based positions, and a self-contained prompt. Plans are capped at 16 tasks; a duplicate id, an unknown dependency, a self-dependency, or a cycle is rejected before any worker starts. If the coordinator cannot produce valid JSON, Baton retries once and then falls back to one direct worker.

### 8.2 Workers

A worker starts in its own git worktree under `$BATON_HOME/worktrees/<short-run>/<task-slug>` on branch `baton/<short-run>/<task-slug>`. Workers run through the normal `baton exec` path. Tasks with no unmet dependency run in parallel; a task starts only after every dependency completes, and a failed or skipped dependency skips its dependents and leaves the run `partial` or `failed`. A worker that changes files commits as `baton worker: <title>`.

If the source checkout is dirty, Baton asks first. Enter snapshots tracked and untracked non-ignored files into each worktree; `s` runs one harness in the live checkout; Esc cancels.

### 8.3 Reviewer

After each completed worker with a commit, the reviewer reads the commit diff and the worker report, then answers `APPROVE` or `REQUEST_CHANGES`. A worker with no commit has no review to run. An unreadable verdict fails closed as `request_changes`. A blocked task makes the run `needs_review`, and integration or shipping names the task and refuses.

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

Open a worker from the run report and send a follow-up. A turn that changes files commits that worktree and refreshes the run. A new commit clears the merge and PR markers and marks the review `pending`. In the TUI, a completed follow-up with a new commit starts the reviewer automatically when the reviewer role is enabled. `baton exec -t` does the same for a worker answer and accepts `--no-review` to skip it. Press `r` in the report or run `baton orchestrate --review <run-id>` to re-review manually.

If a worker ends with `BATON_QUESTION: <question>`, the task and run become `needs_input`. Integration and shipping are refused until the worker thread is answered.

### 8.7 Roles and hooks

The four roles are orchestrator, planner, implementor, and reviewer. Each has a provider, model, optional effort, and a markdown prompt under `~/.baton/roles/<role>.md`. Override models in `~/.baton/roles.json`.

Declared hooks run with a timeout: `before_plan`, `after_plan`, `before_implement`, `after_implement`, `before_review`, and `after_review`. A failing plan hook aborts the run. A failing implement hook fails the task before the commit. A failing review hook marks the review failed.

See [Set up Baton](setup.md) for defaults and complete `roles.json` examples.

### 8.8 Settle a stalled run

A run whose orchestrator process stopped, or a run created before ownership was recorded, can stay `running` with tasks that will never finish. Record a final status:

```sh
baton runs settle <run-id> --status cancelled
baton runs settle <run-id> --status partial --json
```

Inside the workspace, `/runs` opens a run report. Press `x`, choose a status with the arrow keys, and press Enter to settle the run without leaving the app.

Statuses are `completed`, `partial`, `failed`, `cancelled`, `needs_review`, and `needs_input`. `completed` is refused unless every task completed and no turn is open. Every other status closes queued and running tasks, marks their open turns unverified, and writes the final status in one transaction. A run whose recorded owner process is still alive is refused unless you pass `--force`. A short run id works when it is unambiguous.

### 8.9 Shared directories

Large gitignored directories such as `node_modules` or `target` can be shared from the primary checkout. Create `baton.json` in the repository root:

```json
{
  "worktree": {
    "sharedDirectories": ["node_modules", "target"]
  }
}
```

Each listed directory that exists and is gitignored becomes a symlink in the new worktree. Edits inside a shared directory affect the primary checkout, so use it only for build output and dependency caches.

### 8.10 CLI examples

```sh
baton orchestrate --max-tasks 3 "Audit billing, add tests, update docs"
baton orchestrate --list
baton orchestrate --run <run-id> --json
baton orchestrate --integrate <run-id>
baton orchestrate --open-prs <run-id>
baton orchestrate --review <run-id> --task 2
baton orchestrate --archive <run-id>
baton runs settle <run-id> --status cancelled
```

### 8.11 Schedules

```sh
baton schedule add --thread <id> --cron "0 9 * * 1-5" --prompt "Check the failing tests"
baton schedule list --json
baton schedule remove <id>
```

Cron is five fields in UTC: minute, hour, day of month, month, and weekday. Each field accepts `*`, a number, a list, a range, and a step. `baton serve` checks due schedules every second, claims each tick with a compare-and-set so two schedulers cannot fire the same run, writes a `schedule.trigger` audit row, and posts the prompt into its thread. A due schedule is skipped while its thread is archived, and the skip is logged. If the host was down past several occurrences, one run fires when it returns and the row moves to the next future occurrence, so missed runs are not replayed. An invalid stored expression is retried in an hour and is not recorded as a run. Manage schedules from the CLI while no host or TUI owns the store: `baton schedule add --thread <id> --cron "<expr>" --prompt "<text>"`, `baton schedule list`, `baton schedule pause <id>`, `baton schedule resume <id>`, and `baton schedule remove <id>`. `baton serve` accepts the same verbs without the `baton` prefix at its terminal, and `/schedule` opens a view where `p` pauses or resumes, `d` removes after a second press, and Enter opens the thread. `/schedule add` opens a three-field form for an optional thread prefix, the cron, and the prompt; a blank thread uses the current one. A paused row never fires, and resuming recomputes its next run from the current time so a long pause does not replay a missed tick. A host with stdin closed cannot use the terminal commands.

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
- A push never sets or changes the branch upstream.
- No PR without a preview.
- No merge or review after the head moved.
- No commit while another thread has open work in the same worktree.

## 10. Memory and skills

### 10.1 Memory files

`/memory` lists Claude user, project, local, and rule files; Codex user instructions and overrides; project `AGENTS.md` and `AGENTS.override.md`; and the project auto-memory index and topics. Enter opens the selected file or creates it first. Auto-memory toggles Claude's `autoMemoryEnabled` setting.

### 10.2 Skills

`/skills` lists skills discovered in the project's `.claude/skills` and `.codex/skills` directories first, then the global Claude and Codex skill directories; a project skill with the same name shadows the global one. Rows show the harness, whether the copy is project or global, and a description from the skill's frontmatter when one is present. Enter inserts `$name` into the composer, `i` copies a global skill into this project, `u` replaces the project copy from its global source, and `d` removes a project copy after confirmation. A Claude thread rewrites the final payload to `/name`; the stored transcript keeps `$name`.

Install and update copy the whole skill directory through a staging directory. Symlinks and non-regular files are refused, the copy is capped at 2,048 files and 32 MiB, an existing project skill is never overwritten by install, and an update restores the old copy if the swap fails. Update replaces the project directory, so a file removed from the global skill is also removed from the project copy. Removal stages the project copy under a hidden name before deleting it, restores the copy if deletion fails, and never touches the global source.

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

For a remote agent, serve HTTP POST JSON-RPC on a loopback address with a private bearer-token file:

```sh
openssl rand -hex 32 > ~/.baton/mcp-token
chmod 600 ~/.baton/mcp-token
baton mcp --http 127.0.0.1:8787 --token-file ~/.baton/mcp-token
```

The endpoint is `POST /mcp`; `GET /health` answers a readiness object without a token. The token file must be a regular file with mode 0600 and a 32 to 512 character value without whitespace. For HTTPS, add `--tls-cert <pem> --tls-key <pem>`; the key must also be mode 0600. A non-loopback bind needs either TLS or the plain-HTTP `--trusted-overlay`, and the two flags cannot be combined. The token is read at startup. On Unix, replace the file and send SIGHUP to reload it without dropping the listener; an invalid file keeps the current token. On Windows, restart the server. Add `--read-only` to hide `baton_fork` and refuse queue clearing. Read tools, including `baton_queue` without `clear`, stay available. Use repeatable `--allow-tool <name>` instead to expose only the named tools; it cannot be combined with `--read-only`.

`baton serve` claims the local store for one host process, runs provider and turn recovery, and listens on `BATON_HOME/serve.sock` with mode 0600 until SIGINT or SIGTERM. `--stdio` serves the spawner pipe instead, and `--socket <path>` overrides the path. Every socket request except `ping` must carry a live grant id and secret with the scope its command needs: `read` for status, messages, approvals, and questions; `operate` for submit; `approve` for respond and answer. The peer uid must match the host. `baton grant list` shows grants without their secrets, `baton grant renew <id> [--days N]` extends a live grant without shortening its window or changing its secret, and `baton grant revoke <id>` deletes the client secret and refuses every later request. `baton audit list [--limit N] [--json]` shows the append-only audit log: actor, environment, target, action, scope, confirmation, payload digest, result, timestamp, and retention. The target names what changed, such as a grant id, thread id, request id, commit sha, or pull request number. The audit table rejects updates and deletes, and a grant mint or revoke commits with its audit row in one transaction. Every accepted turn writes a `turn.submit` row from the host engine, with the prompt left out of the digest, every approval answer writes an `approval.decide` row and every question answer writes a `question.answer` row before the provider receives them. An answer that cannot be recorded is sent as a denial or an empty answer. Successful git commits, pushes, pull request opens, reviews, and merges write `git.*` rows with the commit sha or pull request number, and a row that cannot be written is reported in the notice after the action lands. A saved global setting writes a `settings.write` row named by its key, and a row that cannot be written is reported after the setting saves. Starting an orchestration run and settling one write `orchestration.start` and `orchestration.settle` rows named by the run id, in the same transaction as the run change. Adding, removing, or clearing a governed memory writes a `memory.*` row named by the memory id or scope, in the same transaction as the memory change. MCP fork and queue clear also write audit rows, with the stdio spawner recorded as the confirming authority. Automatic run completion and abandonment, `baton default` and `baton auth` config writes, and hand-edited config files remain unaudited. A renewal writes a `grant.renew` audit row. At the terminal that started the host, type `pair [--label <name>] [--scope <s>]... [--ceiling <mode>] [--project <path>]... [--expires-days N]`. It mints a grant with the `read` and `operate` scopes by default, replaces those defaults when you pass any `--scope`, and expires after 30 days unless you pass another number. When the client must list and answer approval or question requests, pass `--scope read --scope approve`; the first `--scope` replaces the defaults. The ceiling is `default` unless you pass `--ceiling plan`, `default`, or `acceptEdits`; bypass is never grantable. `--project <path>` restricts the grant to that absolute project root; repeat it for several roots, or omit it to allow every project. A submit or interaction outside the allowlist is refused with `not_permitted`. `baton grant list --json` shows the stored roots. It prints a `baton://local#grant=<id>&secret=<secret>` URL on stdout and the grant id, scopes, and expiry on stderr. `renew <id> [--days N]` extends one live grant from that same terminal. The fragment carries the secret, so treat the whole URL as a credential. A host whose stdin is closed keeps serving without host commands. From another terminal, `baton attach --url <url> [--json]` sends one read-only `status` request over the socket and prints the host scope, thread count, and running turn count. `--thread <id>` reads that thread's status, `--messages` pages its stored messages from a resume cursor, and `--follow` polls until the thread is idle, so a paired client can watch a turn without the TUI. `--submit <text>` starts a turn on that thread, and the host starts or reuses one engine for that thread's project directory. A project directory that is missing or not a directory is refused with `wrong_project`, and a thread in bypass mode or above the grant ceiling is refused with `ceiling`. `--idempotency-key <key>` reuses a retry key; without it attach makes a fresh one. The first accepted submit stores its result, and a repeat with the same key returns that result with `replayed` set and starts no second turn. A stored result is replayed for seven days; after that a retry starts a new turn. A raw socket `submit` must send `args.idempotency_key` as a non-empty string of at most 128 characters. `--approvals` lists the host's pending approval requests, optionally filtered by `--thread`. `--respond <request> --choice <choice>` answers one with the `approve` scope and writes an `approval.decide` row named by the grant; a wrong or already answered request answers `not_found`, and a repeated key returns the stored answer with `replayed` set. `--questions` lists pending question requests. `--answer <request> --answers '<json>'` answers one, where the JSON object maps each prompt id to a string or an array of strings; prefix the value with `@` to read a file. A question answer writes a `question.answer` row named by the grant and only records option labels and counts, never custom text. Attach never starts an engine in the client process; the host starts or reuses one engine per project and keeps it until shutdown. The local socket and attach are available on macOS and Linux; Windows uses `baton serve --stdio`. On shutdown it stops recorded provider children with a verified command and start identity, cancels the turns, tasks, and runs it owns with the `host shut down` reason, reports the worktrees it leaves in place, settles turns and runs whose owner process stopped, then releases the claim. A second host is refused with exit code 3 and the owning process. While a host is live, CLI and MCP clients exit 4 and name the owning pid and the `baton host takeover --pid <pid>` hint. The TUI opens read-only, shows the same ownership line, and refuses to send a prompt or change the store.

With `--stdio`, the host accepts one newline-delimited JSON request per line. Responses carry the same `id`, and stdout carries no diagnostics:

```json
{"v":1,"kind":"request","id":7,"command":"status","args":{"thread":null}}
{"v":1,"kind":"response","id":7,"ok":true,"result":{"scope":"host","threads":2,"running_turns":0}}
```

`ping` returns `{"pong":true}`, `claim-info` returns the live claim row or `null`, and `status` returns host-wide counts or counts for one `thread` id. A malformed line or unsupported version answers `malformed_line` or `bad_version` with `id: null` and keeps the host alive. An unknown command answers `not_implemented`. Closing stdin stops the host and releases the claim.

`baton host takeover --pid <pid>` reads the live claim, prints its pid and process start, and asks for `y` or `yes`. Confirmation replaces the claim, fences the old host out, stops provider children recorded under the old host generation, settles every open turn and running orchestration, releases the takeover claim, and leaves the store ready for a new `baton serve`. A wrong pid, a changed claim, a dead owner, a held write lock, or a declined prompt leaves the claim in place. The command does not signal the old host process.

### 11.2 Tools

The server exposes 15 tools: `baton_doctor`, `baton_status`, `baton_threads`, `baton_turns`, `baton_agents`, `baton_messages`, `baton_queue`, `baton_search`, `baton_latest_diff`, `baton_processes`, `baton_orchestration_runs`, `baton_orchestration_report`, `baton_activity`, `baton_memories`, and `baton_fork`.

It is read-only except for `baton_queue` clear and `baton_fork`, which creates a new thread and leaves the source unchanged.

### 11.3 JSON streams

`baton exec --stream-json` and `baton orchestrate --stream-json` emit one compact JSON object per line. Events include provider and turn ids, text deltas, tool start and finish, approvals, provider questions, plan updates and removals, usage, turn completion, subagent start and finish, and a final `type: "result"` row. A `plan_updated` row carries `entries` with `content`, `status`, `priority`, an optional `kind` (`items`, `markdown`, or `file`) and `uri` for a file plan, an optional `explanation`, and the reporting `turn_id` when the harness supplies one. Item plans omit `kind` and `uri` to keep the original wire shape.

## 12. Command-line reference

### 12.1 Sessions and inspection

Global flags apply before or after a subcommand:

```sh
baton -C <dir>                    # working directory
baton -t <thread-id>              # target a thread
baton -p <provider> -m <model>    # provider and model overrides
baton --effort <level>            # reasoning effort where supported
baton --mode <single|orchestrate> # session mode for new work
baton --plan                      # read-only plan permission mode
baton --full-auto | --yolo        # accept edits, or bypass permissions
baton --claude-home <dir>         # override Claude config home
baton --codex-home <dir>          # override Codex config home
baton --limit <n>                 # list or output limit
baton --dry-run                   # inspect without running a turn
```

`--plan` is the read-only permission mode, `--plan-file` feeds a plan to orchestration, and `/plan` shows the plan a harness reported.

```sh
baton                              # fresh session
baton chat                         # interactive workspace (default)
baton new [title]                  # start a thread
baton --mode single                # one harness
baton -t <id>                      # open a thread
baton login <provider>             # sign in to a provider
baton default <provider>           # set the default provider
baton task <number>                # linked worktree for a GitHub issue
baton threads [--json]
baton inbox [--json]
baton status [-t id] [--json]
baton activity [-t id] [--json]
baton turns [-t id] [--json]
baton agents -t <thread-id> [--json]
baton usage [--by day|model|provider] [--json]
baton search <query> [--all] [--json]
baton doctor [--json]
baton ps [--json]
baton serve [--stdio | --socket <path>]  # claim the store and serve the pipe or socket
# in the host terminal: pair [--label <name>] [--scope <s>]... [--ceiling <mode>] [--project <path>]... [--expires-days N]
baton attach --url <url> [--json]  # read a paired host's status over the socket
baton host takeover --pid <pid>    # replace a live host after confirmation
```

In a directory with no threads, `baton status` still exits 0. The text form says `no thread in this directory yet. Run baton to start one.`, and `--json` keeps the usual shape with `thread_id: null`, `turn_count: 0`, and `next_action: "start a thread with baton"`.

`baton usage` prints one row per thread. `--by day` sums newest days first; `--by model` and `--by provider` sum alphabetically. `--json` emits schema version 1 with a `threads` array, or a `groups` array with `by` when grouping.

### 12.2 Turns and execution

```sh
baton exec -p <provider> [--effort level] [--json | --stream-json] "prompt"
baton exec -t <worker-thread> [--json] [--no-review] "answer"
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
baton runs settle <run-id> --status <status> [--force] [--json]
baton worktrees [--json] [--run <id>]
baton worktrees --clean [--force] [--run <id>]
```

### 12.4 Data and maintenance

```sh
baton files [query...] [--ignored] [--json]
baton open <path[:line]> [--print]
baton grep <query> [--case-sensitive] [--word] [--regex] [--json]
baton memories [add|remove|clear] [-C path] [--json]
baton grant list [--json]
baton grant renew <id> [--days N] [--json]
baton grant revoke <id> [--json]
baton audit list [--limit N] [--json]
baton checkpoints <thread-id>
baton queue [-t id] [--clear] [--json]
baton auth [provider] [subscription|api]
baton secret check [--json]
baton roles [--json]
baton diff <id> [turn]
baton restore <id> <turn> [--force]
baton import [--dry-run]
baton mcp [--read-only | [--allow-tool <name>]...] [--http <addr> --token-file <path> [--tls-cert <pem> --tls-key <pem> | --trusted-overlay]]
baton bench startup [--json] [--repeat N]
baton completions <shell>
baton update
```

### 12.5 Exit codes

| Code | Meaning |
|---|---|
| `0` | The command finished. A query that finds no threads or no work still succeeds. |
| `1` | A runtime failure: a provider error, a missing repository, an unreadable store, or a failed turn. |
| `2` | A usage error: an unknown flag, a missing command argument, or a missing `exec` prompt. |
| `4` | A live host owns the store and this client does not have its host identity. Start the command after the host stops, or use the printed `baton host takeover --pid <pid>` hint. |
| `130` | Interrupted. A cancelled orchestration exits 130; a process killed by Ctrl+C/SIGINT reports signal 2, which shells show as 130. |

Machine output stays on stdout and progress stays on stderr, so a pipeline can parse `--json` without filtering log lines. The TUI itself does not use exit codes for turns; Ctrl+C interrupts a turn and a second idle Ctrl+C exits. A TUI that finds a live host claim starts read-only and shows the same owner line instead of exiting.

## 13. Configuration and storage

### 13.1 Files

- `~/.baton/baton.sqlite`: threads, provider sessions, message mirror, turns, queued follow-ups, review notes, checkpoints, usage, rate-limit snapshots, subagents, and verified process records.
- `~/.baton/config.json`: explicit overrides for providers, default mode, and settings. Written with owner-only permissions on Unix.
- `~/.baton/roles.json` and `~/.baton/roles/<role>.md`: role models and prompts.
- `~/.baton/worktrees/`: managed orchestration and thread worktrees.
- `~/.baton/model-cache/`: the last full model list for each harness.
- Baton reads Claude and Codex settings plus OpenCode's model setting from their global and project-level files.
- `~/.baton/commands/<name>.md` and `.baton/commands/<name>.md`: reusable slash commands whose body expands `{{args}}` and `{{cwd}}`. A body that starts with `!` runs through the shell path instead of the harness. `~/.claude/commands` and `.claude/commands` are also read, and a project file shadows a user file with the same name. `/snippets` lists, adds, and removes the `.baton` files, with `--global` selecting the user file.
- `baton.json` in a repository root: optional shared worktree directories.

### 13.2 BATON_HOME

Set `BATON_HOME` to relocate Baton's data directory, including configuration, database, roles, model cache, and worktrees. Checkpoint refs remain in each project repository. This is useful for isolated tests and for keeping work and personal state apart.

### 13.3 Permission modes

| Mode | What it allows |
|---|---|
| `default` | The harness asks before sensitive actions |
| `acceptEdits` | File edits are accepted; shell commands still ask |
| `bypassPermissions` | No permission checks; use only in trusted work |
| `plan` | Read-only; the harness proposes a plan |

Baton passes these modes to Claude and Codex. Cursor, Grok, OpenCode, and Antigravity keep their own permission flow.

### 13.4 Roles

Roles choose the harness, model, effort, and prompt for planning, implementation, and review. Hooks are global declarations in `roles.json`. `/roles` shows the resolved rows. `baton roles --json` exposes the full registry.

### 13.5 Harness config

Baton reads Claude settings, Codex config, and the OpenCode model setting. A key resolves from `~/.baton/config.json` first, then the harness config, then the environment. `baton secret check` names the winning source for every harness without printing the key. `~/.baton/config.json` supplies provider overrides, including API keys and, for Grok or Antigravity, a custom endpoint. Keys can also come from the environment or the Claude or Codex config. An explicit `config.json` value wins. `/auth` shows which key variable Baton will read.

### 13.6 Terminal capabilities

Baton sizes the interface from the terminal and redraws when it resizes. It uses RGB colors only when `COLORTERM` is `truecolor` or `24bit`, or when `TERM` contains `direct`. A term that advertises `256color` without `direct` gets the nearest xterm-256 color, and every other term gets a 16-color palette picked by role, so the accent stays yellow and errors stay red instead of everything collapsing into gray. Set `NO_COLOR` to any non-empty value, or run with `TERM=dumb`, to draw without palette colors; selections and the composer caret switch to reverse video so they stay visible, and bold and underline still work. The interface needs an interactive terminal: when stdin or stdout is not one, Baton exits with a message and points to `baton exec` for scripts and pipes.

## 14. Safety and recovery

### 14.1 Approvals and permissions

Approval prompts are scoped to the thread that requested them. Answer with `y` to allow once, `a` to allow for the session, or `n` to deny. Provider choices also appear as rows that accept Enter.

### 14.2 Processes

Each provider child has a recorded owner, PID, command line, and start time. Baton reaps a child left by a killed process only when all of those still match. A watchdog terminates the child within about two seconds when Baton disappears.

### 14.3 Worktrees

`baton worktrees` lists managed worktrees. `baton worktrees --clean` removes clean worktrees whose branch is merged or recorded as shipped with a pull request. Dirty work, or a branch with no merged commit and no recorded pull request, needs `--force`.

### 14.4 Privacy

Baton sends no telemetry and has no hosted service. Prompts go to the harness you chose. Baton reads API keys from `~/.baton/config.json`, the environment, or Claude and Codex settings. In API mode, it passes the key to the harness process. The MCP server reads the same local database and honors `BATON_HOME`.

## 15. Troubleshooting

### 15.1 doctor

`baton doctor` is the first stop. It lists each harness binary, its effective model and auth, the config files, database size, thread count, tracked processes, and a next action.

### 15.2 Common problems

| Symptom | What to try |
|---|---|
| `no harness` or a missing binary | Install the harness CLI and put it on `PATH` |
| A command is missing or `/auth` looks different | Run `baton --version`. `command -v baton` names the install that runs; upgrade that one |
| `/auth` says `missing <VARIABLE>` | Set that variable, then reopen `/auth` |
| `/auth` says `cannot read config` | Fix `~/.baton/config.json`; the picker did not overwrite it |
| A turn fails immediately | Run `baton doctor` and read the harness error |
| A worktree cannot be removed | It is dirty, or its branch has no merged commit and no recorded pull request; use `--force` only after checking |
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

The repository keeps the CI contract in `docs/`, installed-binary probes under `.agents/`, and formal state models under `ci/formal/`. See [docs/ci.md](docs/ci.md) for the CI design and [docs/ci-acceptance.md](docs/ci-acceptance.md) for the measured feedback windows.

### 16.3 User guides

- [Install Baton](install.md)
- [Set up Baton](setup.md)

## 17. License

MIT. See [NOTICE](NOTICE) for third-party license terms.
