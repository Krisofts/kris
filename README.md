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

1. Install build tools and Rust:

   ```
   pkg update && pkg upgrade
   pkg install git cmake clang make rust
   ```

2. Build llama.cpp (provides `llama-server`):

   ```
   git clone https://github.com/ggml-org/llama.cpp
   cd llama.cpp
   cmake -B build -DGGML_LLAMAFILE=OFF
   cmake --build build --config Release -j $(nproc)
   cd ..
   ```

3. Get shared storage access and download a small GGUF coding model, e.g.
   Qwen2.5-Coder 1.5B or 3B Instruct (search
   "Qwen2.5-Coder-3B-Instruct-GGUF" on Hugging Face, grab a `Q4_K_M`
   quantization to keep RAM usage down on-device):

   ```
   termux-setup-storage
   curl -L -o qwen2.5-coder-3b-instruct-q4_k_m.gguf \
       "<huggingface-resolve-url-of-the-Q4_K_M-file>"
   ```

4. Start the local inference server (keep this session running):

   ```
   ./llama.cpp/build/bin/llama-server \
       -m qwen2.5-coder-3b-instruct-q4_k_m.gguf \
       --host 127.0.0.1 --port 8080 -c 4096
   ```

5. Open a new Termux session (swipe from the left edge to add one, so the
   server keeps running in the first), then build and run KRIS there:

   ```
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

```
kris > workspace          # show detected project (Rust/Node/Laravel/...)
kris > ask fix the bug in src/main.rs
kris > reset              # clear the conversation history
```

`ask` runs an agent loop: the model can call `read_file`, `list_directory`,
`find_files`, `tree`, and `write_file` (scoped to the detected project root)
before giving its final answer.
