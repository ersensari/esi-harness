# Studio test configuration isolation

On Linux, run Rust tests through the process wrapper from the Studio root:

```bash
source bin/activate-hermit
node scripts/test-isolated.mjs cargo test --locked -p goose --lib skills
node scripts/test-isolated.mjs cargo test --locked -p goose --test acp_custom_requests_test test_custom_list_builtin_skill_sources
node scripts/test-isolated.mjs cargo clippy --locked -p goose --lib -- -D warnings
pnpm --dir ui/desktop run test:run
node --test scripts/test-isolated.test.mjs
```

All executable Desktop test scripts now use the wrapper automatically:
normal/run/UI/coverage, integration (including watch/debug/provider variants),
and Playwright (including dev/UI/debug/single). Report viewing and the static
distribution check do not execute tests and are exempt. Vitest and Playwright
configs reject naked execution before loading tests. For direct Cargo or
direct Vitest/Playwright, prefix with `node scripts/test-isolated.mjs` from Studio root.
The working directory is unchanged; arguments are passed without a shell.

The wrapper allocates an absolute GOOSE_PATH_ROOT, XDG config/data/state/cache
roots and a temporary directory before starting the test process. It disables
keyring use and removes inherited plugin/additional-config overrides. The
original HOME, CARGO_HOME and RUSTUP_HOME are never reassigned. A config digest
checks the original Linux ESI config tree and any inherited GOOSE_PATH_ROOT
config tree before and after the child. Only the aggregate digest is used;
configuration contents and credentials are not printed. Symlinks are hashed
as links without traversing arbitrary targets. This is not an OS sandbox and
does not prevent arbitrary explicit writes or changes to symlink targets.

Child exit codes are preserved. A changed protected config produces a failure
and retains the test root for investigation. The wrapper does not overwrite
potential concurrent user edits to restore an old config. Otherwise it removes
only its own freshly allocated temporary root, including on spawn failure.
External background descendants must be stopped by their owning test fixture.

Rust library unit tests additionally get a process-local temporary fallback
from Paths when GOOSE_PATH_ROOT is absent/invalid. Explicit absolute overrides
remain supported. Production and integration-test library builds do not get
this cfg(test) fallback; use the wrapper for integration tests. A naked Rust
unit-test process may retain its small static temporary root until OS temp
cleanup; under the wrapper the enclosing temporary tree is removed.

Config layouts follow the pinned etcetera 0.11 choose_app_strategy source:
XDG on Unix including macOS; APPDATA/ESI/esi-studio/config on Windows.
APPDATA and LOCALAPPDATA are also isolated in the child. Desktop starts the
Vitest JS entry point through Node, avoiding Windows shell command shims.
Validation in this session is Linux-only; macOS/Windows execution and the full
integration suite are not claimed.

## POST-129 final writer/entry-point audit

The four exact historical test names were not retained in the old log. Do not
invent attribution. The disposable singleton reproduction demonstrates the
reported failure mechanism; this audit identifies current source paths.

| Current writer / entry | Configuration path and protection |
| --- | --- |
| `skills::{project_plugin_skill_is_rejected_by_source_crud_before_discovery,nested_project_plugin_skill_is_listed_read_only_and_rejected_by_source_crud,symlinked_project_plugin_skill_is_rejected_by_source_crud}` | Source CRUD returns to `Config::global`; discovery `filter_by_config` inserts plugin entries. A test-local injected Config is insufficient after singleton pinning. Unit-test Paths fallback plus process wrapper protects first initialization. |
| `plugins::discovery::{newly_discovered_plugin_is_added_to_config_as_enabled,disabled_in_config_drops_plugin,enabled_in_config_keeps_plugin_without_modifying_config}` and other discovery/install fixtures | Injected temp Config, explicit local git/plugin fixtures; all unit globals also use the test fallback. |
| `skills::{symlinked_user_plugin_skill_remains_writable_for_source_crud,ordinary_project_skill_under_plugin_manifest_remains_writable}` | Positive update/export/delete controls also re-enter global source discovery; the same singleton isolation applies. All 62 skill tests, including negative and positive CRUD paths, pass together. |
| `agent.rs::test_batch_summarization_preserves_all_summaries` and extension setup | Writes cutoff/global extension settings despite temp SessionManager. New before-workers constructor binds a fresh root, clears additional/plugin overrides and disables keyring. All 37 tests pass. |
| `acp_custom_requests_test`, `acp_custom_provider_methods_test`, `acp_secret_cache_invalidation_test` | ACP temp config helper / env_lock and shared serialized fixture; provider config and secrets writes must still run inside the process wrapper. Focused/full relevant integration targets validate them. |
| `model_profiles`, `esi_provider_profiles`, `esi_config_isolation`, `litellm_default_host`, `agent_manager_scheduler_disabled` | Explicit TempDir/env_lock or injected Config. POST-141 already validates profile persistence. |
| Remaining Rust integration/CLI/provider subprocesses | Production library build has no cfg(test) fallback. The supported entry is the process wrapper; never use naked `cargo test` for integration targets. CI's disposable runner is not a substitute for local isolation. |
| Desktop mocked unit tests | All script variants enter isolation before imports; fixed `/tmp/test-user-data` is a mocked Electron path, not an actual launched app. |
| Desktop provider integration | Wrapper config + explicit `ESI_TEST_LIVE_PROVIDERS=1` opt-in; no ambient dotenv/provider probing by default. OAuth discovery reads only `GOOSE_PATH_ROOT/config`, not real HOME. PLUGINS cannot be reintroduced from dotenv. |
| Playwright workers and `goosePage` Electron fixture | Config and fixture both check the isolated-runner contract. App subprocess inherits isolated XDG/Goose roots; no user provider config is copied. Tests requiring a provider need explicit fixture setup. |

The guard checks `ESI_STUDIO_TEST_ROOT`, matching absolute Goose root, disabled
keyring, empty additional-config/plugin overrides and isolated XDG/APPDATA paths.
These are accidental-leak safeguards, not a hostile-code security boundary.

Bounded alternate-entry checks (no external credentials, inference or browser):

```bash
pnpm --dir ui/desktop run test:coverage src/acp/__tests__/modelProfiles.test.ts src/components/settings/models/ModelProfileControls.test.tsx --maxWorkers=2
pnpm --dir ui/desktop run test:integration tests/integration/config-isolation.test.ts
pnpm --dir ui/desktop run test-e2e tests/e2e/config-isolation.spec.ts --reporter=list
```

The Playwright check executes a real worker and child config write, not the full
provider-dependent browser suite. POST-141 separately runs real packaged
Electron/backend acceptance with disposable config in both agent loops.
The selected coverage run verifies runner isolation, not whole-app coverage.
Node regressions enumerate every executable test script and actually invoke
three naked runner/config combinations to prove early rejection.
