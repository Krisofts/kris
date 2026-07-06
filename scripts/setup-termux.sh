#!/usr/bin/env bash
# One-shot setup for running KRIS fully offline on Termux:
# installs packages, builds llama.cpp's llama-server, downloads a Qwen2.5-Coder
# GGUF model, starts the server in the background, builds KRIS, then launches it.
#
# Usage: bash scripts/setup-termux.sh [1.5b|3b]
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
#   CTX_SIZE=2048   smaller context = less memory + faster prompt processing
set -euo pipefail

if ! command -v pkg >/dev/null 2>&1; then
  echo "This script only works inside Termux (the 'pkg' command was not found)." >&2
  exit 1
fi

JOBS="${JOBS:-2}"
MODEL_SIZE="${1:-${MODEL_SIZE:-3b}}"
LLAMA_HOST="127.0.0.1"
LLAMA_PORT="${LLAMA_PORT:-8080}"
CTX_SIZE="${CTX_SIZE:-4096}"
THREADS="${THREADS:-}"
MLOCK="${MLOCK:-}"

LLAMA_DIR="$HOME/llama.cpp"
KRIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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
  *)
    echo "Unknown model size '$MODEL_SIZE' (use 1.5b or 3b)" >&2
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
  fi
  (
    cd "$LLAMA_DIR"
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
  server_args=(-m "$MODEL_PATH" --host "$LLAMA_HOST" --port "$LLAMA_PORT" -c "$CTX_SIZE")
  [ -n "$THREADS" ] && server_args+=(-t "$THREADS")
  [ -n "$MLOCK" ] && server_args+=(--mlock)

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

if [ ! -f "$HOME/.kris/config.toml" ]; then
  log "Writing default KRIS config"
  mkdir -p "$HOME/.kris"
  cat > "$HOME/.kris/config.toml" <<EOF2
llama_url = "http://$LLAMA_HOST:$LLAMA_PORT"
model = "${MODEL_FILE%.gguf}"
temperature = 0.2
max_tokens = 1024
max_tool_iterations = 6
EOF2
fi

mkdir -p "$HOME/project"

log "Installing the 'kris' command"
mkdir -p "$PREFIX/bin"
ln -sf "$KRIS_DIR/target/release/kris" "$PREFIX/bin/kris"

log "All set. Launching KRIS (workspace: $HOME/project)..."
echo "From now on, just type 'kris' from anywhere to start it again."
exec "$KRIS_DIR/target/release/kris"
