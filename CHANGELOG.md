# Changelog

## v0.3.0 — 2026-06-16

Adds cross-source contract test asserting driver/README/udev-rule all agree on the device mode octal; fails loudly when 0640 vs 0660 skew is present.

## v0.2.0 — 2026-05-29

Add `memlog-witness` — a long-running userspace daemon that subscribes to
`/dev/memlog`, demultiplexes records by session-id, and persists each
session's snapshots to `~/.claude/memlog/<session-id>/snap-NNNN.json`.
Includes atomic snap writes (tmp→fsync→rename), per-session quota trimming,
single-instance flock guard, `status` subcommand, and `drain` subcommand.
The `libmemlog` crate gains `persistence.rs` (atomic SessionWriter + quota
trim) and `lock.rs` (WitnessLock). Fixture-replay integration tests cover
AC1–AC5 at compile time; AC6–AC8 are boot-gated on `/dev/memlog`.
