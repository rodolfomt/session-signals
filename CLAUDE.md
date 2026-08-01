# CLAUDE.md — Session Signals

> Display name: **Session Signals**. Internal codenames are deliberately kept and
> NOT renamed (they're opaque to users): the crate/repo-local name `cc-beacon`,
> the lib `beacon_lib`, the store file `beacon.json`, the `X-Beacon-Token` header,
> the `beacon-capture` hook marker, and the `com.beacon.cc` bundle identifier.

A lightweight desktop status indicator for Claude Code users. A tray/menu-bar
icon shows a rollup status; a floating always-on-top widget shows a per-session
breakdown. Traffic-light semantics. Themes are swappable.

This file is the standing context for every Claude Code turn in this repo. The
full requirements live in `docs/SPEC.md`. The app was built in four phases (all
complete — see **Build phases** below).

---

## Locked decisions

- **Stack:** Tauri 2 + React 19 + TypeScript + Vite. Rust only for the Tauri
  shell (tray, windows, local listener). All UI in React.
- **Detection:** Claude Code **hooks** POST to a localhost HTTP listener owned by
  the app. No terminal scraping, no process inspection.
- **Multi-session:** tracked per `session_id`; the floating widget shows one row
  per live session. The tray shows a single rollup.
- **Form factor:** tray/menu-bar icon **and** a floating, always-on-top,
  draggable widget window.
- **Notifications:** configurable, per-state.
- **Privacy:** fully local. Listener binds `127.0.0.1` only. No telemetry, no
  network egress, ever.

## State model

| State | Color | Meaning (user POV) | Set by |
|---|---|---|---|
| Needs you | 🔴 Red | Blocked on you — permission, choice, or answer | `Notification` (type ∈ permission_prompt, elicitation_dialog) |
| Working | 🟠 Orange | Actively running — don't interrupt | `UserPromptSubmit`; `PostToolUse` heartbeat; a `Stop` deferred by live subagents |
| Ready | 🟢 Green | Finished its turn — okay to give new instructions | `Stop`, `SubagentStop`, `SessionStart` |
| Waiting for Review | 🔴▲ Red triangle | Finished, and you flagged it for review first | `Stop`/`StopFailure`/`PostCompact` on a session flagged via `set_review_flag` |
| None / stale | ⚪ Grey | No live session / session went silent | `SessionEnd`, or stale timeout |

**Tray rollup priority:** Red > Review (red triangle) > Orange > Green > Grey.
(Tray is red if *any* session needs you; the triangle if any is waiting for
review and none needs you; orange if any is working and neither of those;
etc.)

**Waiting for Review** is the app's first user→engine command, not a
hook-derived state: the widget's per-row toggle calls `set_review_flag(id,
bool)`, setting `Session.review_when_done`. Setting it changes nothing by
itself — it only changes what the session's *next* terminal transition
(`Stop` / `StopFailure` / `PostCompact`) resolves to: `WaitingReview` instead
of `Ready` (see `engine::terminal_state`, the single source of truth both the
`Stop` and `SubagentStop` arms share). **Clearing** it is the one exception:
if the session has *already* finished into `WaitingReview`, clearing the flag
restores `Ready` immediately — waiting for another `Stop` (which may never
arrive again for an already-finished session) would leave a stale red
triangle after the user said "never mind." This asymmetry is deliberate:
plain `Ready` is also a brand-new session's resting state, and there is no
way to tell "just finished" apart from "hasn't started a turn yet" from state
alone, so *setting* the flag on a `Ready` row never jumps it to
`WaitingReview` early. The flag does not survive a `SessionEnd`/restart —
"keep it simple." Shape (an upward triangle), not a fifth hue, carries the
meaning: it reuses the existing red so the glyph still reads correctly in
greyscale and at 16px.

**A finished session is not "free" while subagents are still running.** A
main-agent `Stop` while `subagent_count > 0` (and the session isn't blocked)
defers the terminal transition — the row shows Working, not Ready/Waiting for
Review — until the *last* matching `SubagentStop` releases it. A genuine
block (`NeedsYou`) outranks the backlog in both directions: it neither gets
forced to Working by a live-subagent `Stop`, nor does a draining
`SubagentStop` clear it. See the `Stop`/`SubagentStop` arms in `engine.rs` and
`Session.awaiting_subagents`.

## Hook contract

The app installs HTTP hooks into `~/.claude/settings.json` (merged
non-destructively). Every relevant event POSTs the same JSON Claude Code would
pass on stdin to a single endpoint:

```
POST http://127.0.0.1:4317/hook
body: { hook_event_name, session_id, cwd, ... }   // async, non-blocking
```

Events wired: `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`
(heartbeat), `Notification`, `Stop`, `SubagentStop`, `SessionEnd`. (The full
installed set lives in `hooks.rs:EVENTS` — this is the subset that drives state.)

**Listener derivation logic** (keyed by `session_id`):

```
SessionStart            → upsert session, state = READY,  lastSeen = now
UserPromptSubmit        → state = WORKING,                lastSeen = now
PreToolUse:
  AskUserQuestion     |
  ExitPlanMode          → state = NEEDS_YOU,              lastSeen = now
                          (a block escalates regardless of agent_id)
  any other tool        → state = WORKING,                lastSeen = now
