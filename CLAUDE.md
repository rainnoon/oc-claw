# Project Context

This is the oc-claw project — a Tauri desktop pet application with an agent monitoring platform.

IMPORTANT: oc-claw/ is the project root and has its own git repository. Do NOT operate on any parent directory or sibling directories.

- Frontend: TypeScript + React (Vite)
- Desktop shell: Rust (Tauri)
- Agent platform backend: Go

Before making any changes, confirm you are working in the correct project (oc-claw), not ooclaw, gif-maker, or any other sibling directory.

# Bug Fixing

When fixing a bug:
1. State which exact file(s) you will modify and what each change does before touching anything.
2. After making the fix, re-read every file you changed.
3. Check that the fix does not break any existing functionality.
4. Verify all imports are still valid.
5. Run the type checker (`tsc --noEmit`) if TypeScript files were modified, or `cargo check` if Rust files were modified.
6. Report any issues found before committing.

Do NOT introduce fixes for edge cases the user has not mentioned. Implement exactly what is requested first.

# UI Components

When the user references a UI element or component by name (e.g. "the little character", "the settings panel", "the mini window"):
- Do NOT guess which file or component to edit.
- List the candidate files/components that match the description.
- Ask the user to confirm the correct target before making any changes.

# OpenClaw Data Format

When modifying anything related to OpenClaw session activity detection, health polling, or JSONL parsing:

1. **Do NOT assume data formats.** Always check real data first at `~/.openclaw/agents/*/sessions/*.jsonl`.
2. OpenClaw JSONL is NOT standard Claude API format. Key differences:
   - `role` has three values: `"user"`, `"assistant"`, `"toolResult"` (NOT `"tool_use"`)
   - Tool calls use content type `"toolCall"` (NOT `"tool_use"`)
   - `stop_reason` does NOT exist in the JSONL
   - `usage` is present on every completed API call, including intermediate tool calls
3. A single turn has a tool loop: `user → assistant(toolCall) → toolResult → assistant(toolCall) → toolResult → ... → assistant(text)`. The queue goes idle between each step, but the turn is NOT over until the final `assistant` message with only `text` content (no `toolCall`).
4. `check_agent_active_from_lines()` in `lib.rs` is the single source of truth for session activity. All health, preview, and animation states depend on it. Modify with extreme care.
5. **Sub-agent sessions**: OpenClaw sub-agent session keys contain `:subagent:` (e.g. `agent:main:subagent:uuid`). Use this to identify and filter them — do NOT rely on message content like `[Subagent Context]` which requires preview data that may not be loaded yet. Sub-agent sessions should be hidden from the UI session list and should NOT trigger completion sounds.
6. **OpenClaw source code** is installed at `~/Library/pnpm/global/5/.pnpm/openclaw@*/node_modules/openclaw/`. Check `dist/session-key-*.js` for session key format, `dist/health-*.js` for health endpoint logic. Always verify assumptions against real data at `~/.openclaw/` and the source code.

# Polling & Remote SSH

Mini.tsx has multiple polling loops (`fetchAgents` 5s, `pollHealth` 1s, `fetchAllSessions` 5s, Claude Code 2s). When modifying polling logic:

1. **Never use request-ID / stale-discard patterns on polling functions.** Remote SSH calls routinely take > 5s. If a stale check discards any call that's been superseded by a newer one, and calls consistently take longer than the interval, *every* call gets discarded and state never updates. The `settingsModeRef` guard (pause polling during settings) is sufficient to prevent old-config results from overwriting new-config results.
2. **Async polling functions need a busy lock** (e.g. `pollHealthBusyRef`). Without it, the 1s interval stacks SSH requests, overwhelming the multiplexed socket and causing cascading "stale socket" failures. If the previous call hasn't returned, skip the current tick.
3. **Both `exitSettings()` and `collapse()` must trigger `fetchAgents()` immediately.** Users can close settings via the back button (`exitSettings`) OR by clicking outside (`collapse`). If only `exitSettings` calls `fetchAgents`, clicking outside causes a 5-8s delay before config changes are detected.
4. **`settingsModeRef.current` must be set to `false` BEFORE calling `fetchAgents()`**, otherwise the settings-mode guard inside `fetchAgents` will skip the call.

# App Updates & Releases

When modifying the macOS update flow, keep these rules in mind:

1. **The app no longer checks GitHub Releases directly.** `check_for_update()` in `frontend/src-tauri/src/lib.rs` reads a website-managed manifest instead. In dev it uses `http://[::1]:4321/update/latest.json`; in production it uses `https://www.oc-claw.ai/update/latest.json`.
2. **The website manifest is the release gate.** `website/public/update/latest.json` controls which version the app sees and which DMG it downloads. Do not assume the latest GitHub Release should automatically be offered to users.
3. **The website download button is a separate lever.** `website/src/components/Hero.astro` can intentionally stay on an older DMG while `latest.json` points somewhere else during staged rollouts or updater testing. Do not silently “sync” them unless the user explicitly wants that.
4. **Dev update checks must bypass system proxies.** On macOS, localhost requests can be forwarded through the system proxy and return `502`. The dev update client in Rust must keep using `no_proxy()` for local manifest checks.
5. **The UI can only show progress during the download phase.** The app downloads the DMG first, emits `update-progress` events, then spawns a detached helper script, exits, and lets the helper replace `/Applications/oc-claw.app` and relaunch. After the app exits there is no in-app UI, so that behavior is expected.
6. **The helper installer is the source of truth for the swap flow.** It mounts the downloaded DMG, finds the `.app` bundle inside, copies it into `/Applications`, clears extended attributes, and relaunches the app. If update installation breaks, inspect the helper flow before changing the UI.
7. **Helper logs live in the system temp directory.** The updater writes files under `oc-claw-update-*` in the temp dir, including `install.log`. When debugging install failures, look there first.
8. **Every real user-facing macOS release still needs signing and notarization.** The helper-based installer does not replace Apple signing/notarization requirements. `xattr -cr` is only cleanup, not a substitute for notarization.
9. **Version bumps and public rollout are intentionally decoupled.** The app source version can move to the next release (for example `1.5.2`) while the website manifest and download button stay on the previous public build (for example `1.5.1`) until rollout is ready.

# Comments

Write thorough comments for any non-trivial logic, especially:
- Data format assumptions (JSONL structure, field names, role values)
- State transition logic (active/inactive, working/idle)
- Why a particular approach was chosen over alternatives
- Known pitfalls and edge cases

This prevents re-introducing bugs when context from previous conversations is lost.

# Code Style

- Prefer simple, minimal fixes.
- Do not over-engineer solutions.
- Do not suggest unnecessary refactors unless explicitly asked.
