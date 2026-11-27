# Vendored proto

The files under `proto/muxr/` are a byte-for-byte copy of `muxrd/proto/muxr/` in
`f0x-it-llc/muxr-core` at rev `c6a399967e4b2490204181babcf06b7e7ea72296` (branch `main`).
They are the single definition of the wire this app speaks, so nothing else in the tree
restates a field number, enum value or RPC name.

Refresh: delete `proto/muxr/`, re-copy the whole directory from a newer MERGED muxr-core
`main` rev, update the rev above, and never hand-patch a file.

Check: `diff -r proto/muxr <muxr-core>/muxrd/proto/muxr`

The contract rules are documented upstream in `muxrd/proto/README.md`.
