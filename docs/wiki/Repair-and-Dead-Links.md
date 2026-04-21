# Repair and Dead Links

Use this page for `Dead Links`, `Links`, and repair-oriented follow-up.

## What Counts as a Dead Link

A dead link is a tracked symlink whose source path no longer exists where Symlinkarr expects it.

This usually means:

- source content moved
- source content disappeared
- the old path is stale after provider or cache churn

## Safe Order of Operations

1. Repair first.
2. Cleanup later.

That order matters because repair tries to preserve the library shape. Cleanup is what you do when no trustworthy replacement exists.

## Main Pages

- `Dead Links`: broken-path triage and repair-first action surface
- `Links`: the tracked link inventory
- repair result surfaces: quick feedback after a repair run

## When to Repair

Repair is the right first move when:

- the target path is still supposed to exist in the library
- the content likely still exists somewhere in the source universe
- you want to avoid churn in downstream media-server libraries

## When to Stop Repairing and Move to Cleanup

Move to cleanup when:

- the content is genuinely gone
- the old path is legacy junk
- repair cannot find a safe replacement
- duplicate or trust-blocked rows need a review-first cleanup pass

## Related Pages

- full cleanup review: [Cleanup, Audit, and Prune Preview](Cleanup-Audit-and-Prune-Preview.md)
- scan context before or after repairs: [Scan, History, and Why-Not Signals](Scan-History-and-Why-Not-Signals.md)
