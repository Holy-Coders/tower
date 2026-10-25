#!/usr/bin/env bash
# Tower — install + agent setup, in one non-interactive run.
#
# Written to be run by a human or handed to an AI coding agent as-is:
#
#     bash scripts/install.sh
#
# It is idempotent (safe to re-run after every update) and does:
#   1. check prerequisites (git; installs Rust via rustup if missing)
#   2. build and install the `tower` binary into ~/.cargo/bin
#   3. make sure ~/.cargo/bin is on PATH in ~/.zshrc and ~/.bashrc
#   4. smoke-test the install end to end (scratch repo + MCP tools/list)
#   5. register Tower as an MCP server with Codex (~/.codex/config.toml),
#      if that file exists, and print the MCP config for other clients
#   6. print a short summary of how an agent drives Tower from here
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$HOME/.cargo/bin/tower"
step() { printf '\n== %s ==\n' "$*"; }
ok() { printf 'ok: %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

step "1/6 prerequisites"
command -v git >/dev/null 2>&1 || die "git not found. On macOS run: xcode-select --install, then re-run this script."
ok "git $(git --version | awk '{print $3}')"

if ! command -v cargo >/dev/null 2>&1; then
  step "Rust not found — installing via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
fi
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null 2>&1 || die "cargo still not on PATH after rustup install."
ok "cargo $(cargo --version | awk '{print $2}')"

step "2/6 build + install tower from $ROOT"
cargo install --path "$ROOT" --force
[ -x "$BIN" ] || die "$BIN missing after cargo install."
ok "installed $BIN"

step "3/6 PATH for future shells"
for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
  touch "$rc"
  if grep -qs 'cargo/bin' "$rc"; then
    ok "$rc already includes ~/.cargo/bin"
  else
    printf '\n# Added by Tower scripts/install.sh\nexport PATH="$HOME/.cargo/bin:$PATH"\n' >> "$rc"
    ok "added ~/.cargo/bin to $rc"
  fi
done

step "4/6 smoke test (scratch repo + MCP)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tower-install-XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT
export TOWER_HOME="$SCRATCH/home"
git -C "$SCRATCH" init -q -b main repo
git -C "$SCRATCH/repo" config user.email t@t; git -C "$SCRATCH/repo" config user.name T
echo x > "$SCRATCH/repo/x.txt"; git -C "$SCRATCH/repo" add -A; git -C "$SCRATCH/repo" commit -qm base
"$BIN" --repo "$SCRATCH/repo" init --write-config >/dev/null
"$BIN" --repo "$SCRATCH/repo" board >/dev/null
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | "$BIN" --repo "$SCRATCH/repo" serve-mcp > "$SCRATCH/tools.jsonl" 2>/dev/null
N_TOOLS="$(grep -o '"name":' "$SCRATCH/tools.jsonl" | wc -l | tr -d ' ')"
[ "$N_TOOLS" = "20" ] || die "MCP smoke test: expected 20 tools, got $N_TOOLS."
ok "tower init + board work; MCP serve-mcp advertises 20 tools"

step "5/6 MCP registration"
CODEX_CFG="$HOME/.codex/config.toml"
if [ -f "$CODEX_CFG" ]; then
  if grep -qs 'mcp_servers.tower' "$CODEX_CFG"; then
    ok "Codex config already has [mcp_servers.tower]"
  else
    printf '\n# Tower MCP (added by scripts/install.sh)\n[mcp_servers.tower]\ncommand = "%s"\nargs = ["serve-mcp"]\n' "$BIN" >> "$CODEX_CFG"
    ok "registered Tower in $CODEX_CFG"
  fi
else
  echo "no ~/.codex/config.toml found — skipping Codex registration."
fi
echo "For any other MCP client, register this server (stdio, JSON-RPC):"
printf '  { "mcpServers": { "tower": { "command": "%s", "args": ["serve-mcp"] } } }\n' "$BIN"

step "6/6 done — how an agent drives Tower"
cat <<EOF
Tower is installed at $BIN and verified working.

Per project repo (once):
  tower --repo <repo> init --write-config   # register the repo, write tower.toml
  tower --repo <repo> board                 # the live board (tasks, claims, changesets)

Day to day, agents coordinate through the CLI or the MCP tools
(board_get, changeset_status, changeset_conflicts, gate_run, ...).
Every MCP tool call takes a "repo" argument with the repo path.
Re-run this script after pulling updates: bash scripts/install.sh
EOF
