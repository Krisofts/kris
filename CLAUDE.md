# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

KRIS is a Rust CLI coding assistant, modeled after Claude Code, built primarily to run on a phone under Termux against a local `llama-server` (llama.cpp) — but it also talks to Gemini, Claude, OpenRouter, Opper, or OpenCode Zen instead when online mode is selected. It has its own tool-calling agent loop, a REPL, and a set of file/git/shell tools — it is not a wrapper around another agent SDK.

## Commands

```
cargo build --release       # binary at target/release/kris
cargo test                   # unit tests (colocated in every module) + tests/agent_integration.rs
cargo test --lib <name>      # run a single unit test by name (substring match)
cargo test --test agent_integration <name>   # run a single integration test by name
cargo clippy --all-targets   # must be clean before shipping any change
cargo fmt                    # must be run before shipping any change; cargo fmt --check to verify only
```

All of the above run without needing llama.cpp or a real model — the streaming/tool-call parser, diff renderer, and filesystem tools are exercised with unit tests and a hand-rolled mock HTTP/SSE server (`tests/agent_integration.rs`) standing in for llama-server/Claude/etc. Exercising a real end-to-end conversation (`kris "..."` actually talking to a model) needs a real `llama-server` + GGUF model or a real provider API key, which only exists on an actual Termux/Linux install.

There is no separate lint config beyond clippy defaults, and no separate formatting config beyond `rustfmt` defaults.

## Architecture

### Request flow

`main.rs` loads `Settings` (config.rs) and hands off to `repl.rs`, either `run_interactive` (REPL loop) or `run_once` (single prompt, one-shot). Everything downstream of that is per-turn:

