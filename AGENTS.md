# Rules for delegated agents

You are one of several agents working in this repo under an orchestrator. These rules apply to every task you receive here, in addition to your task's own spec.

- NEVER commit, push, stage, tag, or run any git command that changes state — nor any command that mutates a remote or publishes/deploys (gh, npm publish, deploy CLIs). The orchestrator owns all of these.
- Touch only the files your task lists. Never touch files it excludes. If you need to change a file outside your list, stop and report it instead.
- Run the machine-checkable acceptance criteria from your task (tests/build/lint) before finishing, and report their results.
- Never weaken, skip, disable, or delete tests, lint rules, or checks to make acceptance criteria pass. If criteria fail and you cannot fix the cause within scope, report the failure honestly.
- If your task is contradictory or impossible, stop and report the exact problem. Never guess, never silently reinterpret the task.
- Secrets come from the environment only. Never print, log, or hardcode credentials or API keys.
- UI work: implement the design spec exactly; do not invent design. The written design spec always wins over the defaults that follow. Where the spec is silent: one typeface (two max), one spacing scale, one accent color; no gradients-on-everything, no glassmorphism, no decorative emoji in UI copy, no marketing filler.
- Finish with a short report: files changed (one line each), acceptance criteria results, anything your task missed.

## GUI runs (binding)
- NEVER open the app on the user's desktop. Every `./target/debug/omarchygram`, `cargo run`, `--smoke`, `--probe` or screenshot run MUST be wrapped as `bin/headless <command>` (private headless Wayland compositor). If `bin/headless` is unavailable or fails, skip the GUI check and say so in your report — the orchestrator runs the gate. A visible window on the user's screen is a hard rule violation.
- Never send synthetic keyboard/mouse input to the desktop (no xdotool/wtype/ydotool/wlrctl).
