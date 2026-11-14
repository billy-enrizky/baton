# Grok

Baton runs `grok agent stdio`. With `grok.mode=api`,
`apiKeyEnv=CODEX_ICA_API_KEY`, and
`apiBaseUrl=https://api.servicesessentials.ibm.com/v1`, baton sends
`XAI_API_KEY`, `GROK_XAI_API_BASE_URL`, and `GROK_MODELS_BASE_URL`.
Run `.agents/skills/verify-baton/scripts/grok_probe.py`; it passes on `GROK_OK`.
Model `gemini-3.7-flash` works on the ICA endpoint.
