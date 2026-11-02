#!/usr/bin/env bash
# Tower Phase 2.3 demo: self-reporting against a stub gh, end to end.
#
#   (a) an explicit `tower report` files an issue via the stub with the
#       expected repo/title/labels/signature, and a planted fake secret
#       in --command never reaches the filed body;
#   (b) the same signature again coalesces (occurrence count 2, no
#       second issue);
#   (c) with a failing gh the report stays queued and `tower report`
#       still exits 0;
#   (d) a later `report flush` — and the opportunistic flush at the
#       start of `board` — files queued reports;
#   plus MCP (23 tools, report_create files), a slow operation drafted
#   automatically, enabled=false recording locally only, and the
#   5-per-day filing cap.
#
# Everything happens in a mktemp scratch dir with a scratch TOWER_HOME.
# No real GitHub issue is ever touched: TOWER_GH points at the stubs.
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/demo-common.sh"
BIN="${TOWER_BIN:-$ROOT/target/release/tower}"
if [ ! -x "$BIN" ]; then
  echo "building release binary..."
  (cd "$ROOT" && "${CARGO:-cargo}" build --release) || { echo "FAIL build"; exit 1; }
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tower-demo23-XXXXXX")"
export TOWER_HOME="$SCRATCH/home"
OUT="$SCRATCH/out"; mkdir -p "$OUT"
PASS=0; FAIL=0; FAILED=""

