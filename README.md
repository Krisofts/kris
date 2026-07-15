# KRIS

A coding-assistant CLI written in Rust, modeled after Claude Code. KRIS is
**online-only**: every request goes to a cloud provider - Google's Gemini
API, Anthropic's Claude API, or a multi-model gateway like OpenRouter,
Opper, or OpenCode Zen (each fronts many different providers' models
behind one API and key). The tools (file edits, `run_command`, git, …)
still run locally on your machine; only the model's "thinking" happens in
the cloud. Built with phones under Termux in mind as much as any other
machine. Switch providers at any time with `mode gemini` / `mode claude` /
`mode openrouter` / `mode opper` / `mode opencode`.

## What it does

- Streams the model's response token-by-token as it's generated, instead
  of waiting silently for a full reply.
- Uses native, structured tool-calling rather than asking the model to
  hand-format JSON in plain text, so tool calls are reliable.
- Shows a colorized diff for every file it writes, edits, deletes, or
  moves — nothing changes on disk invisibly.
- Tracks context usage from the exact prompt-token count the provider
  reports on each reply and trims the oldest turns before overflowing the
  context window, instead of erroring out mid-conversation — without a
  spare round trip per turn.
- Self-heals: retries a flaky connection to the active provider a few
  times with backoff before giving up.
- Persists each project's conversation to disk after every turn, so
  closing KRIS (or having it killed - a backgrounded Termux app reaped, a
  crash) doesn't lose it. Switching projects with `project <name>` resumes
  *that* project's own last conversation instead of starting blank; `clear`
  wipes it for good.
- Seeds every brand-new, empty project with its own `KRIS.md` (house rules:
  keep files under 300 lines, always run and smoke-test before finishing)
  and folds a project's `KRIS.md` straight into every session so its
  conventions actually get followed, not just written once and forgotten.
  Customize `~/.config/kris/KRIS.md` to change the default template.

## Build

```
cargo build --release
```

The binary is `target/release/kris`.

## Providers (Gemini / Claude / OpenRouter / Opper / OpenCode Zen)

> **Never put an API key in source code, a commit, or anywhere inside this
> repo.** All five providers below read their key from an environment
> variable first, specifically so it never has to touch disk in plain text
> as part of a config file you might accidentally commit. If a key is ever
> pasted somewhere it could be logged or shared (a chat, an issue, a
> terminal recording), treat it as compromised and rotate it immediately
> in the provider's console.

### OpenRouter (the default)

