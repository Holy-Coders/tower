#!/usr/bin/env bash
# Tower Phase 2.2 demo: the three deep-playtest findings (N1-N3), end to end.
#
#   Fix B/C: a Rust file whose #[cfg(test)] module holds two test functions.
#            A-1 claims one test function but edits the other plus an
#            unclaimed file; the indexer names the individual test
#            function (no synthetic `tests` item), and changeset status /
#            board / MCP name the outside item and file exactly.
#   Fix A:   after the claims are fixed and C-1 lands, A-1's claims are
#            still held (named in the landing receipt and landed event);
#            only `assignment close` releases them.
#   Fix D:   a later conflicting pair makes C-3 red; `changeset conflicts`
#            (CLI text + --json, and MCP changeset_conflicts) names the
#            file and its git merge-tree hunk headers, read-only, and a
#            non-conflicted changeset reports an empty list with exit 0.
#
# Everything happens in a mktemp scratch dir with a scratch TOWER_HOME.
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${TOWER_BIN:-$ROOT/target/release/tower}"
if [ ! -x "$BIN" ]; then
  echo "building release binary..."
  (cd "$ROOT" && "${CARGO:-cargo}" build --release) || { echo "FAIL build"; exit 1; }
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tower-demo22-XXXXXX")"
export TOWER_HOME="$SCRATCH/home"
OUT="$SCRATCH/out"; mkdir -p "$OUT"
PASS=0; FAIL=0; FAILED=""

