# KRIS

A local coding-assistant CLI written in Rust. It runs entirely offline by
talking to a `llama-server` (llama.cpp's OpenAI-compatible HTTP server)
running on the same machine — no cloud API involved.

## Build

```
cargo build --release
```

The binary is `target/release/kris-cli`.

## Running on Termux (Android)

### Quick setup (one command)

```
bash scripts/setup-termux.sh        # defaults to the 3B model
bash scripts/setup-termux.sh 1.5b   # or the smaller 1.5B model, for phones with less RAM
```

This installs the required packages, builds `llama-server` (with the
`libandroid-spawn` fix applied), downloads the GGUF model, starts
`llama-server` in the background, builds KRIS, and drops you into the KRIS
REPL. It's safe to re-run — steps that already finished (packages, the
llama.cpp build, the model download, an already-running server) are
skipped. The server keeps running in the background afterwards, so next
time you just need `cd ~/kris && ./target/release/kris-cli`.

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
   server keeps running in the first), `cd` into this project, and build
   and run KRIS there:

   ```
   cd ~/kris
   cargo build --release
   ./target/release/kris-cli
   ```

6. Point KRIS at the server/model if you changed the defaults:

   ```
   kris > config set llama_url http://127.0.0.1:8080
   kris > config set model qwen2.5-coder-3b-instruct
   ```

   Settings persist to `~/.kris/config.toml`.

## Usage

By default KRIS detects the project root by walking up from the current
working directory looking for `Cargo.toml`, `package.json`, or `artisan`.
To point it at a specific folder instead, pass it as an argument:

```
./target/release/kris-cli /path/to/your/project
```

```
kris > workspace          # show detected project (Rust/Node/Laravel/...)
kris > ask fix the bug in src/main.rs
kris > reset              # clear the conversation history
```

`ask` runs an agent loop: the model can call `read_file`, `list_directory`,
`find_files`, `tree`, and `write_file` (scoped to the detected project root)
before giving its final answer.