KRIS talks to [OpenRouter](https://openrouter.ai) by default, which fronts
many different model providers (OpenAI, Anthropic, Google, Meta, and
others) behind one OpenAI-compatible API and key — handy for trying a
model without signing up for that provider directly, and it has genuinely
free (`:free`-suffixed) models. Get an API key from
[openrouter.ai/keys](https://openrouter.ai/keys), then:

```
export OPENROUTER_API_KEY=your-key-here     # preferred: never written to disk
```

Configuration (all optional, saved to `config.toml`):

| Key                        | Default                         | Meaning                                     |
| -------------------------- | -------------------------------- | -------------------------------------------- |
| `provider`                 | `openrouter`                     | `gemini`, `claude`, `openrouter`, `opper`, or `opencode` |
| `openrouter_model`         | `openai/gpt-5`                   | model id used, e.g. `anthropic/claude-sonnet-5` |
| `openrouter_api_key`       | *(empty)*                        | fallback if `OPENROUTER_API_KEY` isn't set  |
| `openrouter_context_size`  | `128000`                         | history-trim budget for OpenRouter           |
| `openrouter_url`           | `https://openrouter.ai/api/v1`   | OpenAI-compatible base URL                   |
| `openrouter_reasoning_effort` | *(empty)*                     | `none`/`minimal`/`low`/`medium`/`high`, or empty to omit |

Set any of these with e.g. `config set openrouter_model anthropic/claude-sonnet-5`.
The `config` command never prints the real key.

### Gemini

KRIS can talk to Google's Gemini models through their OpenAI-compatible
API. Get an API key from Google AI Studio, then:

```
export GEMINI_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL: `mode gemini`

Configuration (all optional, saved to `config.toml`):

| Key                   | Default                            | Meaning                                     |
| --------------------- | ---------------------------------- | -------------------------------------------- |
| `gemini_model`        | `gemini-2.5-flash`                 | model id used                                |
| `gemini_api_key`      | *(empty)*                          | fallback if `GEMINI_API_KEY` isn't set      |
| `gemini_context_size` | `128000`                           | history-trim budget for Gemini               |
| `gemini_url`          | Gemini's `/v1beta/openai` endpoint | OpenAI-compatible base URL                   |

The `GEMINI_API_KEY` environment variable takes precedence over the stored
`gemini_api_key`, so you can keep the key out of `config.toml` entirely.
Because `gemini_url` is just an OpenAI-compatible base URL, other
compatible providers can work by pointing it (and `gemini_model`/
`GEMINI_API_KEY`) elsewhere.

### Claude

KRIS can also talk to Claude directly through Anthropic's native Messages
API (not an OpenAI-compatibility shim - a separate implementation that
speaks Claude's own request/response and streaming format). Get an API key
from [console.anthropic.com](https://console.anthropic.com), then:

```
export ANTHROPIC_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL: `mode claude`

Configuration (all optional, saved to `config.toml`):

| Key                   | Default                     | Meaning                                   |
| ---------------------- | --------------------------- | ----------------------------------------- |
| `claude_model`         | `claude-sonnet-5`           | model id used, e.g. `claude-opus-4-8`     |
| `claude_api_key`       | *(empty)*                   | fallback if `ANTHROPIC_API_KEY` isn't set |
| `claude_context_size`  | `200000`                    | history-trim budget for Claude            |
| `claude_url`           | `https://api.anthropic.com` | Claude API base URL                       |

### Opper

KRIS can also talk to [Opper](https://opper.ai), another gateway that fronts
many different model providers behind one OpenAI-compatible API and key,
with its own model-routing and observability features. Get an API key from
Opper's dashboard, then:

```
export OPPER_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL: `mode opper`

Configuration (all optional, saved to `config.toml`):

| Key                    | Default                         | Meaning                                     |
| ---------------------- | -------------------------------- | -------------------------------------------- |
| `opper_model`          | `anthropic/claude-haiku-4-5`      | model id used, e.g. `mistral/mistral-large-latest` - defaults to the cheapest confirmed model since Opper has no dedicated free tier model |
| `opper_api_key`        | *(empty)*                        | fallback if `OPPER_API_KEY` isn't set        |
| `opper_context_size`   | `128000`                         | history-trim budget for Opper                |
| `opper_url`            | `https://api.opper.ai/v3/compat` | OpenAI-compatible base URL                   |

### OpenCode Zen

KRIS can also talk to [OpenCode Zen](https://opencode.ai/docs/zen/), a
hosted model gateway from the OpenCode project (a separate CLI coding
agent - Zen is just its model API, not that CLI) that includes a rotating
handful of genuinely free models alongside paid ones. Get an API key from
Zen's dashboard, then:

```
export OPENCODE_API_KEY=your-key-here     # preferred: never written to disk
```

In the REPL: `mode opencode`

Configuration (all optional, saved to `config.toml`):

| Key                       | Default                     | Meaning                                     |
| ------------------------- | ---------------------------- | -------------------------------------------- |
| `opencode_model`          | `big-pickle`                 | model id used - a free-for-a-limited-time model; see Zen's docs for the current list |
| `opencode_api_key`        | *(empty)*                    | fallback if `OPENCODE_API_KEY` isn't set     |
| `opencode_context_size`   | `128000`                     | history-trim budget for OpenCode Zen         |
| `opencode_url`            | `https://opencode.ai/zen/v1` | OpenAI-compatible base URL                   |

#### Reasoning models (e.g. Tencent's Hy3)

A reasoning model can spend its whole `max_tokens` budget on a hidden
"thinking" trace before ever writing a visible answer or tool call, which
otherwise looks like an empty reply after a long wait. KRIS surfaces a
diagnostic instead of silence if a reply is truncated with no content at
all. If it's still cutting off before answering, cap how much of the
budget reasoning is allowed to use:

```
config set openrouter_reasoning_effort low
config set max_tokens 8000
```

## Running on Termux (Android)

```
pkg update && pkg upgrade
pkg install git rust
git clone <this-repo-url> ~/kris
cd ~/kris
cargo build --release
mkdir -p ~/.config/kris ~/projects
export OPENROUTER_API_KEY=your-key-here    # or whichever provider you prefer
./target/release/kris
```

`ln -sf ~/kris/target/release/kris $PREFIX/bin/kris` puts `kris` on your
`PATH` so you can just type `kris` from anywhere afterward. `~/projects` is
KRIS's default projects folder (`config set workspace <path>` to change
it) - put the code you want it to work on there, one subfolder per
project.

## Running on a normal Linux/macOS machine

Same idea: `cargo build --release`, export an API key for whichever
provider you want, then run `target/release/kris` — it defaults to
`~/.config/kris/config.toml` and prompts you to set an API key if one
isn't configured yet.

## Using KRIS

```
kris                    # interactive REPL in the current project
kris "explain main.rs"  # one-shot: ask, print the answer, exit
```

Inside the REPL:

| Command | Does |
|---|---|
| *(anything else)* | ask KRIS about the current project |
| `fix [notes]` | build and iteratively fix errors until it's clean |
| `init` | explore the project and write/update `KRIS.md` with a summary for future turns |
| `review [notes]` | review pending changes (`git diff`) for correctness bugs and simplification opportunities |
| `security-review [notes]` | review pending changes (`git diff`) for security issues |
| `health` | check whether the active provider has an API key configured |
| `mode [gemini\|claude\|openrouter\|opper\|opencode]` | show/switch between providers |
| `project [name\|path]` | pick a project with arrow keys, switch straight to `<name>`, or pass a `<path>` to change the projects folder itself |
| `resume` | pick a saved session (any project, most recently used first) with arrow keys and switch straight to it |
| `export [filename]` | save the current conversation as readable Markdown (defaults to `kris-export-<timestamp>.md` in the project root) |
| `compact [instructions]` | ask the model to summarize the conversation so far, then continue from that recap instead of the full history |
| `config [set key value]` | show or change settings (saved to `config.toml`) |
| `clear` | clear conversation history and the screen (and its saved session on disk) |
| `!<command>` | run a raw shell command directly, bypassing the model |
| `help` | show the command list |
| `exit` / `quit` | leave KRIS |

Press **Tab** at the prompt to complete a command name (`he` + Tab -&gt;
`health`/`help`) or accept a suggestion from your command history (shown
as ghost text once you've typed a matching prefix before), then **Enter**
to run it.

KRIS can read, search, write, and edit files; run shell commands (with a
y/n confirmation); inspect git state (read-only); and outline large files
without reading them in full. File writes/edits/deletes always print a
diff before applying so you can see exactly what changed. It can also
pause and ask *you* a clarifying question - an arrow-key pick between a
few options (one may be marked recommended), with a plain numbered
fallback and a free-text "Other" answer always available - instead of
guessing when a request is genuinely ambiguous.

## Verifying changes to KRIS itself

`cargo build`, `cargo clippy --all-targets`, and `cargo test` all run
without needing a real API key or network access — the streaming/
tool-call parser, diff renderer, and filesystem tools all have unit and
mock-server tests. Exercising a real conversation end-to-end (`kris "..."`
actually talking to a model) needs a real API key for whichever provider
is active.