pass() { echo "PASS  $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL  $1"; FAIL=$((FAIL + 1)); FAILED="$FAILED $1"; }
check() { local name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }
grepq() { if grep -qF "$3" "$2" 2>/dev/null; then pass "$1"; else fail "$1"; fi; }
eq() { if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (got '$2', want '$3')"; fi; }
absent() { if grep -qF "$3" "$2" 2>/dev/null; then fail "$1"; else pass "$1"; fi; }

gitc() { git -C "$1" config user.email t@t; git -C "$1" config user.name T; }
TW() { "$BIN" --repo "$REPO" "$@"; }

echo "== step 1: throwaway Rust repo with a two-function test module =="
REPO="$SCRATCH/repo"; mkdir -p "$REPO/src"; git -C "$REPO" init -q -b main; gitc "$REPO"
cat > "$REPO/src/orders.rs" <<'EOF'
pub fn total(items: &[i64]) -> i64 {
    items.iter().sum()
}

pub fn discount(total: i64) -> i64 {
    total / 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_sums_items() {
        assert_eq!(total(&[1, 2]), 3);
    }

    #[test]
    fn discount_is_tenth() {
        assert_eq!(discount(100), 10);
    }
}
EOF
cat > "$REPO/src/notify.rs" <<'EOF'
pub fn notify(msg: &str) -> String {
    format!("notify: {msg}")
}
EOF
cat > "$REPO/test.sh" <<'EOF'
#!/bin/sh
grep -q 'total' src/orders.rs && grep -q 'notify' src/notify.rs
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

echo "== step 2 (B+C): claim one test fn, edit the other + an unclaimed file =="
git -C "$REPO" worktree add -q -b w1 "$SCRATCH/w1" main; gitc "$SCRATCH/w1"
# Edit only discount_is_tenth inside #[cfg(test)] mod tests, and notify().
sed -i 's/discount(100), 10/discount(200), 20/' "$SCRATCH/w1/src/orders.rs"
sed -i 's/notify: /sent: /' "$SCRATCH/w1/src/notify.rs"
git -C "$SCRATCH/w1" add -A; git -C "$SCRATCH/w1" commit -qm "w1 work"
check "w1 worktree committed" git -C "$SCRATCH/w1" rev-parse HEAD
TW task create "extend discount test and notify wording" --brief b > "$OUT/t1.json" 2>&1
TW assignment create --task T-1 --worktree "$SCRATCH/w1" > "$OUT/a1.json" 2>&1
grepq "assignment A-1 created" "$OUT/a1.json" '"assignment_id": "A-1"'
# Claim only the *other* test function: it must not cover discount_is_tenth.
TW claim acquire --assignment A-1 --node 'src/orders.rs#total_sums_items' > "$OUT/claim1.json" 2>&1
grepq "claim on total_sums_items granted" "$OUT/claim1.json" '"granted": true'
TW changeset register --assignment A-1 --worktree "$SCRATCH/w1" > "$OUT/c1.json" 2>&1
grepq "C-1 registered" "$OUT/c1.json" '"changeset_id": "C-1"'
grepq "register names the outside test fn (file#item)" "$OUT/c1.json" 'src/orders.rs#discount_is_tenth'
grepq "register names the outside whole file" "$OUT/c1.json" 'src/notify.rs (whole file)'
TW changeset status C-1 > "$OUT/c1s.json" 2>&1
grepq "C-1 yellow (outside claim + gates pending)" "$OUT/c1s.json" '"status": "yellow"'
grepq "next hint is refresh claims or changeset" "$OUT/c1s.json" '"next": "refresh claims or changeset"'
grepq "changeset status JSON carries claimed_vs_actual" "$OUT/c1s.json" '"claimed_vs_actual"'
python3 - "$OUT/c1s.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))
cva = v["claimed_vs_actual"]
ok = cva["outside_items"] == [{"file": "src/orders.rs", "item": "discount_is_tenth"}] \
    and [f["file"] for f in cva["outside_files"]] == ["src/notify.rs"]
sys.exit(0 if ok else 1)
PY
if [ $? -eq 0 ]; then pass "status CVA: outside item is the unclaimed test fn only"; else fail "status CVA: outside item is the unclaimed test fn only"; fi
grepq "status reason names the outside test fn" "$OUT/c1s.json" 'touched files outside claim: src/orders.rs#discount_is_tenth'
grepq "status reason names the outside file (whole file)" "$OUT/c1s.json" 'src/notify.rs (whole file)'
TW board > "$OUT/board.txt" 2>&1
grepq "text board names the outside test fn on the reason line" "$OUT/board.txt" 'touched files outside claim: src/orders.rs#discount_is_tenth'
grepq "text board names the outside whole file" "$OUT/board.txt" 'src/notify.rs (whole file)'
TW board --json > "$OUT/board.json" 2>&1
grepq "board items include individual test function discount_is_tenth" "$OUT/board.json" '"name": "discount_is_tenth"'
grepq "board items include individual test function total_sums_items" "$OUT/board.json" '"name": "total_sums_items"'
absent "no synthetic tests item anywhere on the board" "$OUT/board.json" '"name": "tests"'

cat > "$OUT/mcp1.jsonl" <<EOF
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"changeset_status","arguments":{"repo":"$REPO","changeset_id":"C-1"}}}
EOF
"$BIN" --repo "$REPO" serve-mcp < "$OUT/mcp1.jsonl" > "$OUT/mcp1-out.jsonl" 2>&1
TOOLS_LINE="$(grep '"id":1' "$OUT/mcp1-out.jsonl" | head -1)"
N_TOOLS="$(printf '%s' "$TOOLS_LINE" | grep -o '"name":' | wc -l | tr -d ' ')"
if [ "$N_TOOLS" = "19" ]; then pass "MCP tools/list advertises 19 tools (incl. changeset_conflicts)"; else fail "MCP tools/list advertises 19 tools (got $N_TOOLS)"; fi
grepq "MCP advertises changeset_conflicts" "$OUT/mcp1-out.jsonl" 'changeset_conflicts'
grepq "MCP changeset_status carries claimed_vs_actual" "$OUT/mcp1-out.jsonl" 'claimed_vs_actual'
grepq "MCP changeset_status names outside test fn" "$OUT/mcp1-out.jsonl" 'discount_is_tenth'

