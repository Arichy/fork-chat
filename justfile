backend_dir := "fork-chat-backend"
backend_justfile := "fork-chat-backend/justfile"
backend_bin := "fork-chat-backend/target/release/fork-chat-backend"
backend_config := "fork-chat-backend/config.json"
frontend_dir := "fork-chat-frontend"
frontend_dist := "fork-chat-frontend/dist"

default:
  @just --list

# Build frontend bundle and backend release binary once.
build:
  pnpm --dir {{frontend_dir}} build
  cargo build --release --manifest-path {{backend_dir}}/Cargo.toml

# Start local Postgres.
db-up:
  just --justfile {{backend_justfile}} --working-directory {{backend_dir}} db-up

# Stop local Postgres.
db-down:
  just --justfile {{backend_justfile}} --working-directory {{backend_dir}} db-down

# Delete the local Postgres volume.
db-nuke:
  just --justfile {{backend_justfile}} --working-directory {{backend_dir}} db-nuke

# Drop, recreate, and migrate the local database.
reset-db:
  just --justfile {{backend_justfile}} --working-directory {{backend_dir}} reset-db

run:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ ! -f "{{backend_config}}" ]]; then
    echo "Missing {{backend_config}}" >&2
    echo "Copy fork-chat-backend/config.example.json to fork-chat-backend/config.json first." >&2
    exit 1
  fi

  # `run` intentionally does not rebuild. That keeps restart latency low and
  # makes the "did I build since my last code change?" step explicit.
  if [[ ! -x "{{backend_bin}}" ]]; then
    echo "Missing compiled backend binary: {{backend_bin}}" >&2
    echo "Run 'just build' first." >&2
    exit 1
  fi

  if [[ ! -f "{{frontend_dist}}/index.html" ]]; then
    echo "Missing built frontend dist at {{frontend_dist}}/index.html" >&2
    echo "Run 'just build' first." >&2
    exit 1
  fi

  just db-up

  cd {{backend_dir}}

  exec env FORK_CHAT_FRONTEND_DIST_DIR="../{{frontend_dist}}" \
    "../{{backend_bin}}"

up: build
  @just run

dev-backend:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ ! -f "{{backend_config}}" ]]; then
    echo "Missing {{backend_config}}" >&2
    echo "Copy fork-chat-backend/config.example.json to fork-chat-backend/config.json first." >&2
    exit 1
  fi

  just db-up

  cd {{backend_dir}}

  exec cargo run

dev-frontend:
  #!/usr/bin/env bash
  set -euo pipefail

  exec pnpm --dir {{frontend_dir}} dev

dev:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ ! -f "{{backend_config}}" ]]; then
    echo "Missing {{backend_config}}" >&2
    echo "Copy fork-chat-backend/config.example.json to fork-chat-backend/config.json first." >&2
    exit 1
  fi

  just db-up

  backend_url="$(node -e 'const fs = require("fs"); const fallback = "0.0.0.0:3000"; let addr = ""; try { addr = JSON.parse(fs.readFileSync(process.argv[1], "utf8")).server_addr || ""; } catch {} addr = String(addr || fallback).trim(); const match = addr.match(/:(\d+)$/); const port = match ? match[1] : "3000"; const host = addr.slice(0, match ? -(port.length + 1) : 0) || "127.0.0.1"; const browserHost = host === "0.0.0.0" || host === "::" || host === "[::]" ? "127.0.0.1" : host; process.stdout.write(`http://${browserHost}:${port}`);' "{{backend_config}}")"

  cleanup() {
    if [[ -n "${backend_pid:-}" ]] && kill -0 "${backend_pid}" 2>/dev/null; then
      pkill -TERM -P "${backend_pid}" 2>/dev/null || true
      kill "${backend_pid}" 2>/dev/null || true
      wait "${backend_pid}" 2>/dev/null || true
    fi
  }

  trap cleanup EXIT INT TERM

  (cd {{backend_dir}} && exec cargo run) &
  backend_pid=$!

  echo "Backend:  ${backend_url}"
  echo "Frontend: http://127.0.0.1:5173"

  frontend_status=0

  pnpm --dir {{frontend_dir}} dev || frontend_status=$?

  exit "${frontend_status}"
