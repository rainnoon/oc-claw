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
