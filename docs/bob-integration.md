# IBM Bob Shell Integration

Person 6's contribution to the GitLens pipeline model-provider abstraction.

---

## What was built

`pipeline/providers/bob.py` implements the `ModelProvider` ABC using **Bob Shell**
(the `bob` CLI) in non-interactive mode.  The interface is identical to
`GeminiProvider` — consumers call `provider.generate(prompt: str) -> str` and
never import Bob-specific code directly.

The factory in `pipeline/providers/__init__.py` selects `BobProvider` when
`MODEL_PROVIDER=bob` is set in the environment.

---

## What is officially documented vs. what was rejected

### Used — confirmed against official IBM docs

| Mechanism | Source |
|---|---|
| `bob -p "<prompt>"` for non-interactive prompts | [Starting a non-interactive session](https://bob.ibm.com/docs/shell/getting-started/start-bobshell-non-interactive) |
| `--auth-method api-key` flag (required for non-interactive) | [Install and set up Bob Shell § API key authentication](https://bob.ibm.com/docs/shell/getting-started/install-and-setup#api-key-authentication) |
| `BOB_API_KEY` environment variable (confirmed against Bob Shell 2.0.5) | Live run confirmed: `bob` 2.0.5 requires `BOB_API_KEY`; `BOBSHELL_API_KEY` causes an error if both are set |
| `--hide-intermediary-output` — outputs only the final answer | [Configuring § Command-line arguments](https://bob.ibm.com/docs/shell/configuration/configuring) |
| `--accept-license` one-time license acceptance | Same source |
| API key with **Scope = Inference** | [API keys](https://bob.ibm.com/docs/ide/account/api-keys) |

### Rejected — not used

**Unofficial raw HTTP endpoint.**  Several third-party repositories
reverse-engineer a direct HTTP call to what appears to be Bob's internal
inference REST API (bypassing the CLI entirely).  This approach was
**deliberately not used** because:

- It is not documented in any official IBM source.
- The endpoint path, request schema, and authentication headers could change
  without notice.
- Using it would violate the project rule: *"Do not invent an IBM Bob API,
  endpoint, SDK, or authentication mechanism beyond what is confirmed against
  official IBM docs."*

---

## Known correction vs. earlier skeleton

The original skeleton in `pipeline/providers/bob.py` had two bugs caught during
this implementation pass:

1. **Wrong env var name in docs**: docs described `BOBSHELL_API_KEY`, but live run against Bob Shell 2.0.5 confirmed the correct name is `BOB_API_KEY`. Fixed across all files.
2. **Missing auth flag**: non-interactive sessions require `--auth-method api-key`;
   omitting it causes Bob Shell to attempt browser-based SSO login, which hangs in
   automation.
3. **Delimiter hacking not needed**: the skeleton description proposed wrapping the
   prompt with delimiter strings to parse Bob's combined answer+reasoning output.
   The official `--hide-intermediary-output` flag already suppresses intermediary
   output, so the final stdout is the clean answer with no parsing required.

`pipeline/config.py` was also updated to use `BOB_API_KEY`.

---

## Final command invocation

```python
subprocess.run(
    [bob_path, "--auth-method", "api-key", "--hide-intermediary-output", "-p", prompt],
    capture_output=True,
    text=True,
    env={**os.environ, "BOB_API_KEY": self.api_key},
    timeout=120,
)
```

---

## Setup steps for a teammate with a real key

### 1. Install Bob Shell

**Windows (PowerShell):**
```powershell
powershell -ep Bypass 'irm -Uri "https://bob.ibm.com/download/bobshell.ps1" | iex'
```

**macOS / Linux:**
```bash
curl -fsSL https://bob.ibm.com/download/bobshell.sh | bash
```

Verify: `bob --version`

### 2. Create an Inference API key

1. Log in at [bob.ibm.com](https://bob.ibm.com).
2. Open your subscription instance → API key management.
3. Create a new key — choose **type: Inference** (no extra instance/team headers required).
4. Copy the key; you cannot view it again after creation.

### 3. Configure the environment

Create (or edit) `.env` in the repository root.  **Never commit this file** — it
is already listed in `.gitignore`.

```dotenv
MODEL_PROVIDER=bob
BOB_API_KEY=<your-inference-key-here>
```

### 4. Accept the license (once per machine)

```bash
bob --accept-license -p "hello"
```

### 5. Run the smoke test

```bash
cd pipeline
python test_provider.py
```

Expected output:
```
Provider loaded: BobProvider

Model response:
<Bob's answer to the commit explanation prompt>
```

### 6. Run the unit tests

```bash
cd pipeline
python -m pytest tests/test_providers.py -v
```

All 35 tests should pass in under 5 seconds with no network access.

---

## Wiring Person 6 → Person 4 (ModelProviderAdapter)

`narrative_sequencer.py` (Person 4) declares its own `ModelInterface` ABC with
`call_model(prompt, *, max_tokens=500, **kwargs) -> str`.  The pipeline providers
expose `generate(prompt) -> str`.  These are bridged by:

```
pipeline/providers/model_interface_adapter.py
```

### How to use it

Replace `EchoStubModel` in any call to `build_tour` / `NarrativeGenerator`:

```python
from pipeline.providers import get_provider
from pipeline.providers.model_interface_adapter import ModelProviderAdapter
from narrative_sequencer import build_tour

model = ModelProviderAdapter(get_provider())   # reads MODEL_PROVIDER from env
stops = build_tour(commits, model=model)
```

`get_provider()` selects `BobProvider` or `GeminiProvider` based on
`MODEL_PROVIDER` in the environment.  Person 4's `narrative_sequencer.py` is
**unchanged** — it still knows nothing about providers.

### Why max_tokens is ignored

`call_model` accepts `max_tokens` per Person 4's interface contract.  Neither
`BobProvider` nor `GeminiProvider` exposes a token-budget parameter through
`ModelProvider.generate()`.  Adding one would require coordinated changes across
all providers and both calling sites; that is out of scope for this pass.  The
argument is silently accepted so Person 4's callsites don't break if they pass it.

---

## Rust provider: `BobShellProvider` (Tauri backend)

The Rust side of the app (`src-tauri`) also has a `ModelProvider` trait in
`src-tauri/src/provider.rs`.  Person 3's significance-ranking agent and
file/commit summarisation stages call into it from async Tauri commands.

`src-tauri/src/bob_provider.rs` implements `ModelProvider` for Bob Shell:

```
src-tauri/src/bob_provider.rs   ← new
src-tauri/src/lib.rs            ← updated (wiring)
src-tauri/Cargo.toml            ← updated (tokio/process feature)
```

### How the Rust provider selects and invokes Bob Shell

`BobShellProvider::call(req)` does:

1. Concatenates `req.system` + `req.user` into a single prompt string.
2. If `req.schema` is present, appends a JSON-response instruction.
3. Runs `tokio::process::Command` (async, non-blocking):
   ```
   bob --auth-method api-key --hide-intermediary-output -p "<prompt>"
   ```
   with `BOB_API_KEY` injected into the child's environment.
4. On success, attempts to parse stdout as JSON if the request had a schema;
   places the result in `ModelResponse::parsed`.  Raw text is always in
   `ModelResponse::text`.

### Provider selection at app startup (`lib.rs`)

| Environment | Provider selected |
|---|---|
| `GITLENS_MOCK_PROVIDER=1` | `MockProvider` (test / CI — no Bob needed) |
| `MODEL_PROVIDER=bob` + `BOB_API_KEY=<key>` | `BobShellProvider` |
| anything else | `StubProvider` (returns a clear error on every call) |

### `BOB_PATH` override

Set `BOB_PATH=/path/to/bob` to use a non-`PATH` binary location.  Useful when
Bob Shell is installed to a non-standard location on the demo machine.

---

## Current sandbox status

The development machine has **no network egress to `bob.ibm.com`** and Rust /
Cargo are not installed, so neither a live end-to-end run nor a `cargo build`
has been performed in this sandbox.

- **Python tests**: 35 pass in < 3 s with full subprocess mocking.
- **Rust build**: must be verified on a machine with Rust installed.  Run
  `cargo build` in `src-tauri/` after installing the toolchain.
- **Live end-to-end**: requires Bob Shell installed + `BOB_API_KEY` set.
  Expected command to validate both layers:
  ```bash
  MODEL_PROVIDER=bob BOB_API_KEY=<key> python pipeline/test_provider.py
  ```

If the live run reveals a difference in Bob's stdout format (e.g. trailing ANSI
codes, a version-specific prefix), fix only `bob.py` (Python) or
`bob_provider.rs` (Rust), add a regression test, and update this doc with what
was actually observed.

---

## Files changed

| File | Change |
|---|---|
| `pipeline/providers/bob.py` | Full implementation (was `raise NotImplementedError`) |
| `pipeline/providers/model_interface_adapter.py` | Created — bridges Python `ModelProvider` → `ModelInterface` |
| `pipeline/config.py` | Corrected env var to `BOB_API_KEY` |
| `pipeline/tests/__init__.py` | Created (empty, marks directory as package) |
| `pipeline/tests/test_providers.py` | Created — 35 unit tests, fully mocked |
| `.env.example` | Populated with `MODEL_PROVIDER`, `GEMINI_API_KEY`, `BOB_API_KEY` |
| `src-tauri/src/bob_provider.rs` | Created — Rust `ModelProvider` backed by Bob Shell |
| `src-tauri/src/lib.rs` | Wires `BobShellProvider` when `MODEL_PROVIDER=bob` |
| `src-tauri/Cargo.toml` | Added `tokio` with `process` feature |
| `docs/bob-integration.md` | This document |

---

## Next step — Phase 4: Demo repository selection

Phase 4 is demo repository selection (Persons 3/4's task).  They need to pick a
real open-source repository whose git history will be analysed by the pipeline.
Recommended criteria:

- Public GitHub repo with a rich commit history (> 500 commits).
- Mix of feature commits, bug-fix commits, and refactors for varied tour stops.
- Well-known enough that the generated explanations can be fact-checked.

Suggested candidates: `django/django`, `pallets/flask`, `git/git`, or
`microsoft/vscode`.  Confirm the choice and the date range before starting
pipeline integration.
