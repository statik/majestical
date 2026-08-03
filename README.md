# Majestical

Majestical is an agent-first media catalog. `maj` is the CLI over a local,
CRDT-backed catalog of ingested media (video/audio/image assets), their
folksonomy tags, PARA organization, verification history, and search index.
All output is JSON-first so both humans and agents can drive it.

## Quickstart

```bash
maj --catalog /path/to/catalog --machine-id studio-1 catalog init
maj --catalog /path/to/catalog --machine-id studio-1 ingest --source /path/to/media
maj --catalog /path/to/catalog --machine-id studio-1 search "sunset beach"
```

`--catalog` and `--machine-id` can also be set via the `MAJ_CATALOG` and
`MAJ_MACHINE_ID` environment variables (`MAJ_AUTHOR` optionally overrides the
author identity recorded on emitted events; it defaults to the machine id).

## Agent access (MCP)

`maj mcp` serves the catalog to MCP clients over stdio. Point a client at it
with:

```json
{ "mcpServers": { "majestical": {
  "command": "maj", "args": ["mcp"],
  "env": { "MAJ_CATALOG": "/path/to/catalog", "MAJ_MACHINE_ID": "studio-1" }
} } }
```

The server exposes 26 tools mirroring the CLI's verbs: 10 read-only tools
(search, get_asset, list tags/saved-searches/volumes/sync-locations, etc.)
plus 16 mutating tools covering tagging, PARA moves, metadata, scanning,
verification, ingest, sync, and describer configuration. Mutating tools
default to a dry-run preview; pass `confirm: true` to execute. Two
resources — `majestical://` thumbnails and keyframe manifests — let a
client fetch imagery for an asset. Every read result carries the asset's
stable id so an agent can chain calls (look up an asset, then tag it, then
verify it) without re-resolving paths.
