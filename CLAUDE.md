# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Babbage is a local, offline file and content search app: a Tauri 2 desktop app (Rust backend + React 19 frontend), plus a CLI/MCP server that reads the same on-disk index. `docs/ARCHITECTURE.md` is the authoritative design doc; `docs/AGENTS.md` is the contract for the MCP/CLI surface; `docs/CONFIG.md` documents every setting.

## Commands

There are two separate Cargo workspaces (`src-tauri/` and `cli/`) and no root `Cargo.toml`, so cargo commands need `--manifest-path` (or `cd`). The Makefile wraps the common ones but assumes bash; on Windows run the underlying commands directly.

```bash
npm install                      # JS deps (package-lock.json; Makefile prefers bun if present)
npm run tauri dev                # desktop app in dev mode (starts vite on :1420 itself)
npm run dev                      # frontend only in a browser, backed by mock data (see below)
npm run tauri build              # release installers
cargo build --release --manifest-path cli/Cargo.toml   # babbage-cli + babbage-daemon
```

Checks (CI runs all of these; all must pass):

```bash
cargo test --all-targets --manifest-path src-tauri/Cargo.toml
cargo clippy --all-targets --manifest-path src-tauri/Cargo.toml -- -D warnings
npx tsc --noEmit
npm run test                     # vitest run
npm run build                    # tsc && vite build
```

Single tests:

```bash
cargo test --manifest-path src-tauri/Cargo.toml <name_substring>          # unit tests (inline #[cfg(test)] modules)
cargo test --manifest-path src-tauri/Cargo.toml --test golden_set         # retrieval-quality golden query set
npx vitest run src/components/HomeView.test.tsx
npx vitest run -t "<test name>"
```

Benchmarks live in `src-tauri/examples/` (`bench`, `index_bench`, `crawl_bench`, `index_format_bench`, `mem_probe`, `facet_bench`):

```bash
cargo run --release --manifest-path src-tauri/Cargo.toml --example bench -- <dir>
```

Resetting local app data (index/config under `%APPDATA%\babbage`, `~/.config/babbage`, or `~/Library/Application Support/babbage`; `BABBAGE_DATA_DIR` overrides): `.\reset.ps1 [-IndexOnly] [-DryRun]` on Windows, `make reset` / `make reset-index` elsewhere. These delete the user's real index — don't run them unprompted.

## Architecture

### Crates and processes

- `src-tauri/` — package `babbage`, library `babbage_lib`. **All** indexing, extraction, search, OCR, embedding, and agent (MCP/HTTP) logic lives here. `lib.rs` wires the Tauri app and registers commands; `commands.rs` is the Tauri command layer the frontend calls.
- `cli/` — thin binaries over `babbage_lib` (path dependency): `babbage-cli` (`search`, `state`, `mcp`, `serve`, `connect`), `babbage-daemon`, and optional `babbage-quick` (feature `quick-ui`). Their logic is in `src-tauri/src/cli.rs`, `daemon.rs`, and `agent/`.
- `src/` — React UI. Presentation only: the webview never owns indexing, extraction, or model state. That boundary is what lets the CLI and MCP reuse the index.

Process model: on Unix the GUI and CLI/MCP prefer a `babbage-daemon` (single writer, JSON-RPC over a Unix socket, token + protocol-version checked). On **Windows `DaemonClient::ensure()` returns false**, so the GUI hosts the `IndexerManager` in-process and is the only writer; CLI/MCP open the index read-only.

### Search path

`input → lexer → AST → router → planner → tiers → RRF → rank → snippet` (`query/`, `search/`).

Four tiers, fused with reciprocal rank fusion in `search/mod.rs`:
1. Fuzzy filename (`search/fuzzy.rs`) — nucleo over an in-memory arena (`indexer/arena.rs`) with a per-name character bloom prefilter.
2. BM25 content (`indexer/content.rs`) — Tantivy, schema v4, custom `babbage` tokenizer (`indexer/tokenizer.rs`) with CJK unigram+bigram handling.
3. Semantic (`embed/`) — static embedding table lookup (no ONNX at query time); units carry `model_id` so vector spaces never mix.
4. Folded/trigram fallback — look-alike normalization for OCR misreads, verified against `content_raw`, always ranked after exact hits.

`query/intent.rs` turns plain-language phrases into filters with deterministic rules (no model). Explicit grammar always wins; unknown `key:` is literal text; parse errors yield a hint, never zero results.

### Indexing

SQLite (`indexer/metadata_db.rs`, WAL) is durable truth; the arena is rebuilt from it at startup. File identity is `(device, inode)` / Windows `file_id`, not path — renames must not trigger re-extraction. Walker (`jwalk`) → extractors (`indexer/extract/`, one pass feeding both content and semantic indexes) → watcher (`notify`, 400 ms settle). Sensitive paths and credential extensions are default-denied in the walker and again in the agent surface.

### OCR and Cargo features

- `accurate-ocr` (default) compiles PP-OCRv6 via `paddleocr_rs_onnx` + `ort` (`load-dynamic`: needs `libonnxruntime` at runtime, falls back to Tesseract if absent). `ocr_indra/` is a second ONNX engine.
- `smart-embeddings` is an opt-in ONNX embedding model.
- `paddleocr_rs_onnx` is **vendored and patched** (`src-tauri/vendor/`) to expose a shared ONNX Runtime environment. The `[patch.crates-io]` entry must exist in both `src-tauri/Cargo.toml` and `cli/Cargo.toml`.
- The visual (CLIP) tier is not live: `active_encoder()` is `None` in non-test builds, so `similar_images` self-hides.

### Frontend

- `src/main.tsx` loads `src/dev/mockTauri.ts` when not running inside Tauri, so `npm run dev` in a plain browser shows every screen with example data (`#setup=1`, `#q=invoice`, `#empty=1`). New Tauri commands used by the UI need a mock there to keep the browser preview working.
- `src/App.tsx` holds most app state; components are in `src/components/`, Settings tabs in `src/components/settings/`.
- i18n: `src/locales/{en,zh,ru}/<namespace>.json`, statically imported in `src/i18n/config.ts`. User-facing strings go in all three locales; the query grammar stays English.
- Styling: Tailwind 4 with design tokens defined in `src/App.css` — use tokens, no raw colors. UI is deliberately quiet: hairlines, no shadows, light + dark themes, no emoji icons.
- Vitest tests are colocated (`*.test.ts(x)`) and opt into jsdom per file with `// @vitest-environment jsdom`.

## Project conventions (from docs/CONTRIBUTING.md)

- Rust 2024. Run `rustfmt` on new files only — the repo is not globally formatted, so don't reformat unrelated files.
- No `#[allow(...)]` without a comment explaining why; no TypeScript `any` without a comment.
- Tests are hermetic (temp dirs, no network, no user data). Expected test shape per area: parser → assert on the AST, not strings; config → migration/save test with the disk-free helpers; extraction → malformed input that must not panic or hang; ranking → fixture where the expected file lands in the top 3; MCP/CLI → request-handler test with a temp index; SQLite → `open_at(temp)`.
- Query-path or indexer changes should come with before/after numbers from the `bench` example.
- Product non-goals to preserve: no cloud/accounts/telemetry, no bundled LLM or in-app chat, and no writes through the agent API other than the opt-in `reindex` tool.
- Version is duplicated in `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json` (the CLI crate is versioned separately); see `docs/RELEASING.md`.