echo "== step 3 (A): fix claims, gates, land — claims survive the landing =="
TW claim acquire --assignment A-1 --node 'src/orders.rs#discount_is_tenth' --node 'src/notify.rs#notify' > "$OUT/claim2.json" 2>&1
grepq "correct claims granted" "$OUT/claim2.json" '"granted": true'
TW changeset status C-1 > "$OUT/c1s2.json" 2>&1
absent "outside-claim cleared live after claim fix" "$OUT/c1s2.json" 'outside claim'
TW gate run C-1 --tier 1 > "$OUT/g11.json" 2>&1
TW gate run C-1 --tier 2 > "$OUT/g12.json" 2>&1
grepq "C-1 tier-1 gate passed" "$OUT/g11.json" '"passed": true'
TW changeset status C-1 > "$OUT/c1s3.json" 2>&1
grepq "C-1 green after gates" "$OUT/c1s3.json" '"status": "green"'
TW claim list > "$OUT/claims-before.json" 2>&1
eq "three claims held before landing" "$(grep -c '"assignment_id": "A-1"' "$OUT/claims-before.json" || true)" "3"
TW land C-1 > "$OUT/land1.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "land C-1 exits 0"; else fail "land C-1 exits 0 (rc=$rc)"; fi
grepq "C-1 landed" "$OUT/land1.json" '"landed": true'
grepq "landing receipt carries claims_held" "$OUT/land1.json" '"claims_held"'
python3 - "$OUT/land1.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))
held = {(c["file"], c.get("item")) for c in v["claims_held"]}
want = {("src/orders.rs", "total_sums_items"), ("src/orders.rs", "discount_is_tenth"), ("src/notify.rs", "notify")}
sys.exit(0 if held == want else 1)
PY
if [ $? -eq 0 ]; then pass "claims_held names all three claims after landing"; else fail "claims_held names all three claims after landing"; fi
TW claim list > "$OUT/claims-after.json" 2>&1
eq "all three claims still held after landing (N1 fixed)" "$(grep -c '"assignment_id": "A-1"' "$OUT/claims-after.json" || true)" "3"
grepq "discount test claim still listed after landing" "$OUT/claims-after.json" '"item_name": "discount_is_tenth"'
TW events --since 0 > "$OUT/events.json" 2>&1
grepq "landed event records claims_held" "$OUT/events.json" 'claims_held'
git -C "$REPO" show main:src/notify.rs > "$OUT/main-notify.rs" 2>&1
grepq "main contains the landed notify change" "$OUT/main-notify.rs" 'sent:'
git -C "$REPO" show main:src/orders.rs > "$OUT/main-orders.rs" 2>&1
grepq "main contains the landed test change" "$OUT/main-orders.rs" 'discount(200), 20'
# Only assignment close releases them.
TW assignment close A-1 > "$OUT/close1.json" 2>&1
grepq "assignment A-1 closed" "$OUT/close1.json" '"closed": true'
TW claim list > "$OUT/claims-closed.json" 2>&1
eq "assignment close released every A-1 claim" "$(grep -c '"assignment_id": "A-1"' "$OUT/claims-closed.json" || true)" "0"

