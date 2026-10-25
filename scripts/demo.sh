#!/usr/bin/env bash
# Tower acceptance demo (briefs/BUILD-BRIEF.md acceptance item 2, adapting the
# 6-step demo from tower-build-prompt.md to the Rust binary).
#
#   1. throwaway git repo with a small TypeScript program + trivial tests
#   2. tower init + tower watch against it
#   3. three worktrees: 1 and 2 edit the same function conflictingly,
#      3 edits an unrelated file
#   4. board shows 1<->2 as a red collision naming both changesets, 3 green
#   5. task -> claim -> changeset -> rehearsal -> gates -> land, driven via
#      the MCP tools (JSON-RPC piped into `tower serve-mcp`)
#   6. PASS/FAIL per assertion + final summary; exit code reflects failures
#
# Everything happens in a mktemp scratch dir; Tower state lives in a scratch
# TOWER_HOME. Nothing outside the scratch dir is touched.
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${TOWER_BIN:-$ROOT/target/release/tower}"
if [ ! -x "$BIN" ]; then
  echo "building release binary..."
  (cd "$ROOT" && "${CARGO:-cargo}" build --release) || { echo "FAIL build"; exit 1; }
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tower-demo-XXXXXX")"
export TOWER_HOME="$SCRATCH/home"
OUT="$SCRATCH/out"; mkdir -p "$OUT"
PASS=0; FAIL=0; FAILED=""

pass() { echo "PASS  $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL  $1"; FAIL=$((FAIL + 1)); FAILED="$FAILED $1"; }
# check <name> <command...>
check() { local name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }
# grepq <name> <file> <fixed-string>
grepq() { if grep -qF "$3" "$2" 2>/dev/null; then pass "$1"; else fail "$1"; fi; }
# grepe <name> <file> <extended-regex>
grepe() { if grep -qE "$3" "$2" 2>/dev/null; then pass "$1"; else fail "$1"; fi; }

gitc() { git -C "$1" config user.email t@t; git -C "$1" config user.name T; }
TW() { "$BIN" --repo "$REPO" "$@"; }

echo "== step 0: init --write-config on a fresh repo writes tower.toml =="
FRESH="$SCRATCH/fresh"; mkdir -p "$FRESH"; git -C "$FRESH" init -q -b main
gitc "$FRESH"; echo x > "$FRESH/README"; git -C "$FRESH" add -A; git -C "$FRESH" commit -qm init
"$BIN" --repo "$FRESH" init --write-config > "$OUT/fresh-init.json" 2>&1
check "fresh init exits 0" test -f "$OUT/fresh-init.json"
check "tower.toml written with --write-config" test -f "$FRESH/tower.toml"
grepq "fresh init reports config_written" "$OUT/fresh-init.json" '"config_written": true'

echo "== step 1: throwaway TypeScript repo =="
REPO="$SCRATCH/repo"; mkdir -p "$REPO/src"; git -C "$REPO" init -q -b main; gitc "$REPO"
cat > "$REPO/src/math.ts" <<'EOF'
export function add(a: number, b: number): number {
  return a + b;
}
export function mul(a: number, b: number): number {
  return a * b;
}
EOF
cat > "$REPO/src/greet.ts" <<'EOF'
export function greet(name: string): string {
  return `hello ${name}`;
}
EOF
cat > "$REPO/src/other.ts" <<'EOF'
export function other(): number {
  return 1;
}
EOF
cat > "$REPO/test.sh" <<'EOF'
#!/bin/sh
grep -q 'hello' src/greet.ts && grep -q 'a + b' src/math.ts && grep -q 'other' src/other.ts
EOF
chmod +x "$REPO/test.sh"
cat > "$REPO/tower.toml" <<'EOF'
[repo]
main_branch = "main"

[gates]
check = "sh test.sh"
test = "sh test.sh"

[reserved]
paths = ["**/*.lock"]
patterns = []

[escalation]
extra_triggers = []

[limits]
max_concurrent_assignments = 8
EOF
git -C "$REPO" add -A; git -C "$REPO" commit -qm base
check "demo repo base commit" git -C "$REPO" rev-parse main

