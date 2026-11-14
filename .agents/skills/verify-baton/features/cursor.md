# Cursor

Baton runs `cursor-agent acp`. Cursor validates its own keys: the ICA key is
rejected and a custom `apiBaseUrl` is rejected before a turn starts. Use
`cursor-agent login` and subscription auth, or a valid Cursor key with no
`apiBaseUrl`. `baton login cursor` runs `cursor-agent login`.
Run `.agents/skills/verify-baton/scripts/cursor_probe.py` for the exact login
hint and capability rejection.
