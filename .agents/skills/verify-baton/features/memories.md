# Memories

## Sub-features
- `/memories` list/add (`--global`)/remove/clear
- `$memories` injects project and global notes at the engine
- Project key is the canonical repo root, shared by worktrees and symlinks
- CLI `baton -C <project> memories ...` with JSON
- Read-only MCP `baton_memories`
- Global clear requires `--yes`

## Driving it with the CLI
```sh
baton memories add "Use tabs"
baton memories add --global "Answer briefly"
baton memories --json
baton memories clear --global --yes
```

## Gotchas
- `-t` is always a thread id; use `-C/--cwd` for a project.
- A memory is plain text; do not store secrets.
- The transcript keeps `$memories`; the harness receives the expanded block.
