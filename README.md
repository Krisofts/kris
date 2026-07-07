# KRIS

A local coding-assistant CLI written in Rust. It runs entirely offline by
talking to a `llama-server` (llama.cpp's OpenAI-compatible HTTP server)
running on the same machine — no cloud API involved.

## Build

```
cargo build --release
```

The binary is `target/release/kris`.

## Running on Termux (Android)

### Quick setup (one command)

```
bash scripts/setup-termux.sh        # defaults to the 3B model
bash scripts/setup-termux.sh 1.5b   # or the smaller 1.5B model, for phones with less RAM
```

This installs the required packages, builds `llama-server` (with the
`libandroid-spawn` fix applied), downloads the GGUF model, starts
`llama-server` in the background, creates `~/project` (KRIS's default
workspace — put the code you want it to work on there), builds KRIS,
symlinks it as `kris` on your `PATH`, and drops you into the KRIS REPL.
It's safe to re-run — steps that already finished (packages, the
llama.cpp build, the model download, an already-running server) are
skipped. The server keeps running in the background afterwards, so next
time you just need to type `kris` from anywhere.

### Manual setup (or if the script fails partway)

1. Install build tools and Rust. `libandroid-spawn` is required — without it,
   building `llama-server` fails with `fatal error: 'spawn.h' file not
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

   Use a low `-j` (2 or even 1) instead of `-j $(nproc)` — building with too
   many parallel jobs is a common way to get the compiler OOM-killed on a
   phone. If the build fails partway with no clear compiler error (the shell
   just returns to the prompt), that's usually OOM: rerun with `-j 1`.

   Verify it actually built before moving on:

   ```
   ls -la ~/llama.cpp/build/bin/llama-server
   ```

3. Get shared storage access and download a small GGUF coding model
   (`Q4_K_M` quantization keeps RAM usage down on-device):

   ```
   termux-setup-storage
   cd ~

   # ~2GB, needs more RAM but better quality
   curl -L -o qwen2.5-coder-3b-instruct-q4_k_m.gguf \
       "https://huggingface.co/Qwen/Qwen2.5-Coder-3B-Instruct-GGUF/resolve/main/qwen2.5-coder-3b-instruct-q4_k_m.gguf?download=true"

   # or ~1GB, for phones with limited RAM
   curl -L -o qwen2.5-coder-1.5b-instruct-q4_k_m.gguf \
       "https://huggingface.co/Qwen/Qwen2.5-Coder-1.5B-Instruct-GGUF/resolve/main/qwen2.5-coder-1.5b-instruct-q4_k_m.gguf?download=true"
   ```

   Double-check the download actually completed before starting the server —
   a truncated download still leaves a file behind, just a much smaller one:

   ```
   ls -la ~/*.gguf
   ```

4. Start the local inference server (keep this Termux session running),
   pointing `-m` at whichever `.gguf` you downloaded:

   ```
   ~/llama.cpp/build/bin/llama-server \
       -m ~/qwen2.5-coder-3b-instruct-q4_k_m.gguf \
       --host 127.0.0.1 --port 8080 -c 4096
   ```

5. Open a new Termux session (swipe from the left edge to add one, so the
   server keeps running in the first), `cd` into this project, build KRIS,
   and put it on your `PATH` so you can just type `kris`:

   ```
   cd ~/kris
   cargo build --release
   mkdir -p "$PREFIX/bin"
   ln -sf "$PWD/target/release/kris" "$PREFIX/bin/kris"
   kris
   ```

6. Point KRIS at the server/model if you changed the defaults:

   ```
   kris > config set llama_url http://127.0.0.1:8080
   kris > config set model qwen2.5-coder-3b-instruct
   ```

   Settings persist to `~/.kris/config.toml`.

## Usage

By default KRIS opens `$HOME/project` (e.g. `~/project` on Termux) as the
workspace. Put the code you want KRIS to work on there, or point it at a
different folder with an argument:

```
kris /path/to/your/project
```

To switch workspace from inside KRIS without typing a full path, run
`workspace` on its own - it lists every subfolder of `$HOME/project` as a
numbered menu, and `workspace <number>` switches to whichever one you
picked.

```
kris > workspace          # show current workspace + a numbered list of switchable folders
kris > workspace 2        # switch to folder #2 from that list - no path typing needed
kris > workspace <path>   # or switch by path (~/ and relative-to-$HOME both work)
kris > model              # list available offline models and which are downloaded
kris > health             # check whether llama-server is reachable
kris > serve              # start llama-server in the background if it isn't running
kris > ask fix the bug in src/main.rs
kris > fix                # build the project and iteratively fix errors until it works
kris > reset              # clear the conversation history
```

`model <1|2|3|4>` switches between:

1. Qwen2.5-Coder-1.5B-Instruct
2. Qwen2.5-Coder-3B-Instruct
3. Qwen2.5-Coder-7B-Instruct
4. Qwen3-Coder-30B-A3B-Instruct (MoE, newer generation, ~18.6GB download -
   the "30B" is total parameters across all experts, not what's active per
   token, but llama.cpp still needs to hold the whole thing in RAM/mmap it,
   so this is realistically a PC/laptop/high-RAM-device option, not a
   typical phone)

`model` only updates `model`/`model_path` in the config (downloading the
`.gguf` first if needed, with the exact `curl` command printed for you).
Since llama-server can't swap models without restarting, stop it (Ctrl-C
in its session, or `pkill -f llama-server`) and run `serve` again
afterwards. For on-device/phone use, Qwen2.5-Coder (1-3) remains the pick -
there's no dedicated small "coder" fine-tune newer than that as of this
writing; the rest of the Qwen3-Coder lineup is exclusively large
mixture-of-experts models.

`serve` needs `llama_server_path` and `model_path` configured (the setup
script does this for you automatically; set them manually otherwise):

```
kris > config set llama_server_path ~/llama.cpp/build/bin/llama-server
kris > config set model_path ~/qwen2.5-coder-3b-instruct-q4_k_m.gguf
kris > serve
```

Other server-related settings: `context_size` (default 4096), `threads`
(default `auto`), `mlock` (default `false`), `flash_attn` (default `false`),
`cache_type_k`/`cache_type_v` (default `auto`) - see the Performance tips
section below for what these do.

`ask` and `fix` no longer require `llama-server` to already be running: if
it isn't reachable, KRIS automatically runs the same startup logic as
`serve` first (using `llama_server_path`/`model_path` from config) and only
then sends the request - so a fresh session just needs `kris > ask ...`,
not `serve` followed by `ask`.

Anything typed that isn't a built-in command is run as a real shell command
inside the current workspace - `ls -la`, `git status`, `cargo build`,
`npm install`, `python3 script.py`, or anything else Termux has installed
all just work directly from the KRIS prompt.

Both `ask` and `fix` run an agent loop: the model can call tools (scoped to
the detected project root) before giving its final answer. `fix` is `ask`
pre-loaded with an instruction to build the project, fix errors one at a
time, and rebuild until it succeeds (and tests pass) - allowing many more
tool-call rounds than `ask`'s default, since fixing several errors can take
a while. The system prompt also gets a short language-specific hint (e.g.
`cargo check`/`build`/`test` for Rust, `npm install`/`run build`/`test` for
Node) based on the detected project type.

Available tools:

- `read_file`, `list_directory`, `find_files`, `tree` - browse the project
- `write_file` - create or overwrite a file
- `edit_file` - replace an exact snippet inside an existing file
- `create_directory` - create a directory (and missing parents)
- `delete_file` - delete a single file
- `delete_directory` - delete a directory and everything inside it (refuses
  to delete the project root itself)
- `move_file` - move or rename a file or directory
- `search_code` - grep file contents by regex across the project
- `outline_file` - quick list of a file's top-level functions/classes/structs
  (simple pattern matching, not a full parser) without reading the whole
  file - handy for orienting in a large file first
- `git` - read-only `status`/`diff`/`log`/`show`/`branch`. Never modifies
  anything, so it runs without a confirmation prompt
- `run_command` - run a shell command (e.g. `cargo build`, `cargo test`).
  **Always asks for a y/n confirmation before executing anything** - review
  the command before approving it. Answer `a` instead of `y` to approve
  every command for the rest of that `ask`/`fix` call (useful for `fix`,
  which may need to build several times in a row). Killed after 2 minutes
  if it hasn't finished - background long-running processes yourself
  (e.g. `tmux new-session -d -s preview 'npm run dev'`) instead of relying
  on this tool for them.

## Performance tips (Termux)

CPU-only inference on a phone is the bottleneck, not KRIS itself. Running a
heavy build (`cargo build` via `run_command`) at the same time llama-server
is holding the model in memory can briefly starve it of CPU/RAM - KRIS
automatically retries a request a few times with backoff if the connection
to llama-server drops, which recovers from most of these blips on its own.
If `ask`/`fix` still reports a connection error after retrying, run
`health` to confirm, and `serve` to bring it back up if it's really down.

Things worth trying, roughly in order of impact:

- **Quantize the KV cache**: `CACHE_TYPE_K=q8_0 CACHE_TYPE_V=q8_0 bash
  scripts/setup-termux.sh` (or `kris > config set cache_type_k q8_0` /
  `cache_type_v q8_0` on an existing setup, then restart `serve`). This is
  usually the best memory-per-quality trade available: it roughly halves
  the KV cache's memory footprint (the part of memory that grows with
  context length and conversation history) for a quality hit that's
  normally hard to notice - unlike switching to a smaller model, the model
  weights themselves don't change. The freed-up memory can go toward a
  larger `context_size` instead, or just toward headroom so `llama-server`
  doesn't get starved when a build is running at the same time. `q4_0` is
  more aggressive (roughly a quarter of f16) with a more noticeable quality
  cost - only reach for it if `q8_0` isn't enough.
- **Enable flash attention**: `FLASH_ATTN=1 bash scripts/setup-termux.sh`
  (or `kris > config set flash_attn true`). Speeds up attention computation
  with no quality loss - it's opt-in rather than the default only because
  it needs a reasonably recent llama.cpp build; if `serve` fails after
  turning it on, rebuild llama.cpp from the latest master. Combine this
  with KV cache quantization above for the best of both.
- **Use the 1.5B model** (`bash scripts/setup-termux.sh 1.5b`) if the 3B
  model feels too slow — it generates tokens noticeably faster at some
  quality cost.
- **Keep Termux in the foreground** (or run `termux-wake-lock`) while
  `llama-server` is generating — Android throttles CPU heavily for
  backgrounded apps, which can make inference dramatically slower.
- **Tune thread count**: `THREADS=4 bash scripts/setup-termux.sh` (passed
  as `-t` to `llama-server`). More threads isn't always faster on
  big.LITTLE phone chips — pinning to just the performance cores can beat
  using every core. Try a few values and compare.
- **Lower the context size** if you don't need long conversations:
  `CTX_SIZE=2048 bash scripts/setup-termux.sh` — less memory and faster
  prompt processing.
- **Lock the model in RAM**: `MLOCK=1 bash scripts/setup-termux.sh` (only
  if you have enough free RAM to spare — it prevents swap-related jitter
  but can make things worse if memory is already tight).
- **Cap response length** if answers tend to ramble on: `kris > config set
  max_tokens 512` (defaults to `1024`).

I haven't been able to benchmark these directly on a phone, so treat them
as a starting point to experiment from rather than guaranteed wins.
