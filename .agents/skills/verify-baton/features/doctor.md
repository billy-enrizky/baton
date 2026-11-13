# Doctor

## Sub-features
- Provider executable readiness
- Effective model, auth mode, and permission mode
- Harness config file presence
- Database path, size, and thread count
- Tracked provider process health
- Warnings for an unusable default provider or broken tracked process

## How to get to it (user POV)
Run `baton doctor` or `baton doctor --json`.

## Driving it with the PTY/CLI harness
```sh
tmp=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton doctor --json > /tmp/baton-doctor.json
jq -e '.schema_version == 1 and (.providers | length == 6) and (.db.thread_count == 0)' /tmp/baton-doctor.json
BATON_HOME="$tmp" target/debug/baton ps --json | jq -e '.processes == []'
```

## Gotchas
- Provider readiness still reads the real harness config and PATH. Only `BATON_HOME` and the database are isolated.
- Optional missing providers are shown as `installed: false`; they are not warnings unless the default provider is missing.
- A tracked process that is not alive is a warning, not a clean doctor report.