echo "== step 4 (D): conflicting pair, merge-tree hunk detail without raw git =="
for w in w2 w3; do git -C "$REPO" worktree add -q -b "$w" "$SCRATCH/$w" main; gitc "$SCRATCH/$w"; done
check "w2/w3 worktrees forked from post-land main" test -d "$SCRATCH/w3/src"
sed -i 's/items.iter().sum()/items.iter().sum::<i64>() + 0/' "$SCRATCH/w2/src/orders.rs"
sed -i 's/items.iter().sum()/items.iter().fold(0, |a, b| a + b)/' "$SCRATCH/w3/src/orders.rs"
for w in w2 w3; do git -C "$SCRATCH/$w" add -A; git -C "$SCRATCH/$w" commit -qm "$w work"; done
TW task create "total via turbofish sum" --brief b > "$OUT/t2.json" 2>&1
TW task create "total via fold" --brief b > "$OUT/t3.json" 2>&1
TW assignment create --task T-2 --worktree "$SCRATCH/w2" > "$OUT/a2.json" 2>&1
TW assignment create --task T-3 --worktree "$SCRATCH/w3" > "$OUT/a3.json" 2>&1
TW changeset register --assignment A-2 --worktree "$SCRATCH/w2" > "$OUT/c2.json" 2>&1
TW changeset register --assignment A-3 --worktree "$SCRATCH/w3" > "$OUT/c3.json" 2>&1
grepq "C-2 registered" "$OUT/c2.json" '"changeset_id": "C-2"'
grepq "C-3 registered" "$OUT/c3.json" '"changeset_id": "C-3"'
TW changeset status C-3 > "$OUT/c3s.json" 2>&1
grepq "C-3 is red" "$OUT/c3s.json" '"status": "red"'
grepq "C-3 reason names earlier C-2 and the file" "$OUT/c3s.json" 'conflicts with C-2 in src/orders.rs'
MAIN_BEFORE="$(git -C "$REPO" rev-parse main)"
W3_HEAD="$(git -C "$SCRATCH/w3" rev-parse HEAD)"
TW changeset conflicts C-3 > "$OUT/conf3.txt" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "changeset conflicts C-3 exits 0"; else fail "changeset conflicts C-3 exits 0 (rc=$rc)"; fi
grepq "conflicts text names the file vs C-2" "$OUT/conf3.txt" 'src/orders.rs (vs C-2)'
grepq "conflicts text shows a merge-tree hunk header" "$OUT/conf3.txt" '@@'
TW changeset conflicts C-3 --json > "$OUT/conf3.json" 2>&1
grepq "conflicts --json names the counterparty" "$OUT/conf3.json" '"against": "C-2"'
grepq "conflicts --json names the file" "$OUT/conf3.json" '"file": "src/orders.rs"'
grepq "conflicts --json carries hunk headers" "$OUT/conf3.json" '@@'
TW changeset conflicts C-2 > "$OUT/conf2.txt" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "changeset conflicts C-2 exits 0"; else fail "changeset conflicts C-2 exits 0 (rc=$rc)"; fi
grepq "earlier changeset reports no conflicts (it only blocks later one)" "$OUT/conf2.txt" 'C-2: no conflicts'
TW changeset conflicts C-2 --json > "$OUT/conf2.json" 2>&1
grepq "empty conflict list in --json form" "$OUT/conf2.json" '"conflicts": []'
cat > "$OUT/mcp2.jsonl" <<EOF
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"changeset_conflicts","arguments":{"repo":"$REPO","changeset_id":"C-3"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"changeset_conflicts","arguments":{"repo":"$REPO","changeset_id":"C-2"}}}
EOF
"$BIN" --repo "$REPO" serve-mcp < "$OUT/mcp2.jsonl" > "$OUT/mcp2-out.jsonl" 2>&1
grepq "MCP changeset_conflicts names counterparty C-2" "$OUT/mcp2-out.jsonl" '"against":"C-2"'
grepq "MCP changeset_conflicts names the file" "$OUT/mcp2-out.jsonl" '"file":"src/orders.rs"'
grepq "MCP changeset_conflicts carries hunk headers" "$OUT/mcp2-out.jsonl" '@@'
grepq "MCP changeset_conflicts empty list for C-2" "$OUT/mcp2-out.jsonl" '"conflicts":[]'
eq "main untouched by conflict inspection" "$(git -C "$REPO" rev-parse main)" "$MAIN_BEFORE"
eq "w3 HEAD untouched by conflict inspection" "$(git -C "$SCRATCH/w3" rev-parse HEAD)" "$W3_HEAD"
check "w3 worktree clean after conflict inspection" test -z "$(git -C "$SCRATCH/w3" status --porcelain)"

echo "== step 5: summary =="
echo "board snapshot (text):"
TW board 2>/dev/null | sed -n '1,40p'
echo
echo "DEMO-PHASE22 RESULT: $PASS passed, $FAIL failed"
if [ "$FAIL" -eq 0 ]; then
  echo "DEMO-PHASE22 PASS (scratch removed; set KEEP=1 to keep it)"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$SCRATCH"
  exit 0
else
  echo "DEMO-PHASE22 FAIL:$FAILED"
  echo "scratch kept for inspection: $SCRATCH"
  exit 1
fi