PostToolUse:
  AskUserQuestion     |
  ExitPlanMode          → state = WORKING (main agent only — you answered)
  any other tool        → lastSeen = now (heartbeat; keep current state)
Notification:
  permission_prompt   |
  elicitation_dialog    → state = NEEDS_YOU,              lastSeen = now
  idle_prompt         |
  auth_success        |
  elicitation_complete  → ignore, no lastSeen touch (known-inert: idle_prompt
                          in particular fires BECAUSE the session is idle, so
                          it must never postpone the stale timer — a past
                          regression heartbeated here and let idle sessions
                          stay green indefinitely; see engine.rs's
                          `idle_prompt_does_not_postpone_or_clear_staleness`)
  other/unrecognised     → heartbeat only, lastSeen = now (fail-open for
                          undocumented types — agent_completed,
                          agent_needs_input, anything invented later — never
                          guessed at; see "Waiting for Review" below)
Stop / StopFailure /
PostCompact (main agent):
  subagentCount > 0 &&
    state == NEEDS_YOU    → awaitingSubagents = true,        lastSeen = now
                            (heartbeat only — the block still wins)
  subagentCount > 0        → awaitingSubagents = true, state = WORKING
  else                     → state = terminalState(session)  // READY or
                            WAITING_REVIEW, per review_when_done
SubagentStart           → subagentCount++,                lastSeen = now (no state change)
SubagentStop            → subagentCount--,                lastSeen = now (no state change);
                          if count hit 0 && awaitingSubagents && state != NEEDS_YOU:
                            state = terminalState(session)  // releases the deferred transition
set_review_flag(id, bool) → Session.review_when_done = bool (no state change;
                            only affects the NEXT terminal transition)
