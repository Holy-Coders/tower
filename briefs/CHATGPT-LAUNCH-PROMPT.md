# Prompt for ChatGPT — Launch the Tower Build (starter only)

Give this to ChatGPT in a session that can run terminal commands on this Mac. Everything between the lines is the prompt.

---

Launch the Tower build on this Mac using Muse Code. Do not write any code yourself; your only job is to start it and confirm it is running.

Run this command in a terminal:

```
cd ~/code/holy-coders/tower && muse exec --model muse-wonder-full-1.0 "$(cat briefs/BUILD-BRIEF.md)"
```

If the `--model` flag is rejected, drop it and rerun. If `muse` asks to sign in or shows a device code, stop and tell Aaron; do not proceed past a sign-in by yourself.

Context, so you understand what you started: the brief makes Muse Code implement a Rust tool called Tower, following `tower-spec.md` in that folder. Muse Code does all the implementation and all the logging itself (`PROGRESS.md` at every stage boundary, `reports/stage-<n>-report.md` per stage). Dobby, Aaron's Muse assistant, is monitoring the build by reading those files and may leave instructions in `briefs/DIRECTION.md`, which Muse Code checks at every stage boundary.

After launching: confirm the command is running and tell Aaron it has started. If the run later ends before the build is complete (check `PROGRESS.md` in that folder for the last completed stage), tell Aaron it stopped and at which stage. Do not relaunch on your own; Aaron will have Dobby review first, and if he asks you to continue, run the exact same command again. Muse Code resumes from `PROGRESS.md`; it does not start over.
