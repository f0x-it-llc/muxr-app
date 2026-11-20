# Vendored proto

The files under `proto/muxr/` are a byte-for-byte copy of `muxrd/proto/muxr/` in
`f0x-it-llc/muxr-core` at rev `a01004de434105534dba2cc7c7f4a51467edda4d` (branch `main`).
They are the single definition of the wire this app speaks, so nothing else in the tree
restates a field number, enum value or RPC name.

Refresh: delete `proto/muxr/`, re-copy the whole directory from a newer MERGED muxr-core
`main` rev, update the rev above, and never hand-patch a file.

Check: `diff -r proto/muxr <muxr-core>/muxrd/proto/muxr`

The contract rules are documented upstream in `muxrd/proto/README.md`.
