# Discover and Queue

Use this page for `Discover`, discover results, and the persistent auto-acquire queue.

## Discover

Discover is read-only.

It previews where RD-backed source content could land under tagged library roots without writing links.

Use it when:

- you want placement visibility before a normal scan is enough
- you are checking library targeting assumptions
- you want a safer way to inspect content that is not yet linked

## Queue

Queue is where deferred or persistent acquisition work lives after scans.

Typical states:

- queued
- downloading
- relinking
- blocked
- no-result
- failed
- completed but still unlinked

## When These Pages Matter Most

- scan runs are surfacing acquisition pressure
- background work is stuck or repeatedly blocked
- you want to understand what Symlinkarr would do before it creates anything

## Related Pages

- scan context and reason codes: [Scan, History, and Why-Not Signals](Scan-History-and-Why-Not-Signals.md)
- dashboard triage: [Dashboard and Daily Operations](Dashboard-and-Daily-Operations.md)