echo "== step 2: tower init + watch =="
TW init --write-config > "$OUT/init.json" 2>&1
grepq "init registered repo" "$OUT/init.json" '"main_branch": "main"'
grepq "custom gates preserved (not overwritten)" "$REPO/tower.toml" 'check = "sh test.sh"'
TW watch --iterations 2 --interval 1 > "$OUT/watch.log" 2>&1
check "watch (bounded polling) exits 0" test -s "$OUT/watch.log"
grepq "watch scanned worktrees" "$OUT/watch.log" "scan: "
TW board --html "$OUT/board.html" > "$OUT/board-empty.txt" 2>&1
grepq "static HTML board generated" "$OUT/board.html" "Tower Board"

echo "== step 3: three worktrees, two conflicting on add(), one unrelated =="
for w in w1 w2 w3; do git -C "$REPO" worktree add -q -b "$w" "$SCRATCH/$w" main; gitc "$SCRATCH/$w"; done
sed -i 's/return a + b;/return a + b + 0;/' "$SCRATCH/w1/src/math.ts"
sed -i 's/return a + b;/const s = a + b;\n  return s;/' "$SCRATCH/w2/src/math.ts"
sed -i 's/return 1;/return 2;/' "$SCRATCH/w3/src/other.ts"
for w in w1 w2 w3; do git -C "$SCRATCH/$w" add -A; git -C "$SCRATCH/$w" commit -qm "$w work"; done
check "three worktrees created" test -d "$SCRATCH/w3/src"

echo "== step 4: board shows red collision (both named) and green unrelated =="
TW board > "$OUT/board-radar.txt" 2>&1
grepe "radar: w1<->w2 red collision names both worktrees" "$OUT/board-radar.txt" "RED .*w1 <-> .*w2|RED .*w2 <-> .*w1"
grepe "radar: w3 green" "$OUT/board-radar.txt" "\[GREEN\].*/w3"

TW task create "add variant A" --brief b > "$OUT/t1.json" 2>&1
TW task create "add variant B" --brief b > "$OUT/t2.json" 2>&1
TW task create "other change" --brief b > "$OUT/t3.json" 2>&1
grepq "task IDs are T-1.." "$OUT/t1.json" '"task_id": "T-1"'
TW assignment create --task T-1 --worktree "$SCRATCH/w1" > "$OUT/a1.json" 2>&1
TW assignment create --task T-2 --worktree "$SCRATCH/w2" > "$OUT/a2.json" 2>&1
TW assignment create --task T-3 --worktree "$SCRATCH/w3" > "$OUT/a3.json" 2>&1
grepq "assignment IDs are A-1.." "$OUT/a1.json" '"assignment_id": "A-1"'
TW claim acquire --assignment A-3 --node 'src/other.ts#other' > "$OUT/claim3.json" 2>&1
grepq "claim for A-3 granted" "$OUT/claim3.json" '"granted": true'
TW changeset register --assignment A-1 --worktree "$SCRATCH/w1" > "$OUT/c1.json" 2>&1
TW changeset register --assignment A-2 --worktree "$SCRATCH/w2" > "$OUT/c2.json" 2>&1
TW changeset register --assignment A-3 --worktree "$SCRATCH/w3" > "$OUT/c3.json" 2>&1
grepq "C-1 registered" "$OUT/c1.json" '"changeset_id": "C-1"'
grepq "C-2 registered red (C-1 exists by now)" "$OUT/c2.json" '"status": "red"'
grepq "C-2 reason names C-1" "$OUT/c2.json" 'conflicts with C-1'
TW changeset status C-1 > "$OUT/c1s.json" 2>&1
# Phase 2.1 intentional change (F1, order-aware): C-1 is first in integration
# order, so it is no longer red merely because later C-2 conflicts with it.
grepq "C-1 not red once C-2 registered (order-aware)" "$OUT/c1s.json" '"status": "yellow"'
grepq "C-1 info reason says it blocks C-2" "$OUT/c1s.json" 'blocks C-2'
grepq "C-3 yellow until gates run" "$OUT/c3.json" '"status": "yellow"'
TW board > "$OUT/board-cs.txt" 2>&1
grepe "board: RED C-1 <-> C-2 naming both changesets" "$OUT/board-cs.txt" "RED C-1 <-> C-2|RED C-2 <-> C-1"
TW gate run C-3 --tier 1 > "$OUT/g31.json" 2>&1
TW gate run C-3 --tier 2 > "$OUT/g32.json" 2>&1
grepq "C-3 tier-1 gate passed" "$OUT/g31.json" '"passed": true'
grepq "C-3 tier-2 gate passed" "$OUT/g32.json" '"passed": true'
TW changeset status C-3 > "$OUT/c3s.json" 2>&1
grepq "C-3 green after gates" "$OUT/c3s.json" '"status": "green"'
# Phase 2.1 intentional change (F1+F5): the red changeset is now C-2 (C-1 is
# landable first in order), so the refused-landing check targets C-2.
TW land C-2 > "$OUT/land2.log" 2>&1; rc=$?
if [ "$rc" -ne 0 ]; then pass "land of red C-2 refused"; else fail "land of red C-2 refused"; fi
grepq "refusal explains non-green" "$OUT/land2.log" "non-green"

