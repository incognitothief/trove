# Recall

> **DISCLAIMER — MACHINE OF ORIGIN:** These Recall notes were written on the
> **desktop computer**. They are not from a laptop, a remote host, or any other
> machine. Treat host-specific paths, disk layout, `~/.trove`, and local
> hardware (for example `/Volumes/T72`) as **this desktop's** context unless a
> later note says otherwise.

Episodic memory of Trove working sessions.

ADRs (`docs/adr/`) are the source of truth for **decisions**. Runbooks
(`docs/runbook/`) are **how to operate** the CLI. This folder captures **what
happened in a session**: how far we got, what failed, and what the next action
was. It is not a status checklist and it is not architecture.

## How to use

- **Write only when the user asks.** Agents must not auto-record a session,
  recap, or next-action note. A Recall file is created or updated only on an
  explicit prompt.
- Add one note per prompted recap when the work is worth remembering across
  chats.
- Prefer dated filenames: `YYYY-MM-DD-short-slug.md`.
- Write what a future session needs: context, outcome, next action. Point at
  ADRs and runbooks instead of duplicating them.
- Do not rewrite old notes to match later reality. Add a new note.

## Notes

| Note | Session |
| --- | --- |
| [2026-08-15-bootstrap-through-first-backfill.md](2026-08-15-bootstrap-through-first-backfill.md) | Recap of July 7–9 chats: genesis through failed first archive backfill |