SessionEnd              → remove session (clears review_when_done, awaitingSubagents)
(no event for staleTimeout) → mark stale, subagentCount = 0 → drop after grace
```

**Main agent vs. subagent (`agent_id`):** a subagent (Task tool) shares its
parent's `session_id`, so its events would otherwise mutate the parent row. Every
hook carries `agent_id` **only when emitted by a subagent** (null/absent for the
main agent). Rule: **only the main agent moves a session into WORKING/READY;
subagent events are heartbeat-only** (keep it live + drive `subagentCount`, never
recolor the row). The exception is a *block*: a subagent hitting a permission gate
still needs the user, so `Notification(permission_prompt|elicitation_dialog)` —
and likewise `PreToolUse(AskUserQuestion|ExitPlanMode)`, which halts on you the
instant it fires — escalates to NEEDS_YOU regardless of `agent_id`. Clearing that
red is main-agent-only, though: a subagent's `PostToolUse` for a blocking tool
must not resume a genuinely-blocked parent. This is why a session can be
RED/NEEDS_YOU with subagents running and not get cleared by their activity. The
row's color tracks the main agent; the "N agents running" sub-line tracks
subagents — fully independent. See `engine.rs` (`is_subagent`, `heartbeat`).

> **This decision is narrowed, not reversed, by "Waiting for Review" above:**
> a `SubagentStop` is now *allowed* to move the row's color — but only to
> **release a transition the main agent's own `Stop` already earned and
> deferred** (`awaiting_subagents`), never to invent one. It still can never
> *clear* a `NeedsYou` (the guard is explicit in both the `Stop` and
> `SubagentStop` arms), and it can never move a session into WORKING/READY on
> its own initiative the way a genuine subagent-activity event (`SubagentStart`,
> a subagent's `PostToolUse`) still cannot. The rule remains: only the main
> agent originates a color change; a subagent can only release one already
> owed.

> ⚠️ **Verify before building (Phase 1):** confirm exact event names, the
> `Notification` payload's type field, and that `http` + `async` hooks are
> supported on the installed Claude Code version (`claude --help hooks`). Some
> events are version-gated. If `http` hooks are unavailable, fall back to a
> `command` hook that forwards stdin to the listener (e.g. a tiny bundled
> `curl`/forwarder). Do not assume the schema — read it.

> ✅ **Verified (Claude Code 2.1.195):** All seven event names are valid.
> `type: "http"` hooks are natively supported (Claude POSTs the stdin JSON to
> the URL) — **no command-hook fallback needed**. The `Notification` payload
> carries a `notification_type` field; `permission_prompt` and
> `elicitation_dialog` mean NEEDS_YOU. `idle_prompt` is **ignored** (it fires on
> mere idleness, not a real block — an idle session stays Ready/green until the
> stale sweep greys it), as are `auth_success` / `elicitation_complete`. Every
> event includes `session_id`, `cwd`,
> `transcript_path`, `hook_event_name`. HTTP hooks are non-blocking by nature
> (a non-2xx/timeout is a non-blocking error), and Session Signals' listener answers
> instantly, so an explicit `async` flag is unnecessary for `http` hooks. The
> installed block uses an empty matcher (`""`) per event. See `hooks.rs`.

> ✅ **Verified (Claude Code 2.1.x) — `agent_id` distinguishes subagents:**
> empirically captured raw hook bodies (spawned an Explore subagent against the
> live listener). On a **single `session_id`**, the main agent's events carry
> `agent_id: null`, while the subagent's `PreToolUse` / `PostToolUse` /
> `PostToolBatch` / `SubagentStart` / `SubagentStop` all carry a non-null
> `agent_id` plus `agent_type` (e.g. `"Explore"`). This is the signal Session Signals uses
> to stop subagent activity from overwriting the parent's traffic-light state (the
> `NEEDS_YOU`-masking bug). `HookEvent` now parses `agent_id`/`agent_type`.

## Session presentation

- **Label:** `basename(cwd)` + git branch if resolvable. Resolve branch by
  reading `<cwd>/.git/HEAD` (no subprocess); fall back to none.
- **Row:** status dot • label • state text • time-in-state.
- **Expiry:** removed on `SessionEnd`; otherwise marked stale after
  `staleTimeoutMin` (default 10) of silence, then dropped after a short grace.
- **Fork/resume duplicates:** a session launched with `--fork-session --resume
  <parent>.jsonl` (e.g. computer-use automation) can emit hook events under
  *both* the new and the parent `session_id`, so Session Signals may briefly show a
  duplicate "twin" row for the parent. This is a byproduct of forking, not a
  bug: the hook payload carries no fork/parent linkage, and detecting it would
  require process inspection (a locked-out decision), so we don't suppress it
  while active. Once the fork stops emitting, the parent greys out via the
  normal stale sweep. Ordinary terminal sessions don't fork and never duplicate.

## Defaults (all user-overridable in settings)

- Listener port: `4317`.
- Stale timeout: `10` min.
- Notifications: Red → OS notification **on**, sound **off**. Orange/Green
  silent. Waiting for Review → **on**, sound **off** (matches Red). Fire on
  **state transitions only**, never while idle.
- Widget: remembers position; expanded by default; opacity adjustable.
- Launch on login: off by default.

## Suggested project structure

```
session-signals/
├─ src/                 # React UI (widget, settings, tray menu views)
│  ├─ widget/
│  ├─ settings/
│  ├─ state/            # session store, derivation client-side mirror
│  └─ themes/           # data-driven theme definitions
├─ src-tauri/
│  ├─ src/
│  │  ├─ listener.rs    # 127.0.0.1 HTTP server, parses hook payloads
│  │  ├─ engine.rs      # session state map, rollup, stale sweep
│  │  ├─ tray.rs        # tray icon + menu
│  │  ├─ hooks.rs       # settings.json install/uninstall (non-destructive)
│  │  └─ windows.rs     # floating widget + settings windows
│  └─ tauri.conf.json
└─ docs/SPEC.md
```

## Conventions

- TypeScript strict. No `any`. Functional React, hooks-based.
- State flows one way: Rust engine is source of truth → emits events to the
  webview → React renders. UI never derives state independently.
- Keep Rust surface minimal and well-commented; prefer doing logic in the engine
  so the UI stays a thin renderer.
- No browser storage APIs. Persist via `tauri-plugin-store` (JSON in app config
  dir): settings, window position, theme.
- Versioning: SemVer, releases-only. `package.json` is the single source of
  truth; bump via `npm run release:{patch,minor,major}`. Never hand-edit the
  version in `tauri.conf.json` / `Cargo.toml`. See `docs/VERSIONING.md`.

## Guardrails

- Bind the listener to `127.0.0.1` only. Reject non-loopback. Optional shared
  token header is a later hardening, not v1.
- Hooks must be `async: true` so they never slow Claude Code down. Keep the
  endpoint's response immediate.
- The hook installer must **merge** into existing `~/.claude/settings.json`,
  never overwrite the user's other hooks. Always offer a copy-paste fallback and
  a clean uninstall.
- Local only. If you ever find yourself adding a network call out, stop.

## Build phases

The app was built in four ordered phases, each ending runnable and demoable.
All four are **complete**; this is the historical roadmap, kept for context:

1. **Foundation** — hooks → listener → engine → tray rollup.
2. **Widget** — floating widget + per-session breakdown.
3. **Notifications** — configurable notifications + settings.
4. **Themes** — data-driven themes + packaging/polish.

(The original per-phase `/goal` build prompts are no longer tracked in the
published tree; any internal scratch lives under `docs/internal/`, which is
gitignored.)

## Parallel session conventions

This repo is frequently worked on by multiple concurrent Claude Code sessions that could be
running in separate git worktrees. Follow these rules.

### Branch & commit hygiene
- Always work on a dedicated task branch, never directly on `main`.
- Pull/rebase from `main` before starting edits.
- Commit in small, logical units, and commit often — don't leave a large
  uncommitted working set.
- Keep each change scoped to its task. Do not opportunistically refactor or
  reformat unrelated files.

### No repo-wide sweeps
- Do not run project-wide formatters, `lint --fix` across the whole tree,
  codemods, or mass find-and-replace without flagging it first. These create
  merge conflicts across every other active session.
- If a broad change is genuinely needed, stop and ask before running it.

### Runtime state is not isolated by the worktree
- Do not assume exclusive use of the default database, ports, or services.
- Use this worktree's own `.env`/config. If you need a database, use an
  isolated schema or container — never run destructive migrations against a
  shared default DB.

### Scope ownership (only when sharing a single checkout, not worktrees)
- If you've been assigned a specific area, edit only files under that path:
  <!-- e.g. this session owns src/api/ only -->
- Treat files outside your assigned scope as read-only.