# ESI-Studio Provider Profiles

`team.yaml` is the distribution default. It selects Goose's native ChatGPT
Codex browser OAuth provider. Team members can select `claude-acp` as the
supported alternative after authenticating the official Claude client and
installing its ACP adapter.

`operator.yaml` starts from the same safe default. Operators may additionally
select `codex-acp`, `openai`, `anthropic`, `litellm`, local providers such as
`ollama` or `lmstudio`, and any other Goose-supported provider through normal
Goose configuration. Private LiteLLM and ForgeLoop settings are never stored
in these profiles and must come from explicit operator configuration.

Use a profile as a lowest-precedence Goose configuration layer by setting
`GOOSE_ADDITIONAL_CONFIG_FILES` to its absolute path. User configuration and
environment variables continue to override it.

Authentication remains with the selected provider client. These profiles do
not read, copy, import, or redistribute Codex, Claude, ChatGPT, or API-key
credential files.