pass() { echo "PASS  $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL  $1"; FAIL=$((FAIL + 1)); FAILED="$FAILED $1"; }
check() { local name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }
grepq() { if grep -qF -- "$3" "$2" 2>/dev/null; then pass "$1"; else fail "$1"; fi; }
eq() { if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (got '$2', want '$3')"; fi; }
absent() { if grep -qF -- "$3" "$2" 2>/dev/null; then fail "$1"; else pass "$1"; fi; }

gitc() { git -C "$1" config user.email t@t; git -C "$1" config user.name T; }
TW() { "$BIN" --repo "$REPO" "$@"; }
creates() { grep -c 'issue create' "$GH_CREATED" 2>/dev/null || true; }

echo "== step 1: scratch repo + stub gh (ok and failing modes) =="
REPO="$SCRATCH/repo"; mkdir -p "$REPO"; git -C "$REPO" init -q -b main; gitc "$REPO"
echo x > "$REPO/x.txt"
cat > "$REPO/tower.toml" <<'EOF'
[repo]
main_branch = "main"

[feedback]
repo = "Holy-Coders/tower"
enabled = true
EOF
git -C "$REPO" add -A; git -C "$REPO" commit -qm base
check "demo repo base commit" git -C "$REPO" rev-parse main

GH_LOG="$SCRATCH/gh.log"; : > "$GH_LOG"
GH_CREATED="$SCRATCH/gh-created.log"; : > "$GH_CREATED"
cat > "$SCRATCH/gh-ok.sh" <<EOF
#!/bin/sh
echo "\$@" >> '$GH_LOG'
if [ "\$1 \$2" = "issue list" ]; then echo '[]'; exit 0; fi
if [ "\$1 \$2" = "issue create" ]; then echo "\$@" >> '$GH_CREATED'; echo 'https://github.com/Holy-Coders/tower/issues/42'; exit 0; fi
if [ "\$1 \$2" = "issue comment" ]; then echo 'https://github.com/Holy-Coders/tower/issues/42#issuecomment-1'; exit 0; fi
exit 0
EOF
cat > "$SCRATCH/gh-fail.sh" <<EOF
#!/bin/sh
echo "\$@" >> '$GH_LOG'
echo 'gh: not authenticated' >&2
exit 1
EOF
chmod +x "$SCRATCH/gh-ok.sh" "$SCRATCH/gh-fail.sh"
check "stub gh scripts executable" test -x "$SCRATCH/gh-ok.sh"

export TOWER_GH="$SCRATCH/gh-ok.sh"
TW init --write-config > "$OUT/init.json" 2>&1
grepq "init registered repo" "$OUT/init.json" '"main_branch": "main"'
TW task create "demo task" --brief b > "$OUT/t1.json" 2>&1
grepq "task T-1 exists for report context" "$OUT/t1.json" '"task_id": "T-1"'

echo "== step 2 (a): explicit CLI report files via stub, secret scrubbed =="
TW report --kind bug --title "Board contradicts changeset status C-12" \
  --body "board said green, changeset status said red" \
  --command "tower --repo $REPO changeset status C-12 --token FAKESECRET999 --api-key=sekrit777" \
  > "$OUT/r1.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "report exits 0"; else fail "report exits 0 (rc=$rc)"; fi
grepq "report state is filed" "$OUT/r1.json" '"state": "filed"'
grepq "filed URL recorded" "$OUT/r1.json" '"filed_url": "https://github.com/Holy-Coders/tower/issues/42"'
grepq "stub saw issue create on the feedback repo" "$GH_LOG" 'issue create --repo Holy-Coders/tower'
grepq "stub saw the title" "$GH_LOG" 'Board contradicts changeset status'
grepq "stub saw tower-auto-report label" "$GH_LOG" '--label tower-auto-report'
grepq "stub saw kind label bug" "$GH_LOG" '--label bug'
grepq "filed body carries signature marker" "$GH_LOG" '<!-- tower-signature:'
grepq "filed body carries auto context (Tower version)" "$GH_LOG" 'tower_version'
grepq "filed body carries task context T-1" "$GH_LOG" 'T-1'
absent "planted token never reached the filed issue" "$GH_LOG" 'FAKESECRET999'
absent "planted api-key never reached the filed issue" "$GH_LOG" 'sekrit777'
absent "home path of the scratch repo never filed" "$GH_LOG" "$HOME/"
grepq "secret value replaced with [redacted]" "$GH_LOG" '[redacted]'
eq "exactly one issue created so far" "$(creates)" "1"

echo "== step 3 (b): same signature coalesces, no second issue =="
TW report --kind bug --title "board contradicts changeset status c-99" \
  --body "again, with different id and case" \
  --command "tower --repo $REPO changeset status C-99 --token OTHERSECRET111 --api-key=sekrit000" \
  > "$OUT/r2.json" 2>&1
grepq "coalesced occurrence count is 2" "$OUT/r2.json" '"occurrence_count": 2'
grepq "coalesced report stays filed at the same URL" "$OUT/r2.json" '"filed_url": "https://github.com/Holy-Coders/tower/issues/42"'
eq "still exactly one issue created" "$(creates)" "1"
TW report list > "$OUT/list1.json" 2>&1
python3 - "$OUT/list1.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))
sys.exit(0 if len(v["reports"]) == 1 and v["reports"][0]["occurrence_count"] == 2 else 1)
PY
if [ $? -eq 0 ]; then pass "report list shows one coalesced report with count 2"; else fail "report list shows one coalesced report with count 2"; fi

echo "== step 4: MCP advertises 23 tools and report_create files =="
cat > "$OUT/mcp1.jsonl" <<EOF
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"report_create","arguments":{"repo":"$REPO","kind":"inefficiency","title":"CLI and MCP disagree on board","body":"MCP board_get differed from CLI board","command":"tower board --password hunter2demo"}}}
EOF
"$BIN" --repo "$REPO" serve-mcp < "$OUT/mcp1.jsonl" > "$OUT/mcp1-out.jsonl" 2>&1
TOOLS_LINE="$(grep '"id":1' "$OUT/mcp1-out.jsonl" | head -1)"
N_TOOLS="$(printf '%s' "$TOOLS_LINE" | grep -o '"name":' | wc -l | tr -d ' ')"
eq "MCP tools/list advertises 23 tools" "$N_TOOLS" "23"
grepq "MCP advertises report_create" "$OUT/mcp1-out.jsonl" 'report_create'
grepq "report_create description fences off orchestrated code" "$OUT/mcp1-out.jsonl" 'Do NOT report bugs in the code you are orchestrating'
grepq "MCP report_create result is filed" "$OUT/mcp1-out.jsonl" '"state":"filed"'
grepq "stub saw kind label inefficiency" "$GH_LOG" '--label inefficiency'
absent "planted MCP password never filed" "$GH_LOG" 'hunter2demo'
eq "two issues created after MCP report" "$(creates)" "2"

echo "== step 5 (c)+(d): failing gh queues silently; flush and board file later =="
export TOWER_GH="$SCRATCH/gh-fail.sh"
TW report --kind bug --title "Watch radar missed a collision" \
  --body "radar said clean while two worktrees touched the same item" \
  > "$OUT/r3.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "report with failing gh still exits 0"; else fail "report with failing gh still exits 0 (rc=$rc)"; fi
grepq "report stays queued when gh fails" "$OUT/r3.json" '"state": "queued"'
eq "no new issue created while gh fails" "$(creates)" "2"
TW report list > "$OUT/list2.json" 2>&1
grepq "report list shows the queued report" "$OUT/list2.json" '"state": "queued"'
TW report flush > "$OUT/flush-fail.json" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "report flush with failing gh exits 0"; else fail "report flush with failing gh exits 0 (rc=$rc)"; fi
grepq "failed flush files nothing" "$OUT/flush-fail.json" '"filed": 0'

