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

# Code Style

- Prefer simple, minimal fixes.
- Do not over-engineer solutions.
- Do not suggest unnecessary refactors unless explicitly asked.
