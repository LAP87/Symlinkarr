# Configuration and Doctor

Use this page for `Configuration`, `Validate Config`, and `Doctor`.

## Why These Pages Exist

They answer two different questions:

- `Configuration`: what is Symlinkarr configured to do right now?
- `Doctor`: is the runtime actually safe enough to do it?

## Configuration Page

Use it to inspect:

- library roots
- source roots
- matching defaults
- database and backup locations

Validation is the fast way to catch config mistakes before a risky run.

## Doctor Page

Use it before:

- large scans
- repair work
- cleanup and prune work
- environment or mount changes

Doctor is intentionally blunt. Failures here should block risky mutation until the cause is understood.

## Common Failure Classes

- schema/version issues
- bad or missing runtime paths
- unwritable backup or data locations
- config/runtime mismatch

## Related Pages

- backup and first-run recovery: [Backup and Restore](Backup-and-Restore.md)
- media refresh context: [Media Server Refresh and Deferred Work](Media-Server-Refresh-and-Deferred-Work.md)
