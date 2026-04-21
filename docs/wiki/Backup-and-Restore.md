# Backup and Restore

Use this page for `Backup`, `restore`, `bootstrap`, and no-config recovery.

## What a Backup Covers

Symlinkarr backups can capture:

- tracked link state
- SQLite state
- the active config file
- config-local `secretfile:` secrets that Symlinkarr can see

Environment-only secrets still live outside the backup set.

## Main Recovery Paths

- existing install, normal rollback: use `Backup`
- fresh install from backup: use `symlinkarr restore <backup>`
- no config present: use auto-restore or `bootstrap`

## Important Safety Rule

Restore is destructive.

That means it can overwrite current tracked state with the chosen snapshot. It is a recovery tool, not a casual sync operation.

## When to Use `bootstrap`

Use `bootstrap` when:

- this is a fresh install
- you do not already have a backup to restore
- you want starter directories and a guided `config.yaml`

## When Auto-Restore Helps

If `config.yaml` is missing and a suitable backup exists in the configured/default backup location, Symlinkarr can restore automatically on startup.

That is meant to shorten recovery on containerized or rebuilt installs.

## Related Pages

- validate after restore or bootstrap: [Configuration and Doctor](Configuration-and-Doctor.md)
- daily operational context: [Dashboard and Daily Operations](Dashboard-and-Daily-Operations.md)
