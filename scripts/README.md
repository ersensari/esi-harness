# Goose Benchmark Scripts

For isolated Rust/Desktop validation, see [test isolation](TEST_ISOLATION.md).

This directory contains scripts for running and analyzing Goose benchmarks.

## Deterministic recipe acceptance

`self-test-model-profiles.mjs` runs the real CLI `goose run --recipe
goose-self-test.yaml` tool loop with a scripted loopback provider and disposable
configuration. Its optional third argument selects `model-profiles` (default)
or `state-concurrency` or `tool-authority`; each phase executes three real isolated Rust test
commands and writes two reports. This tests tools/recipe wiring, not LLM quality.
Build the CLI first and activate Hermit, then run:

```bash
node scripts/test-isolated.mjs node scripts/self-test-model-profiles.mjs /absolute/path/to/goose /absolute/path/to/report.json state-concurrency
```

The fixture requires the Studio source/toolchain, accepts only the named phases,
and does not register a provider in the user's normal configuration.

The `tool-authority` phase includes actual Desktop both-loop/shared-dispatch
and ACP app-call fixtures, host human-approval receipts, revocation/forgery,
factory provenance and Linux bubblewrap containment. It requires `/usr/bin/bwrap`
and `/usr/bin/python3`; do not skip failures or substitute unrestricted tools.
Its CLI driver is intentionally unmanaged so it can run build/test commands;
the Rust fixtures explicitly construct managed Desktop agents. Native official
Codex/Claude delegation is a separately trusted boundary, not sandbox coverage.

## Installed Wiki release acceptance (Linux)

`accept-wiki-release.mjs` tests a packaged Desktop executable against a
disposable real Wiki database/service, without provider inference or real user
credentials. Requires Docker, the locally built `forgeloop-ai-esi-wiki:local`
image, PostgreSQL 17 Alpine image, Xvfb, and Desktop Playwright dependencies.
Run from Studio with the Hermit environment active:

```bash
node --test scripts/wiki-release-ownership.test.mjs
xvfb-run -a node scripts/test-isolated.mjs node scripts/accept-wiki-release.mjs /absolute/path/to/esi-studio /absolute/path/to/acceptance-report.json
```

The fixture uses an internal network, isolated app/config roots, disposable
authorization, and exact ownership-label checks before cleanup. It exercises
the visible renewal form, trusted ACP plan capture/revision, explicit MCP
reconnect, fresh-process retrieval and profile/transcript privacy. No model
turn or automatic existing-client header refresh is claimed. Chromium sandbox
protections must remain enabled; a per-user installation needs a valid
root-owned sandbox helper. The release uses renderer CDP instead of changing
Electron's disabled Node inspector fuses.

## run-benchmarks.sh

This script runs Goose benchmarks across multiple provider:model pairs and analyzes the results.

### Prerequisites

- Goose CLI must be built or installed
- `jq` command-line tool for JSON processing (optional, but recommended for result analysis)

### Usage

```bash
./scripts/run-benchmarks.sh [options]
```

#### Options

- `-p, --provider-models`: Comma-separated list of provider:model pairs (e.g., 'openai:gpt-4o,anthropic:claude-sonnet-4')
- `-s, --suites`: Comma-separated list of benchmark suites to run (e.g., 'core,small_models')
- `-o, --output-dir`: Directory to store benchmark results (default: './benchmark-results')
- `-d, --debug`: Use debug build instead of release build
- `-h, --help`: Show help message

#### Examples

```bash
# Run with release build (default)
./scripts/run-benchmarks.sh --provider-models 'openai:gpt-4o,anthropic:claude-sonnet-4' --suites 'core,small_models'

# Run with debug build
./scripts/run-benchmarks.sh --provider-models 'openai:gpt-4o' --suites 'core' --debug
```

### How It Works

The script:
1. Parses the provider:model pairs and benchmark suites
2. Determines whether to use the debug or release binary
3. For each provider:model pair:
   - Sets the `GOOSE_PROVIDER` and `GOOSE_MODEL` environment variables
   - Runs the benchmark with the specified suites
   - Analyzes the results for failures
4. Generates a summary of all benchmark runs

### Output

The script creates the following files in the output directory:

- `summary.md`: A summary of all benchmark results
- `{provider}-{model}.json`: Raw JSON output from each benchmark run
- `{provider}-{model}-analysis.txt`: Analysis of each benchmark run

### Exit Codes

- `0`: All benchmarks completed successfully
- `1`: One or more benchmarks failed

## parse-benchmark-results.sh

This script analyzes a single benchmark JSON result file and identifies any failures.

### Usage

```bash
./scripts/parse-benchmark-results.sh path/to/benchmark-results.json
```

### Output

The script outputs an analysis of the benchmark results to stdout, including:

- Basic information about the benchmark run
- Results for each evaluation in each suite
- Summary of passed and failed metrics

### Exit Codes

- `0`: All metrics passed successfully
- `1`: One or more metrics failed