1. `repl.rs::ask_with_iterations` → `run_turn` builds an `Agent` (`server::client_for(&settings)` picks the `ModelClient` for whichever `Provider` is active) and calls `Agent::run`.
2. `agent.rs::Agent::run` is the actual agentic loop: pushes the user message, calls `ModelClient::chat_stream`, and for each returned tool call, executes it via `ToolRegistry` and appends the result back into history, looping until a plain-text final answer or `max_iterations` is hit.
3. `client.rs::ModelClient` is where the three very different wire protocols get unified into one `StreamOutcome` (content + tool_calls + prompt_tokens): `chat_stream_openai` (used for both local llama-server and every OpenAI-compatible online provider — Gemini/OpenRouter/Opper/OpenCode Zen all share this path, differing only in base URL/model/key) and `chat_stream_anthropic` (Claude's native Messages API — separate content-block-based streaming format, no leaked-tool-call heuristics needed since Claude never leaks a tool call into plain text).

### Provider abstraction is two-layered

`config::Provider` has six variants (`Local`, `Gemini`, `Claude`, `OpenRouter`, `Opper`, `Opencode`), each with its own URL/model/API-key/context-size settings fields — but they only ever map down to three `client::Backend` variants (`Llama`, `OpenAiCompat`, `Anthropic`). Adding a new OpenAI-compatible gateway provider means adding a `Provider` variant plus config fields and wiring it through `server::client_for`/`describe_mode`/etc. in `repl.rs` — it does **not** need any new code in `client.rs`, since `Backend::OpenAiCompat` already covers it.

### Context budget enforcement

`agent::enforce_context_budget` runs before every iteration inside the loop, not just once per turn. It extrapolates the current prompt size from the last exact `prompt_tokens` llama-server/the provider reported (avoiding a `/tokenize` round trip most of the time), and drops the *oldest complete turns* from history when over budget — but it can never touch the current in-progress turn. If the current turn's own accumulated tool output alone exceeds the budget (nothing older left to trim), it returns `true` and `run()` stops the turn with a clear message instead of sending a doomed, oversized request to the provider.

### Streaming robustness (client.rs)

There is deliberately no blanket `.timeout()` on the `reqwest::Client` — that would bound the entire request including body streaming, which is wrong for a slow-but-still-progressing local model on a phone CPU. Instead: `connect_timeout` bounds only the TCP handshake, and a per-chunk inactivity timeout (`stream_inactivity_timeout`, overridable via `with_stream_inactivity_timeout` for tests) inside the SSE read loop catches a stream that's gone genuinely silent. A held-back/live state machine (`apply_content_delta`) buffers content that *looks* like it might be a leaked tool call (starts with `{`, `` ` ``, or `<`) until either it resolves into a real tool call or a byte cap (`MAX_HELD_BACK_BYTES`) is hit — this only matters on the OpenAI-compatible path, since a local/ungrammar-constrained model can leak a tool call as plain text; Claude's path never needs it.

### Tools

`tools/mod.rs` defines the `Tool` trait and `ToolRegistry`, which exposes tool schemas in three different shapes depending on backend: `describe_all` (full JSON Schema, llama-server), `describe_all_gemini` (sanitized subset, wrapped OpenAI-style), `describe_all_anthropic` (sanitized subset, Claude's flat shape). Each tool lives in its own file under `tools/`: `fs.rs` (read/list/tree/find/search — all git-ignore-aware via the `ignore` crate), `edit.rs` (write/append/edit/delete/move/mkdir — all share one confirmation gate via `Rc<Cell<bool>>` so approving "always" once covers every mutating file tool for the session), `run_command.rs` (shell exec with a 120s timeout and non-blocking output capture for backgrounded processes), `git.rs` (read-only inspection + a separate confirmed commit tool), `outline.rs` (regex-based top-level-definition scan), `ask.rs` (lets the model pause and ask the user to pick between options via the same `picker.rs` widget the `project` command uses).

When walking the filesystem with `ignore::WalkBuilder`, do not pass `min_depth()` to exclude the walked root from results — the `ignore` crate only loads a directory's own `.gitignore` when its `WalkEvent::Dir` is processed, and `min_depth` suppresses that event for the root, silently disabling the root's own `.gitignore` rules for its direct children (this bit `tree()` in production: it exploded to hundreds of thousands of tokens on an ordinary git project because `/target` in the root `.gitignore` was never actually applied). Filter out the depth-0 entry from the collected results afterward instead.

### Session persistence

`session_store.rs` saves `Session::history` to `~/.config/kris/sessions/<sanitized-root-path>-<hash>.json` after every turn (`repl.rs::run_turn`, right after `agent.run` returns, regardless of Ok/Err - a failed turn can still have kept real progress) and loads it back in `Session::new`/`refresh_root`. Keyed by project root, not a single global file, so switching projects with `project <name>` resumes *that* project's own last conversation rather than sharing one history across all of them. Each session file also carries its own `root` (a `PersistedSession` envelope, not just the raw history array) so `session_store::list_sessions()` can enumerate every saved project without having to reverse the sanitized filename back into a path - that's what backs the `resume` REPL command (KRIS's counterpart to Claude Code's own `/resume`: picks a saved session via `picker.rs` and switches straight to it via `Session::switch_to_root`, which repoints `workspace` itself since a saved session's project isn't necessarily under the *current* workspace). `export` (KRIS's counterpart to Claude Code's `/export`, since Claude Code has no `/handoff`) renders history as readable Markdown via `export.rs` - a human-facing format, deliberately separate from `session_store`'s JSON, which only KRIS itself ever needs to read back. A missing or corrupt session file is never an error - `load` just falls back to an empty history. Tests that point `$HOME` at a scratch tempdir (session_store.rs and repl.rs both have one) must lock `crate::test_support::HOME_ENV_LOCK` - it's shared crate-wide on purpose, since `$HOME` is a single process-global value and a per-module lock alone doesn't stop one module's test from repointing it out from under another module's test running concurrently on a different thread.

### Message representation

`message.rs`'s `Message`/`ToolCall` are OpenAI-shaped internally (used for local + every OpenAI-compatible provider directly). `client.rs::to_anthropic_messages` converts that into Claude's native shape (system prompt pulled into its own field, content as typed blocks, consecutive same-role messages merged since Claude rejects repeats) only at the point of sending a Claude request — the rest of the codebase (agent loop, context budget, REPL) only ever deals with the OpenAI shape.

### Git workflow for this repo specifically

Development happens on a working branch; `main` is only fast-forward-merged into when explicitly asked. Before merging: `git fetch origin main <branch>`, confirm a clean fast-forward is possible (`git rev-list --left-right --count origin/main...origin/<branch>` showing 0 behind), merge with `--ff-only`, push, then switch back to the working branch.

### Verifying a fix before shipping it

The established discipline in this codebase: when fixing a bug, reproduce it first (a minimal script or a temporarily-reverted patch that makes the new regression test fail with the exact same symptom), then apply the real fix and confirm the same test now passes. Every bug fix ships with a regression test that encodes this proof, not just a patch.
