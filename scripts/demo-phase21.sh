#!/usr/bin/env bash
# Tower Phase 2.1 demo: sequential Tower-owned candidate landing.
#
# Three changesets all fork from the same base commit:
#   C-1 (w1) edits src/greet.ts  - compatible
#   C-2 (w2) edits src/other.ts  - compatible, but no longer fast-forwards
#                                  main once C-1 has landed
#   C-3 (w3) edits src/greet.ts on the same line as C-1 - conflicting
# C-1 and C-2 land sequentially via Tower-built landing candidates (no
# manual merges in any worker worktree); C-3 is refused with named files
# while main stays untouched. PASS/FAIL per assertion + summary.
#
# Everything happens in a mktemp scratch dir with a scratch TOWER_HOME.
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${TOWER_BIN:-$ROOT/target/release/tower}"
if [ ! -x "$BIN" ]; then
  echo "building release binary..."
  (cd "$ROOT" && "${CARGO:-cargo}" build --release) || { echo "FAIL build"; exit 1; }
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tower-demo21-XXXXXX")"
export TOWER_HOME="$SCRATCH/home"
OUT="$SCRATCH/out"; mkdir -p "$OUT"
PASS=0; FAIL=0; FAILED=""

pass() { echo "PASS  $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL  $1"; FAIL=$((FAIL + 1)); FAILED="$FAILED $1"; }
check() { local name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }
grepq() { if grep -qF "$3" "$2" 2>/dev/null; then pass "$1"; else fail "$1"; fi; }
eq() { if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (got '$2', want '$3')"; fi; }

gitc() { git -C "$1" config user.email t@t; git -C "$1" config user.name T; }
TW() { "$BIN" --repo "$REPO" "$@"; }

echo "== step 1: throwaway repo, same base for all three worktrees =="
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
EOF
git -C "$REPO" add -A; git -C "$REPO" commit -qm base
check "demo repo base commit" git -C "$REPO" rev-parse main
TW init --write-config > "$OUT/init.json" 2>&1
grepq "init registered repo" "$OUT/init.json" '"main_branch": "main"'
BASE="$(git -C "$REPO" rev-parse main)"

for w in w1 w2 w3; do git -C "$REPO" worktree add -q -b "$w" "$SCRATCH/$w" main; gitc "$SCRATCH/$w"; done
sed -i 's/hello ${name}/hello ${name.trim()}/' "$SCRATCH/w1/src/greet.ts"
sed -i 's/return 1;/return 2;/' "$SCRATCH/w2/src/other.ts"
sed -i 's/hello ${name}/hello ${name.toUpperCase()}/' "$SCRATCH/w3/src/greet.ts"
for w in w1 w2 w3; do git -C "$SCRATCH/$w" add -A; git -C "$SCRATCH/$w" commit -qm "$w work"; done
check "three worktrees forked from same base" test "$(git -C "$SCRATCH/w1" merge-base HEAD "$BASE")" = "$BASE"

echo "== step 2: tasks, claims (incl. a partial grant), changesets =="
TW task create "greet trims name" --brief b > "$OUT/t1.json" 2>&1
TW task create "other returns 2" --brief b > "$OUT/t2.json" 2>&1
TW task create "greet shouts name" --brief b > "$OUT/t3.json" 2>&1
TW assignment create --task T-1 --worktree "$SCRATCH/w1" > "$OUT/a1.json" 2>&1
TW assignment create --task T-2 --worktree "$SCRATCH/w2" > "$OUT/a2.json" 2>&1
TW assignment create --task T-3 --worktree "$SCRATCH/w3" > "$OUT/a3.json" 2>&1
TW claim acquire --assignment A-1 --node 'src/greet.ts#greet' > "$OUT/claim1.json" 2>&1
grepq "claim for A-1 fully granted" "$OUT/claim1.json" '"granted": true'
# Partial grant: A-3 asks for greet (held by A-1) plus math (free). The free
# node is granted, the held one refused by name, and the exit code is 0.
TW claim acquire --assignment A-3 --node 'src/greet.ts#greet' --node 'src/math.ts#add' > "$OUT/claim3.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "partial claim grant exits 0"; else fail "partial claim grant exits 0 (rc=$rc)"; fi
grepq "partial grant reports granted:false overall" "$OUT/claim3.json" '"granted": false'
grepq "partial grant names the granted free node" "$OUT/claim3.json" '"file": "src/math.ts"'
grepq "partial grant names the holder of the refused node" "$OUT/claim3.json" '"assignment_id": "A-1"'
# Fully refused bundle exits 1 and grants nothing.
TW claim acquire --assignment A-3 --node 'src/greet.ts#greet' > "$OUT/claim3b.json" 2>&1; rc=$?
if [ "$rc" -eq 1 ]; then pass "fully refused claim exits 1"; else fail "fully refused claim exits 1 (rc=$rc)"; fi
# Per-node release frees only the named node.
TW claim release --assignment A-3 --node 'src/math.ts#add' > "$OUT/rel3.json" 2>&1
TW claim list > "$OUT/claims.json" 2>&1
if grep -q 'src/math.ts' "$OUT/claims.json"; then fail "per-node release freed src/math.ts#add"; else pass "per-node release freed src/math.ts#add"; fi
grepq "per-node release kept A-1 greet claim" "$OUT/claims.json" '"file_path": "src/greet.ts"'
TW claim acquire --assignment A-2 --node 'src/other.ts#other' > "$OUT/claim2.json" 2>&1

TW changeset register --assignment A-1 --worktree "$SCRATCH/w1" > "$OUT/c1.json" 2>&1
TW changeset register --assignment A-2 --worktree "$SCRATCH/w2" > "$OUT/c2.json" 2>&1
TW changeset register --assignment A-3 --worktree "$SCRATCH/w3" > "$OUT/c3.json" 2>&1
grepq "C-1 registered" "$OUT/c1.json" '"changeset_id": "C-1"'
grepq "C-2 registered" "$OUT/c2.json" '"changeset_id": "C-2"'
grepq "C-3 registered red (blocked by earlier C-1)" "$OUT/c3.json" '"status": "red"'
grepq "C-3 reason names C-1 and the file" "$OUT/c3.json" 'conflicts with C-1 in src/greet.ts'
# Order-aware: first-in-order C-1 is landable-yellow (gates pending), not red.
TW changeset status C-1 > "$OUT/c1s.json" 2>&1
grepq "C-1 is yellow, not red (order-aware)" "$OUT/c1s.json" '"status": "yellow"'
grepq "C-1 carries informational blocks C-3" "$OUT/c1s.json" 'blocks C-3 in src/greet.ts'
grepq "C-1 next action is run gates" "$OUT/c1s.json" '"next": "run gates"'

echo "== step 3: land C-1 via a Tower-built candidate (no worker merge) =="
W1_HEAD="$(git -C "$SCRATCH/w1" rev-parse HEAD)"
TW land C-1 > "$OUT/land1.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "land C-1 exits 0"; else fail "land C-1 exits 0 (rc=$rc)"; fi
grepq "C-1 landed" "$OUT/land1.json" '"landed": true'
grepq "C-1 landed via candidate" "$OUT/land1.json" '"method": "candidate"'
eq "C-1 source head recorded" "$(python3 -c "import json;print(json.load(open('$OUT/land1.json'))['source_head'])")" "$W1_HEAD"
eq "C-1 based-on main head recorded" "$(python3 -c "import json;print(json.load(open('$OUT/land1.json'))['based_on_main_head'])")" "$BASE"
eq "w1 HEAD untouched by landing" "$(git -C "$SCRATCH/w1" rev-parse HEAD)" "$W1_HEAD"
check "w1 worktree clean after landing" test -z "$(git -C "$SCRATCH/w1" status --porcelain)"
git -C "$REPO" show main:src/greet.ts > "$OUT/main-greet1.ts" 2>&1
grepq "main contains the C-1 change" "$OUT/main-greet1.ts" 'name.trim()'
MAIN1="$(git -C "$REPO" rev-parse main)"

echo "== step 4: land C-2 via candidate (compatible, not a fast-forward) =="
W2_HEAD="$(git -C "$SCRATCH/w2" rev-parse HEAD)"
MB="$(git -C "$REPO" merge-base main "$W2_HEAD")"
if [ "$MB" != "$MAIN1" ]; then pass "C-2 does not fast-forward main after C-1"; else fail "C-2 does not fast-forward main after C-1"; fi
TW gate run C-2 --tier 1 > "$OUT/g21.json" 2>&1
TW gate run C-2 --tier 2 > "$OUT/g22.json" 2>&1
TW changeset status C-2 > "$OUT/c2s.json" 2>&1
grepq "C-2 green after gates on rehearsal tree" "$OUT/c2s.json" '"status": "green"'
grepq "C-2 next action is land" "$OUT/c2s.json" '"next": "land"'
W2_GREET_BEFORE="$(cat "$SCRATCH/w2/src/greet.ts")"
TW land C-2 > "$OUT/land2.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "land C-2 exits 0"; else fail "land C-2 exits 0 (rc=$rc)"; fi
grepq "C-2 landed via candidate" "$OUT/land2.json" '"method": "candidate"'
CAND2="$(python3 -c "import json;print(json.load(open('$OUT/land2.json'))['candidate_head'])")"
if [ "$CAND2" != "$W2_HEAD" ]; then pass "candidate head is a Tower merge commit, not the worker head"; else fail "candidate head is a Tower merge commit, not the worker head"; fi
eq "main advanced exactly to the candidate head" "$(git -C "$REPO" rev-parse main)" "$CAND2"
eq "C-2 based-on is the post-C-1 main head" "$(python3 -c "import json;print(json.load(open('$OUT/land2.json'))['based_on_main_head'])")" "$MAIN1"
eq "w2 HEAD untouched by candidate landing" "$(git -C "$SCRATCH/w2" rev-parse HEAD)" "$W2_HEAD"
eq "w2 files untouched (no merge in worker worktree)" "$(cat "$SCRATCH/w2/src/greet.ts")" "$W2_GREET_BEFORE"
git -C "$REPO" show main:src/other.ts > "$OUT/main-other.ts" 2>&1
grepq "main contains the C-2 change" "$OUT/main-other.ts" 'return 2;'
git -C "$REPO" show main:src/greet.ts > "$OUT/main-greet2.ts" 2>&1
grepq "main still contains the C-1 change after C-2" "$OUT/main-greet2.ts" 'name.trim()'
MAIN2="$(git -C "$REPO" rev-parse main)"

echo "== step 5: conflicting C-3 is refused with named files =="
TW changeset status C-3 > "$OUT/c3s.json" 2>&1
grepq "C-3 is red" "$OUT/c3s.json" '"status": "red"'
grepq "C-3 next action names the conflicting file" "$OUT/c3s.json" 'resolve conflict with main in src/greet.ts'
W3_HEAD="$(git -C "$SCRATCH/w3" rev-parse HEAD)"
TW land C-3 > "$OUT/land3.log" 2>&1; rc=$?
if [ "$rc" -ne 0 ]; then pass "land of conflicting C-3 refused"; else fail "land of conflicting C-3 refused"; fi
grepq "refusal names the conflicted file" "$OUT/land3.log" 'src/greet.ts'
eq "main unchanged by refused landing" "$(git -C "$REPO" rev-parse main)" "$MAIN2"
eq "w3 HEAD untouched by refused landing" "$(git -C "$SCRATCH/w3" rev-parse HEAD)" "$W3_HEAD"
check "final main tree passes the gates" sh -c "cd '$REPO' && sh test.sh"
TW events --since 0 > "$OUT/events.json" 2>&1
grepq "event log records candidate heads" "$OUT/events.json" 'candidate_head'
TW board > "$OUT/board.txt" 2>&1
grepq "board shows landed next-action for C-1" "$OUT/board.txt" 'next: landed'

echo "== step 6: summary =="
echo "DEMO-PHASE21 RESULT: $PASS passed, $FAIL failed"
if [ "$FAIL" -eq 0 ]; then
  echo "DEMO-PHASE21 PASS (scratch removed; set KEEP=1 to keep it)"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$SCRATCH"
  exit 0
else
  echo "DEMO-PHASE21 FAIL:$FAILED"
  echo "scratch kept for inspection: $SCRATCH"
  exit 1
fi
