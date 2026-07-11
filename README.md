# KRIS

A coding-assistant CLI written in Rust. By default it runs entirely offline
by talking to a `llama-server` (llama.cpp's OpenAI-compatible HTTP server,
started with `--jinja` for native tool-calling) running on the same
machine — no cloud API involved. Built primarily for running comfortably on
a phone under Termux with a small Qwen2.5-Coder model, but works on any
Linux/macOS box with llama.cpp installed.

It can also run **online**, sending the same conversation to a cloud model
through Google's Gemini API, Anthropic's Claude API, or OpenRouter (which
fronts many different providers' models behind one API and key) instead —
useful when you want a stronger model than the phone can host, or when you
don't have llama.cpp set up. Either way the tools (file edits, `run_command`,
git, …) still run locally on your machine; only the model's "thinking" moves
to the cloud. Switch at any time with `mode offline` / `mode online` /
`mode claude` / `mode openrouter`.

## What it does

- Streams the model's response token-by-token as it's generated, instead
  of waiting silently for a full reply (the slow part on a phone CPU).
- Uses llama.cpp's native, grammar-constrained tool-calling (via
  `--jinja`) rather than asking the model to hand-format JSON in plain
  text, so tool calls are reliable.
- Shows a colorized diff for every file it writes, edits, deletes, or
  moves — nothing changes on disk invisibly.
- Reuses llama-server's KV cache across turns (`cache_prompt`) so it isn't
  reprocessing the whole conversation from scratch every message.
- Tracks context usage from the exact prompt-token count llama-server
  reports on each reply (falling back to its `/tokenize` endpoint) and
  trims the oldest turns before overflowing the context window, instead of
  erroring out mid-conversation — without a spare round trip per turn.
- Self-heals: if llama-server was killed while KRIS was idle, it's
  restarted automatically on the next request.

## Build

```
cargo build --release
```

The binary is `target/release/kris`.

## Online mode (Gemini / Claude / OpenRouter)

> **Never put an API key in source code, a commit, or anywhere inside this
> repo.** All three online providers below read their key from an environment
> variable first, specifically so it never has to touch disk in plain text
> as part of a config file you might accidentally commit. If a key is ever
> pasted somewhere it could be logged or shared (a chat, an issue, a
> terminal recording), treat it as compromised and rotate it immediately
> in the provider's console.

### Gemini

KRIS can talk to Google's Gemini models through their OpenAI-compatible API
instead of a local llama-server. Get an API key from Google AI Studio, then:

```
export GEMINI_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL:

```
mode online          # switch to Gemini (offline is the default)
mode offline         # switch back to local llama.cpp
```

Configuration (all optional, saved to `config.toml`):

| Key                   | Default                            | Meaning                                     |
| --------------------- | ---------------------------------- | -------------------------------------------- |
| `provider`            | `local`                            | `local` (offline), `gemini` (online), `claude`, or `openrouter` |
| `gemini_model`        | `gemini-2.5-flash`                 | model id used online                        |
| `gemini_api_key`      | *(empty)*                          | fallback if `GEMINI_API_KEY` isn't set      |
| `gemini_context_size` | `128000`                           | history-trim budget in online mode          |
| `gemini_url`          | Gemini's `/v1beta/openai` endpoint | OpenAI-compatible base URL                   |

Set any of these with e.g. `config set gemini_model gemini-2.5-pro`. The
`GEMINI_API_KEY` environment variable takes precedence over the stored
`gemini_api_key`, so you can keep the key out of `config.toml` entirely; when
shown via the `config` command the stored key is masked. Because `gemini_url`
is just an OpenAI-compatible base URL, other compatible providers can work by
pointing it (and `gemini_model`/`GEMINI_API_KEY`) elsewhere.

### Claude

KRIS can also talk to Claude directly through Anthropic's native Messages
API (not an OpenAI-compatibility shim - a separate implementation that
speaks Claude's own request/response and streaming format). Get an API key
from [console.anthropic.com](https://console.anthropic.com), then:

```
export ANTHROPIC_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL:

```
mode claude          # switch to Claude
mode offline         # switch back to local llama.cpp
```

Configuration (all optional, saved to `config.toml`):

| Key                   | Default                     | Meaning                                   |
| ---------------------- | --------------------------- | ----------------------------------------- |
| `provider`             | `local`                     | `local`, `gemini`, `claude`, or `openrouter` |
| `claude_model`         | `claude-sonnet-5`           | model id used, e.g. `claude-opus-4-8`     |
| `claude_api_key`       | *(empty)*                   | fallback if `ANTHROPIC_API_KEY` isn't set |
| `claude_context_size`  | `200000`                    | history-trim budget in Claude mode        |
| `claude_url`           | `https://api.anthropic.com` | Claude API base URL                       |