export TOWER_GH="$SCRATCH/gh-ok.sh"
TW report flush > "$OUT/flush-ok.json" 2>&1
grepq "later flush files the queued report" "$OUT/flush-ok.json" '"filed": 1'
eq "three issues created after flush" "$(creates)" "3"

# Opportunistic flush: queue under the failing stub, then `board` files it.
export TOWER_GH="$SCRATCH/gh-fail.sh"
TW report --kind inefficiency --title "Changeset refresh felt sluggish" \
  --body "refresh visibly stalled mid-work" > "$OUT/r4.json" 2>&1
grepq "second queued report recorded" "$OUT/r4.json" '"state": "queued"'
export TOWER_GH="$SCRATCH/gh-ok.sh"
TW board > "$OUT/board.txt" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "board still exits 0 around opportunistic flush"; else fail "board still exits 0 around opportunistic flush (rc=$rc)"; fi
eq "board opportunistically filed the queued report" "$(creates)" "4"
TW report list > "$OUT/list3.json" 2>&1
grepq "the board-flushed report is listed" "$OUT/list3.json" 'Changeset refresh felt sluggish'

echo "== step 6: slow operation is drafted automatically =="
TOWER_SLOW_REPORT_MS=1 TW board > "$OUT/board-slow.txt" 2>&1; rc=$?
if [ "$rc" -eq 0 ]; then pass "slow board still exits 0"; else fail "slow board still exits 0 (rc=$rc)"; fi
TW report list > "$OUT/list4.json" 2>&1
grepq "slow board drafted an automatic inefficiency report" "$OUT/list4.json" 'Slow operation: board'
python3 - "$OUT/list4.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))
slow = [r for r in v["reports"] if r["title"].startswith("Slow operation: board")]
sys.exit(0 if slow and slow[0]["kind"] == "inefficiency" else 1)
PY
if [ $? -eq 0 ]; then pass "automatic slow report has kind inefficiency"; else fail "automatic slow report has kind inefficiency"; fi

echo "== step 7: enabled=false records locally; daily cap holds at 5 =="
BEFORE="$(creates)"
edit_fixture_file "$REPO/tower.toml" 'enabled = true' 'enabled = false' || exit 1
for w in red blue green black white pink; do
  TW report --kind bug --title "cap probe $w failure" --body "queued only while disabled" > "$OUT/cap-$w.json" 2>&1
done
TW report flush > "$OUT/flush-disabled.json" 2>&1
grepq "disabled flush is a no-op" "$OUT/flush-disabled.json" '"filed": 0'
eq "nothing filed while feedback disabled" "$(creates)" "$BEFORE"
TW report list > "$OUT/list5.json" 2>&1
grepq "disabled reports still recorded locally as queued" "$OUT/list5.json" 'cap probe red failure'
edit_fixture_file "$REPO/tower.toml" 'enabled = false' 'enabled = true' || exit 1
TW report flush > "$OUT/flush-cap.json" 2>&1
TW report list > "$OUT/list6.json" 2>&1
python3 - "$OUT/list6.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))
filed = [r for r in v["reports"] if r["state"] == "filed"]
queued = [r for r in v["reports"] if r["state"] == "queued"]
sys.exit(0 if len(filed) == 5 and len(queued) >= 1 else 1)
PY
if [ $? -eq 0 ]; then pass "daily cap: exactly 5 filed, the rest stay queued (never dropped)"; else fail "daily cap: exactly 5 filed, the rest stay queued (never dropped)"; fi
TW events --since 0 > "$OUT/events.json" 2>&1
grepq "report_created event recorded" "$OUT/events.json" 'report_created'
grepq "report_filed event recorded" "$OUT/events.json" 'report_filed'

echo "== step 8: summary =="
echo "report list snapshot:"
TW report list 2>/dev/null | sed -n '1,40p'
echo
echo "DEMO-PHASE23 RESULT: $PASS passed, $FAIL failed"
if [ "$FAIL" -eq 0 ]; then
  echo "DEMO-PHASE23 PASS (scratch removed; set KEEP=1 to keep it)"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$SCRATCH"
  exit 0
else
  echo "DEMO-PHASE23 FAIL:$FAILED"
  echo "scratch kept for inspection: $SCRATCH"
  exit 1
fi
