# oc-claw Terminal Focus

A companion extension for [oc-claw](https://github.com/anthropics/oc-claw) desktop pet. It enables precise window and terminal focusing when you click a Cursor session in oc-claw.

## What it does

This extension runs a lightweight HTTP server on `127.0.0.1` (ports 23456–23460) inside each Cursor window. It exposes two things:

- **Window metadata** — workspace name, workspace roots, focus state, and a native window handle so oc-claw can figure out which Cursor window belongs to which coding session.
- **Focus endpoint** — oc-claw sends a request to bring the active terminal or editor to the foreground in the correct window.

## Why it's needed

Cursor's hook system doesn't expose stable terminal or window identifiers. Without this extension, oc-claw can only do `tell application "Cursor" to activate`, which picks an arbitrary window. With it, oc-claw matches each session to the right window by workspace path and jumps there precisely — even when you have multiple Cursor windows open.

## Installation

You don't need to install this manually. oc-claw automatically syncs the extension to `~/.cursor/extensions/oc-claw.terminal-focus-1.0.0/` on every startup. Just reload your Cursor window if it's not active yet.

## Privacy

- The HTTP server only listens on `127.0.0.1` (localhost). No data leaves your machine.
- No telemetry, no analytics, no network requests.