echo "== step 5: MCP task -> claim -> changeset -> rehearsal -> gates -> land =="
cat > "$OUT/mcp1.jsonl" <<EOF
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"task_create","arguments":{"repo":"$REPO","title":"greet trims name","brief":"trim"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"task_plan_save","arguments":{"repo":"$REPO","task_id":"T-4","plan":"one assignment, claim src/greet.ts#greet"}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"worktree_create","arguments":{"repo":"$REPO","task_id":"T-4"}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"claim_acquire","arguments":{"repo":"$REPO","assignment_id":"A-4","nodes":[{"file":"src/greet.ts","item":"greet"}]}}}
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"claim_acquire","arguments":{"repo":"$REPO","assignment_id":"A-3","nodes":[{"file":"src/greet.ts"}]}}}
EOF
"$BIN" --repo "$REPO" serve-mcp < "$OUT/mcp1.jsonl" > "$OUT/mcp1-out.jsonl" 2>&1
grepq "MCP initialize handshake" "$OUT/mcp1-out.jsonl" '"protocolVersion"'
TOOLS_LINE="$(grep '"id":2' "$OUT/mcp1-out.jsonl" | head -1)"
N_TOOLS="$(printf '%s' "$TOOLS_LINE" | grep -o '"name":' | wc -l | tr -d ' ')"
# Phase 2.1 intentional change: 16 spec tools + changeset_refresh/supersede;
# Phase 2.2 adds changeset_conflicts (19 total).
if [ "$N_TOOLS" = "19" ]; then pass "MCP tools/list advertises 19 tools (16 + refresh/supersede/conflicts)"; else fail "MCP tools/list advertises 19 tools (got $N_TOOLS)"; fi
grepq "MCP task_create -> T-4" "$OUT/mcp1-out.jsonl" '"task_id":"T-4"'
grepq "MCP worktree_create -> A-4" "$OUT/mcp1-out.jsonl" '"assignment_id":"A-4"'
grepq "MCP claim granted" "$OUT/mcp1-out.jsonl" '"granted":true'
grepq "MCP conflicting claim refused" "$OUT/mcp1-out.jsonl" '"granted":false'
grepq "MCP refusal names colliding A-4" "$OUT/mcp1-out.jsonl" '"assignment_id":"A-4","file":"src/greet.ts"'

WT="$(find "$TOWER_HOME/worktrees" -maxdepth 2 -name A-4 -type d | head -1)"
case "$WT" in "$TOWER_HOME"*) pass "managed worktree lives under ~/.tower/worktrees";; *) fail "managed worktree lives under ~/.tower/worktrees (got $WT)";; esac
# The worker does its job inside its claim.
gitc "$WT"
sed -i 's/hello ${name}/hello ${name.trim()}/' "$WT/src/greet.ts"
git -C "$WT" add -A; git -C "$WT" commit -qm "trim greet name"