Set any of these with e.g. `config set claude_model claude-opus-4-8`. Same
masking behavior as Gemini: the `config` command never prints the real key.

### OpenRouter

KRIS can also talk to [OpenRouter](https://openrouter.ai), which fronts many
different model providers (OpenAI, Anthropic, Google, Meta, and others)
behind one OpenAI-compatible API and key — handy for trying a model without
signing up for that provider directly. Get an API key from
[openrouter.ai/keys](https://openrouter.ai/keys), then:

```
export OPENROUTER_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL:

```
mode openrouter      # switch to OpenRouter
mode offline         # switch back to local llama.cpp
```

Configuration (all optional, saved to `config.toml`):

| Key                        | Default                         | Meaning                                     |
| -------------------------- | -------------------------------- | -------------------------------------------- |
| `provider`                 | `local`                          | `local`, `gemini`, `claude`, or `openrouter` |
| `openrouter_model`         | `openai/gpt-5`                   | model id used, e.g. `anthropic/claude-sonnet-5` |
| `openrouter_api_key`       | *(empty)*                        | fallback if `OPENROUTER_API_KEY` isn't set  |
| `openrouter_context_size`  | `128000`                         | history-trim budget in OpenRouter mode       |
| `openrouter_url`           | `https://openrouter.ai/api/v1`   | OpenAI-compatible base URL                   |

Set any of these with e.g. `config set openrouter_model anthropic/claude-sonnet-5`.
Same masking behavior as Gemini and Claude: the `config` command never
prints the real key.

## Running on Termux (Android)

### Quick setup (one command)

```
bash scripts/setup-termux.sh        # defaults to the 3B model
bash scripts/setup-termux.sh 1.5b   # smaller model, for phones with less RAM
bash scripts/setup-termux.sh 7b     # bigger/slower, for phones with plenty of RAM
```

This installs the required packages, builds `llama-server` (with the
`libandroid-spawn` fix applied, and `--jinja` support so tool-calling
works), downloads the GGUF model, starts `llama-server` in the background,
creates `~/project` (KRIS's default workspace — put the code you want it
to work on there), builds KRIS, symlinks it as `kris` on your `PATH`, and
drops you into the KRIS REPL. It's safe to re-run — steps that already
finished are skipped. The server keeps running in the background
afterwards, so next time you just type `kris` from anywhere.

### Manual setup (or if the script fails partway)

1. Install build tools and Rust. `libandroid-spawn` is required — without
   it, building `llama-server` fails with `fatal error: 'spawn.h' file not
   found`, because Android doesn't ship `posix_spawn` support by default:

   ```
   pkg update && pkg upgrade
   pkg install git cmake clang make rust libandroid-spawn
   ```

2. Clone and build llama.cpp **outside** of this repo (e.g. in your home
   directory — don't nest it inside `kris/`, they're unrelated git repos):

   ```
   cd ~
   git clone https://github.com/ggml-org/llama.cpp
   cd llama.cpp
   cmake -B build -DGGML_LLAMAFILE=OFF
   cmake --build build --config Release --target llama-server -j 2
   ```

   Use a low `-j` (2 or even 1) instead of `-j $(nproc)` — building with
   too many parallel jobs is a common way to get the compiler OOM-killed on
   a phone. If the build fails partway with no clear compiler error, rerun
   with `-j 1`.

   Verify it actually built before moving on:

   ```
   ls -la ~/llama.cpp/build/bin/llama-server
   ```

3. Get shared storage access and download a small GGUF coding model
   (`Q4_K_M` quantization keeps RAM usage down on-device):

   ```
   termux-setup-storage
   cd ~
   curl -L -o qwen2.5-coder-3b-instruct-q4_k_m.gguf \
       "https://huggingface.co/Qwen/Qwen2.5-Coder-3B-Instruct-GGUF/resolve/main/qwen2.5-coder-3b-instruct-q4_k_m.gguf?download=true"
   ```

   Double-check the download actually completed before starting the
   server — a truncated download still leaves a file behind, just a much
   smaller one:

   ```
   ls -la ~/*.gguf
   ```

4. Start the local inference server (keep this Termux session running),
   pointing `-m` at whichever `.gguf` you downloaded. `--jinja` is
   required — without it, KRIS falls back to a much less reliable
   plain-text tool-calling mode:

   ```
   ~/llama.cpp/build/bin/llama-server \
       -m ~/qwen2.5-coder-3b-instruct-q4_k_m.gguf \
       --host 127.0.0.1 --port 8080 -c 8192 --jinja
   ```

5. Open a new Termux session (swipe from the left edge to add one, so the
   server keeps running in the first), `cd` into this project, and build:

   ```
   cargo build --release
   mkdir -p ~/.config/kris
   cat > ~/.config/kris/config.toml <<'EOF'
   model_path = "/data/data/com.termux/files/home/qwen2.5-coder-3b-instruct-q4_k_m.gguf"
   llama_server_path = "/data/data/com.termux/files/home/llama.cpp/build/bin/llama-server"
   llama_url = "http://127.0.0.1:8080"
   context_size = 8192
   temperature = 0.2
   max_tokens = 4096
   mlock = false
   flash_attn = true
   cache_type_k = "q8_0"
   cache_type_v = "q8_0"
   workspace = "/data/data/com.termux/files/home/project"
   EOF
   mkdir -p ~/project
   ./target/release/kris
   ```

## Running on a normal Linux/macOS machine

Same idea, minus the Termux-specific packages: build llama.cpp's
`llama-server` normally (`cmake -B build && cmake --build build --target
llama-server`), start it with `--jinja`, then `cargo build --release` and
run `target/release/kris` — it defaults to `~/.config/kris/config.toml`
and prompts you to set `model_path` if it isn't configured yet.

## Using KRIS

```
kris                    # interactive REPL in the configured workspace
kris "explain main.rs"  # one-shot: ask, print the answer, exit
```

Inside the REPL:

| Command | Does |
|---|---|
| *(anything else)* | ask KRIS about the current project |
| `fix [notes]` | build and iteratively fix errors until it's clean |
| `health` | check whether llama-server is reachable |
| `serve` | start llama-server in the background if it isn't running |
| `model [preset]` | show/switch the Qwen2.5-Coder model (`1.5b`/`3b`/`7b`) |
| `workspace [path]` | show/switch the project KRIS is working in |
| `config [set key value]` | show or change settings (saved to `config.toml`) |
| `clear` | clear conversation history and the screen |
| `!<command>` | run a raw shell command directly, bypassing the model |
| `help` | show the command list |
| `exit` / `quit` | leave KRIS |

KRIS can read, search, write, and edit files; run shell commands (with a
y/n confirmation); inspect git state (read-only); and outline large files
without reading them in full. File writes/edits/deletes always print a
diff before applying so you can see exactly what changed.

## Troubleshooting: offline mode stuck "thinking" for a long time

If a plain message that used to answer in a few seconds now sits at
"thinking..." for minutes, check whether native tool-calling is actually
engaging on your `llama-server` build. Confirmed on a real device
(Qwen2.5-Coder-1.5B, llama.cpp build b9888): with `--jinja` on, some
model/template/build combinations still don't apply grammar-constrained
tool-calling - instead of using the structured `tool_calls` field, the
model falls back to writing a plain-text ` ```json ` fence attempting to
imitate one, with no natural stopping point, and just keeps generating
until `max_tokens`.

To check: hit `llama-server` directly, once with `tools` and once without,
and compare:

```
# baseline - should be a few seconds
curl -N http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"x","messages":[{"role":"user","content":"halo"}],"stream":true,"max_tokens":50}'

# with a tool attached
curl -N http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"x","messages":[{"role":"user","content":"halo"}],"stream":true,"max_tokens":50,"tools":[{"type":"function","function":{"name":"read_file","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}}],"tool_choice":"auto"}'
```

If the second one starts writing ` ```json ` as plain `content` instead of
a real `tool_calls` delta, that confirms it. What to try:

- `config set max_tokens 256` (or lower) to bound the worst case while you
  sort out the rest - this doesn't fix the cause, just the ceiling.
- Rebuild llama.cpp against the latest master (`cd ~/llama.cpp && git pull
  && cmake --build build --config Release --target llama-server -j 2`) -
  tool-calling grammar support for various chat templates has kept
  improving there.
- Try a different model size/quant - a GGUF's embedded chat template (not
  just the model weights) needs to define proper tool-call formatting for
  llama.cpp to build a grammar from it, and not every quantized upload
  ships one.

## Verifying changes to KRIS itself

`cargo build`, `cargo clippy --all-targets`, and `cargo test` all run
without needing llama.cpp or a model — the streaming/tool-call parser,
diff renderer, and filesystem tools all have unit and mock-server tests.
Exercising a real conversation end-to-end (`kris "..."` actually talking
to a model) needs a real `llama-server` + GGUF model, which only exists on
an actual Termux/Linux install with the setup above completed.
