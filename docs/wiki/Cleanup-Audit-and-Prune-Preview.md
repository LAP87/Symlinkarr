# Cleanup, Audit, and Prune Preview

Use this page for `Cleanup`, cleanup result views, and `Prune Preview`.

## Core Principle

Cleanup is review-first.

Symlinkarr should show you what it found, explain why it wants to act, and only then let you apply destructive changes.

## The Flow

1. Run cleanup audit.
2. Review the generated report.
3. Open prune preview.
4. Check blockers, legacy groups, and detailed findings.
5. Apply only if the preview still looks correct.

## What the Pages Mean

- `Cleanup`: starting point, scope, and safety reminders
- cleanup result pages: whether a report was generated or a background audit started
- `Prune Preview`: exact candidate rows, blockers, trust gates, and final apply confirmation

## Common Reasons a Row Is Blocked

- no trustworthy tracked anchor exists yet
- ownership or confidence checks are not satisfied
- legacy anime root duplication still needs human review

Blocked rows are not bugs by default. They are often intentional safety rails.

## When to Expand the Details Table

Only go deep when:

- grouped reasons are not enough
- you need the exact symlink path
- you are about to apply cleanup and want final confirmation

## Related Pages

- dead-link recovery before cleanup: [Repair and Dead Links](Repair-and-Dead-Links.md)
- anime-specific legacy cleanup: [Anime Remediation](Anime-Remediation.md)