# Rehearsal across all open changesets on the scratch integration tree.
TW merge-check > "$OUT/merge-check.json" 2>&1
grepq "rehearsal ran configured gates" "$OUT/merge-check.json" '"passed": true'

cat > "$OUT/mcp2.jsonl" <<EOF
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"changeset_register","arguments":{"repo":"$REPO","assignment_id":"A-4","worktree":"$WT"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gate_run","arguments":{"repo":"$REPO","changeset_id":"C-4","tier":1}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gate_run","arguments":{"repo":"$REPO","changeset_id":"C-4","tier":2}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"changeset_status","arguments":{"repo":"$REPO","changeset_id":"C-4"}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"escalate_create","arguments":{"repo":"$REPO","task_id":"T-4","question":"land C-4 while C-1/C-2 unresolved?","options":["yes, disjoint file","wait"],"recommendation":"yes, disjoint file"}}}
EOF
"$BIN" --repo "$REPO" serve-mcp < "$OUT/mcp2.jsonl" > "$OUT/mcp2-out.jsonl" 2>&1
grepq "MCP changeset_register -> C-4" "$OUT/mcp2-out.jsonl" '"changeset_id":"C-4"'
grepq "claimed-vs-actual: nothing outside claim" "$OUT/mcp2-out.jsonl" '"outside_files":[]'
grepq "MCP tier gates passed" "$OUT/mcp2-out.jsonl" '"passed":true'
grepq "C-4 green after gates" "$OUT/mcp2-out.jsonl" '"status":"green"'
grepq "MCP escalate_create -> E-1" "$OUT/mcp2-out.jsonl" '"escalation_id":"E-1"'

TW escalate resolve E-1 "yes, disjoint file" > "$OUT/e1.json" 2>&1
grepq "escalation resolved by ID" "$OUT/e1.json" '"resolved": true'
TW decisions log "disjoint-file changesets may land while reds are unresolved" > "$OUT/d1.json" 2>&1
grepq "decision logged as D-1" "$OUT/d1.json" '"decision_id": "D-1"'
TW lease acquire --holder demo-orchestrator > "$OUT/lease.json" 2>&1
grepq "orchestrator lease acquired" "$OUT/lease.json" '"acquired": true'
TW lease acquire --holder second-host > "$OUT/lease2.log" 2>&1; rc=$?
if [ "$rc" -ne 0 ]; then pass "second orchestrator refused the lease"; else fail "second orchestrator refused the lease"; fi

TW land C-4 > "$OUT/land4.json" 2>&1
grepq "C-4 landed" "$OUT/land4.json" '"landed": true'
git -C "$REPO" show main:src/greet.ts > "$OUT/main-greet.ts" 2>&1
grepq "main now contains the landed change" "$OUT/main-greet.ts" 'name.trim()'
git -C "$SCRATCH/w1" show HEAD:src/math.ts > "$OUT/w1-math.ts" 2>&1
grepq "user worktree w1 untouched by landing" "$OUT/w1-math.ts" 'return a + b + 0;'
TW events --since 0 > "$OUT/events.json" 2>&1
grepq "event log records the landing" "$OUT/events.json" 'changeset_landed'
grepq "event log records the claim conflict" "$OUT/events.json" 'claim_conflict'
TW lease release --holder demo-orchestrator > /dev/null 2>&1
"$BIN" --repo "$REPO" lsp > "$OUT/lsp.txt" 2>&1
grepq "out-of-scope stubs are honest" "$OUT/lsp.txt" "not implemented in v1"

echo "== step 6: summary =="
echo "board snapshot (text):"
TW board 2>/dev/null | sed -n '1,40p'
echo
echo "DEMO RESULT: $PASS passed, $FAIL failed"
if [ "$FAIL" -eq 0 ]; then
  echo "DEMO PASS (scratch removed; set KEEP=1 to keep it)"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$SCRATCH"
  exit 0
else
  echo "DEMO FAIL:$FAILED"
  echo "scratch kept for inspection: $SCRATCH"
  exit 1
fi
