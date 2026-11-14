# Antigravity

## Sub-features
- The CLI is `agy`; baton spawns `agy acp --gemini-dir <profile> --app-data-dir antigravity-acp`
- The ACP server requires an explicit `authenticate` with `gemini-api-key` on a fresh profile
- The model is a config family selected with `session/set_config_option` (`configId: "model"`), not `session/set_model`
- `apiKeyEnv` names the variable baton reads; the child receives `GEMINI_API_KEY`
- `apiBaseUrl` maps to `GOOGLE_GEMINI_BASE_URL`; it is the endpoint root, not the OpenAI `/v1` base

## How to get to it
```sh
baton exec -p antigravity "Reply with exactly AGY_OK"
```

## Driving it with the PTY/CLI harness
```sh
CODEX_ICA_API_KEY=... .agents/skills/verify-baton/scripts/antigravity_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe writes a throwaway config with `mode: "api"`, `apiKeyEnv: "CODEX_ICA_API_KEY"`, the ICA root URL, and `gemini-3.7-flash`, runs one turn, and passes only when the reply is `AGY_OK`, the phase is completed, the error is null, exit is 0, and `baton ps --json` is empty. It spends one small Gemini turn.

## Gotchas
- The base URL is the host root. `agy` appends `/v1beta/models/...`; adding `/v1` returns HTTP 404.
- `gemini-3.8-flash` is not available to the ICA key even though it appears in the catalog. `gemini-3.7-flash` works and resolves to a thinking variant.
- A fresh ACP profile answers `Authentication required` until the client sends `authenticate {methodId: "gemini-api-key"}`. A stored OAuth profile needs no authenticate call.
- Do not use `session/set_model` with an Antigravity `-high` id; the server silently keeps its default. Use the family with `session/set_config_option`.
- The user's `~/.gemini` profile is not used; baton keeps an isolated profile under `~/.baton/antigravity/gemini`.
