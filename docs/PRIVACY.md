# Privacy

Babbage is local-first, private, and offline.

## What stays on your machine

- The index (metadata, full-text, embeddings) lives in your OS config directory (`%APPDATA%\babbage` on Windows).
- No account, no sign-in, no cloud search.
- No telemetry and no analytics: search queries, file names, and usage data are never sent anywhere.
- **Update checking is completely disabled.** No periodic pings or checks to any server.
- Model downloads fetch local inference weights from Hugging Face or GitHub releases only when required or enabled. Packs are verified against pinned SHA-256 checksums before loading.

## What leaves the machine

- Nothing about your files or searches.
- No telemetry or update checks.
- Purely model/OCR downloads from the model host when enabling local OCR or embeddings.

## MCP server

- The MCP server is read-only by default. It can read the local index and file metadata/contents that Babbage has already indexed.
- It cannot delete or modify files.
- `agent_scoped_roots` can restrict MCP/CLI results to specific roots.
- The MCP server does not expose arbitrary filesystem access beyond the index.
