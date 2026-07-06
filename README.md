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

```
kris > workspace          # show detected project (Rust/Node/Laravel/...)
kris > ask fix the bug in src/main.rs
kris > reset              # clear the conversation history
```

`ask` runs an agent loop: the model can call `read_file`, `list_directory`,
`find_files`, `tree`, and `write_file` (scoped to the detected project root)
before giving its final answer.

## Performance tips (Termux)

CPU-only inference on a phone is the bottleneck, not KRIS itself. Things
worth trying, roughly in order of impact:

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
