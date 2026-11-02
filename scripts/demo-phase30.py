#!/usr/bin/env python3
"""Bounded synthetic dogfood of Tower's real scheduler/adapter/handoff interfaces.

Uses only newly created temporary git worktrees, fake local workers and pure
integer arithmetic. It never runs an AI agent, installs Tower or accesses providers.
The retained fixture path and measured receipts are printed as JSON.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tower", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/tower")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.tower.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="tower-phase30-", dir="/tmp")).resolve()
    repo = root / "repo"
    repo.mkdir()
    env = dict(os.environ, TOWER_HOME=str(root / "state"), TOWER_GH=str(root / "disabled-gh"))
    started = time.monotonic()
    observations = {}

    def command(argv, cwd=repo, check=True):
        result = subprocess.run([str(x) for x in argv], cwd=cwd, env=env, text=True, capture_output=True)
        if check and result.returncode:
            raise RuntimeError(f"{argv[0:3]} failed in fixture: {result.stderr}\n{result.stdout}")
        return result

    def git(*argv, cwd=repo, check=True):
        return command(["git", *argv], cwd, check).stdout.strip()

    def tower(*argv, check=True):
        result = command([binary, "--repo", repo, *argv], check=check)
        if not check:
            return result
        return json.loads(result.stdout)

    def action(name, value):
        path = root / "input.json"
        path.write_text(json.dumps(value))
        return tower("reconcile", name, "--input", path)

    def commit(cwd, message):
        git("add", "-A", cwd=cwd)
        git("commit", "-m", message, cwd=cwd)
        return git("rev-parse", "HEAD", cwd=cwd)

    def tick():
        return tower("schedule", "tick", "--holder", "synthetic-pilot")

    def until(predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(0.1)
        raise RuntimeError("bounded synthetic pilot timed out; inspect retained fixture " + str(root))

    permit = root / "permit-reconciler"
    fake = root / "fake_reconciler.py"
    fake.write_text('''import os, pathlib, subprocess, sys, time
prompt = sys.stdin.read()
assert "clamp negative totals" in prompt and "apply the discount" in prompt
assert "PROVISIONAL CONTRACT" in prompt
run = pathlib.Path(os.environ["TOWER_RUN_DIR"])
with pathlib.Path(sys.argv[1]).parent.joinpath("fake-launches").open("a") as launches: launches.write("launch\\n")
(run / "fixture-started").write_text("started")
deadline = time.monotonic() + 20
while not pathlib.Path(sys.argv[1]).exists():
    if time.monotonic() > deadline: raise RuntimeError("fixture permit timeout")
    time.sleep(.05)
stub = pathlib.Path(sys.argv[2]).read_text().strip()
subprocess.run(["git", "merge", "--ff-only", stub], check=True)
pathlib.Path("shared.py").write_text("def settled_total(subtotal_cents, discount_cents):\\n    return max(0, subtotal_cents - discount_cents)\\n")
subprocess.run(["git", "add", "shared.py"], check=True)
subprocess.run(["git", "commit", "-m", "synthetic combined implementation"], check=True)
''')
    stub_oid = root / "stub-oid"
    git("init", "-b", "main")
    git("config", "user.name", "Tower Synthetic Fixture")
    git("config", "user.email", "fixture@example.invalid")
    (repo / "shared.py").write_text("def settled_total(subtotal_cents, discount_cents):\n    return subtotal_cents\n")
    (repo / "verify.py").write_text("from shared import settled_total\nassert settled_total(100,20)==80\nassert settled_total(100,150)==0\nassert settled_total(100,0)==100\n")
    (repo / ".gitignore").write_text("__pycache__/\n")
    (repo / "tower.toml").write_text(
        '[repo]\nmain_branch="main"\n[gates]\ncheck="python3 verify.py"\ntimeout_secs=10\nresources=["synthetic-database"]\n'
        '[limits]\nmax_concurrent_assignments=2\nmax_concurrent_reconciliations=1\n[feedback]\nenabled=false\n'
        '[scheduler.agent]\nprogram=' + json.dumps(sys.executable) + '\nargs=' + json.dumps([str(fake), str(permit), str(stub_oid)]) + '\ntimeout_secs=30\n'
    )
    base = commit(repo, "synthetic fixture base")
    tower("init", "--main-branch", "main")
    workers = []
    for name, goal, implementation, tests in [
        ("a", "clamp negative totals", "return max(0, subtotal_cents)", ["python3 -c 'from shared import settled_total; assert settled_total(100,150)==0'"]),
        ("b", "apply the discount", "return subtotal_cents - discount_cents", ["python3 -c 'from shared import settled_total; assert settled_total(100,20)==80'"]),
    ]:
        path = root / name
        git("worktree", "add", "-b", "worker-" + name, path, "main")
        (path / "shared.py").write_text("def settled_total(subtotal_cents, discount_cents):\n    " + implementation + "\n")
        draft = commit(path, "synthetic intent " + name)
        task = tower("task", "create", "Synthetic consumer " + name, "--brief", goal)["task_id"]
        spec = root / ("spec-" + name + ".json")
        spec.write_text(json.dumps({"assumptions": ["nonnegative small integer cents", "stable two-argument settled_total interface"], "acceptance_tests": tests}))
        tower("schedule", "configure", task, "--spec", spec)
        aid = tower("assignment", "create", "--task", task, "--worktree", path)["assignment_id"]
        tower("claim", "acquire", "--assignment", aid, "--node", "consumer_" + name + ".py")
        if name == "a":
            tower("claim", "acquire", "--assignment", aid, "--node", "shared.py")
        workers.append({"name": name, "path": path, "assignment": aid, "draft": draft})
    tick()  # Automatically discover both recorded intents; pause ACKs still required.
    records = tower("reconcile", "list")
    assert len(records) == 1, records
    reconciliation = records[0]
    rid = reconciliation["id"]
    observations["automatic_overlap_discovered_seconds"] = round(time.monotonic() - started, 3)
    for worker in workers:
        action("pause", {"id": rid, "assignment_id": worker["assignment"], "head": worker["draft"]})
    stub_tree = root / "stub"
    git("worktree", "add", "-b", "authored-stub", stub_tree, base)
    stub_source = 'def settled_total(subtotal_cents, discount_cents):\n    raise NotImplementedError("TOWER_PROVISIONAL ' + rid + '")\n'
    (stub_tree / "shared.py").write_text(stub_source)
    stub = commit(stub_tree, "explicit provisional contract")
    stub_oid.write_text(stub)
    contract = action("contract", {"id": rid, "interface": "settled_total(nonnegative integer subtotal_cents, discount_cents) -> nonnegative integer cents; provisional behavior explicitly raises until verified replacement", "stub_commit": stub})
    for worker in workers:
        git("merge", "--no-commit", stub, cwd=worker["path"], check=False)
        (worker["path"] / "shared.py").write_text(stub_source)
        head = commit(worker["path"], "acknowledge provisional contract")
        action("stub-ack", {"id": rid, "assignment_id": worker["assignment"], "head": head, "version": contract["contract"]["version"]})
    tick()
    run = tower("schedule", "status")["runs"][0]
    until(lambda: (Path(run["run_dir"]) / "fixture-started").exists())
    # While the real Tower adapter owns the fake reconciliation process, both
    # consumers make independent committed progress against the explicit stub.
    independent_started = time.monotonic()
    for worker in workers:
        (worker["path"] / ("consumer_" + worker["name"] + ".py")).write_text("# independent progress while shared implementation is pending\n")
        worker["progress"] = commit(worker["path"], "independent consumer progress")
        cs = tower("changeset", "register", "--assignment", worker["assignment"], "--worktree", worker["path"])["changeset_id"]
        refused = tower("land", cs, check=False)
        assert refused.returncode != 0 and "unresolved shared reconciliation" in refused.stderr
    observations["both_consumers_progressed_while_reconciler_running"] = True
    observations["independent_progress_seconds"] = round(time.monotonic() - independent_started, 3)
    permit.write_text("complete the pure fixture")

    def returned():
        tick()  # New process each tick dogfoods persisted coordinator restart.
        value = action("show", {"id": rid})
        return value if value["state"] == "returned" else None

    returned_record = until(returned)
    result = returned_record["result_commit"]
    for worker in workers:
        git("merge", "--no-edit", result, cwd=worker["path"])
        assert git("merge-base", "--is-ancestor", worker["draft"], "HEAD", cwd=worker["path"]) == ""
        assert (worker["path"] / ("consumer_" + worker["name"] + ".py")).exists()
        cs = tower("changeset", "register", "--assignment", worker["assignment"], "--worktree", worker["path"])["changeset_id"]
        proof = tower("gate", "run", cs, "--tier", "1")
        assert proof["passed"] is True, proof
        action("resume", {"id": rid, "assignment_id": worker["assignment"], "changeset_id": cs})
        worker["validated_changeset"] = cs
    final_record = action("show", {"id": rid})
    assert final_record["state"] == "completed", final_record
    assert tower("reconcile", "discover") == [], "identical returned shared scope must not cause reconciliation loop"
    for cs in [returned_record["result_changeset"], *[worker["validated_changeset"] for worker in workers]]:
        landed = tower("land", cs)
        assert landed["landed"] is True, landed
    command([sys.executable, "verify.py"])
    assert "TOWER_PROVISIONAL" not in (repo / "shared.py").read_text()
    observations["all_three_verified_changesets_integrated"] = True
    all_runs = tower("schedule", "status")["runs"]
    assert len(all_runs) == 1, all_runs
    launches = (root / "fake-launches").read_text().splitlines()
    assert len(launches) == 1, launches
    assert tower("resources", "list") and not any(q["state"] in ("queued", "running") for q in tower("resources", "list"))
    observations["total_seconds"] = round(time.monotonic() - started, 3)
    metrics = action("metrics", {"id": rid})
    receipt = {
        "kind": "synthetic_local_fixture", "tower_version": command([binary, "--version"]).stdout.strip(),
        "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "fixture": str(root), "tower_home": str(root / "state"), "repo": str(repo), "holder": "synthetic-pilot",
        "production_agents_or_providers_used": False,
        "reconciliation_id": rid, "reconciliation_task_id": reconciliation["task_id"], "launches": len(launches), "durable_attempts": len(all_runs), "result_commit": result,
        "state": final_record["state"], "observations": observations, "metrics": metrics,
        "speedup_claim": None, "real_project_pilot": "pending coordinated handoff",
    }
    rendered = json.dumps(receipt, indent=2) + "\n"
    (root / "dogfood-receipt.json").write_text(rendered)
    if args.output:
        args.output.write_text(rendered)
    print(rendered, end="")


if __name__ == "__main__":
    main()
