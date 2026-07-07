#!/usr/bin/env bash
# One-shot setup for running KRIS fully offline on Termux: installs
# packages, builds llama.cpp's llama-server (with --jinja support, which
# KRIS relies on for native Qwen tool-calling), downloads a Qwen2.5-Coder
# GGUF model, starts the server in the background, builds KRIS, and
# launches it.
#
# Usage: bash scripts/setup-termux.sh [1.5b|3b|7b]
#
# Safe to re-run: steps that are already done (packages, build, model
# download, running server) are skipped.
#
# Performance tuning (all optional env vars):
#   THREADS=4       llama-server thread count (default: let it auto-detect;
#                    on big.LITTLE phone chips, matching only the
#                    performance cores is often faster than using all of them)
#   MLOCK=1         pass --mlock to llama-server (locks the model in RAM,
#                    avoids swap jitter, needs enough free RAM to hold it)
#   CTX_SIZE=4096   smaller context = less memory + faster prompt processing
#   FLASH_ATTN=0    set to 0 to disable --flash-attn (on by default; needs a
#                    reasonably recent llama.cpp build, which this script
#                    always pulls fresh)
#   CACHE_TYPE_K=q8_0  quantize the KV cache (roughly halves its memory use
#   CACHE_TYPE_V=q8_0  for a small, usually unnoticeable quality cost)
set -euo pipefail

if ! command -v pkg >/dev/null 2>&1; then
  echo "This script only works inside Termux (the 'pkg' command was not found)." >&2
  exit 1
fi

JOBS="${JOBS:-2}"
MODEL_SIZE="${1:-${MODEL_SIZE:-3b}}"
LLAMA_HOST="127.0.0.1"
LLAMA_PORT="${LLAMA_PORT:-8080}"
CTX_SIZE="${CTX_SIZE:-8192}"
THREADS="${THREADS:-}"
MLOCK="${MLOCK:-}"
FLASH_ATTN="${FLASH_ATTN:-1}"
CACHE_TYPE_K="${CACHE_TYPE_K:-q8_0}"
CACHE_TYPE_V="${CACHE_TYPE_V:-q8_0}"

LLAMA_DIR="$HOME/llama.cpp"
KRIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="$HOME/.config/kris"

log() { printf '\n\033[1;32m==> %s\033[0m\n' "$*"; }

case "$MODEL_SIZE" in
  1.5b)
    MODEL_REPO="Qwen/Qwen2.5-Coder-1.5B-Instruct-GGUF"
    MODEL_FILE="qwen2.5-coder-1.5b-instruct-q4_k_m.gguf"
    ;;
  3b)
    MODEL_REPO="Qwen/Qwen2.5-Coder-3B-Instruct-GGUF"
    MODEL_FILE="qwen2.5-coder-3b-instruct-q4_k_m.gguf"
    ;;
  7b)
    MODEL_REPO="Qwen/Qwen2.5-Coder-7B-Instruct-GGUF"
    MODEL_FILE="qwen2.5-coder-7b-instruct-q4_k_m.gguf"
    ;;
  *)
    echo "Unknown model size '$MODEL_SIZE' (use 1.5b, 3b, or 7b)" >&2
    exit 1
    ;;
esac

MODEL_PATH="$HOME/$MODEL_FILE"

log "Installing packages"
pkg update -y
pkg upgrade -y
pkg install -y git cmake clang make rust libandroid-spawn curl

log "Building llama.cpp (llama-server)"
if [ ! -x "$LLAMA_DIR/build/bin/llama-server" ]; then
  if [ ! -d "$LLAMA_DIR" ]; then
    git clone https://github.com/ggml-org/llama.cpp "$LLAMA_DIR"
  else
    (cd "$LLAMA_DIR" && git pull --ff-only) || true
  fi
  (
    cd "$LLAMA_DIR"
    # -j low on purpose: building with too many parallel jobs is a common
    # way to get the compiler OOM-killed on a phone. If this fails partway
    # with no clear compiler error, rerun with JOBS=1.
    cmake -B build -DGGML_LLAMAFILE=OFF
    cmake --build build --config Release --target llama-server -j "$JOBS"
  )
else
  echo "llama-server already built, skipping."
fi

log "Getting a GGUF model ($MODEL_FILE)"
termux-setup-storage || true
if [ ! -s "$MODEL_PATH" ] || [ "$(stat -c%s "$MODEL_PATH" 2>/dev/null || echo 0)" -lt 500000000 ]; then
  curl -L -o "$MODEL_PATH" \
    "https://huggingface.co/$MODEL_REPO/resolve/main/$MODEL_FILE?download=true"
else
  echo "Model already downloaded, skipping."
fi

log "Starting llama-server"
if curl -sf "http://$LLAMA_HOST:$LLAMA_PORT/health" >/dev/null 2>&1; then
  echo "llama-server is already running on port $LLAMA_PORT, skipping."
else
  server_args=(-m "$MODEL_PATH" --host "$LLAMA_HOST" --port "$LLAMA_PORT" -c "$CTX_SIZE" --jinja)
  [ -n "$THREADS" ] && server_args+=(-t "$THREADS")
  [ -n "$MLOCK" ] && server_args+=(--mlock)
  [ "$FLASH_ATTN" = "1" ] && server_args+=(--flash-attn on)
  [ -n "$CACHE_TYPE_K" ] && server_args+=(--cache-type-k "$CACHE_TYPE_K")
  [ -n "$CACHE_TYPE_V" ] && server_args+=(--cache-type-v "$CACHE_TYPE_V")

  nohup "$LLAMA_DIR/build/bin/llama-server" "${server_args[@]}" \
    > "$HOME/llama-server.log" 2>&1 &
  disown

  echo "Waiting for llama-server to become ready..."
  ready=0
  for _ in $(seq 1 60); do
    if curl -sf "http://$LLAMA_HOST:$LLAMA_PORT/health" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 2
  done

  if [ "$ready" -ne 1 ]; then
    echo "llama-server didn't come up in time, check $HOME/llama-server.log" >&2
    exit 1
  fi
fi

log "Building KRIS"
(cd "$KRIS_DIR" && cargo build --release)

log "Writing KRIS config"
mkdir -p "$CONFIG_DIR"
cat > "$CONFIG_DIR/config.toml" <<EOF2
model_path = "$MODEL_PATH"
llama_server_path = "$LLAMA_DIR/build/bin/llama-server"
llama_url = "http://$LLAMA_HOST:$LLAMA_PORT"
context_size = $CTX_SIZE
temperature = 0.2
max_tokens = 4096
mlock = $( [ -n "$MLOCK" ] && echo true || echo false )
flash_attn = $( [ "$FLASH_ATTN" = "1" ] && echo true || echo false )
workspace = "$HOME/project"
EOF2

if [ -n "$THREADS" ]; then
  echo "threads = $THREADS" >> "$CONFIG_DIR/config.toml"
fi
if [ -n "$CACHE_TYPE_K" ]; then
  echo "cache_type_k = \"$CACHE_TYPE_K\"" >> "$CONFIG_DIR/config.toml"
fi
if [ -n "$CACHE_TYPE_V" ]; then
  echo "cache_type_v = \"$CACHE_TYPE_V\"" >> "$CONFIG_DIR/config.toml"
fi

mkdir -p "$HOME/project"

log "Installing the 'kris' command"
mkdir -p "$PREFIX/bin"
ln -sf "$KRIS_DIR/target/release/kris" "$PREFIX/bin/kris"

log "All set. Launching KRIS (workspace: $HOME/project)..."
echo "From now on, just type 'kris' from anywhere to start it again."
exec "$KRIS_DIR/target/release/kris"
