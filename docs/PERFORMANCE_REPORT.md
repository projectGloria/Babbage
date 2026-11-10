# Babbage Performance Analysis

2026-11-10

## Executive summary

Babbage's core filename search is fast, but the app has one defect that can hang search during indexing, and most of its remaining cost comes from work that grows with index size on every keystroke, every launch, or every file change. The analysis found 40 issues: 2 critical, 12 high, 16 medium and 10 low. Nothing was changed in the code.

**The one to look at first.** Search and the crawl run on the same thread pool. While a crawl is extracting, a search waits for a crawl thread to become free; the crawl, told to yield to the search, puts its threads to sleep until the search finishes. In a standalone model of this pattern the search never completed. With semantic search on, which is the default, every text query during a crawl takes this path. This was reproduced in a model of the code, not observed in the running app, so it should be confirmed there.

**What was measured on this machine (i7-11800H, 16 threads, Windows 11):**

| Measurement | Result |
| --- | --- |
| Filename scan, 100k entries, default 16 threads | 7.6 ms |
| Same scan, 4 threads | 0.5 ms |
| Fixed cost of a full search on a small index | 5-6 ms, against 0.06 ms for the filename tier itself |
| Rebuilding the entry list after any index change | 30 ms at 100k entries, about 310 ms at 1M |
| Directory walk as implemented, 8,943 entries | 330-650 ms |
| Same walk reusing enumeration data, single thread | 21-23 ms |
| Parallel query while a crawl-shaped job holds the pool | 13.4 s, against 0.1 ms idle |

**The main themes:**

1. **Shared thread pool.** Search, snippets, facets, the semantic scan and the crawl all use rayon's global pool. The default pool size is also the slowest setting for the filename scan.
2. **Per-keystroke work that scales with the index.** Regex compilation in the intent pass, two corpus-sized hash tables for the semantic tier, full-text decompression for every content hit, a heavy fallback tier on narrow queries, and a full re-materialisation of the file list whenever the index has changed.
3. **Per-launch work that scales with the index.** The whole index is loaded on the main thread before the window appears, then every file is stat'ed again with a call that costs about 15 times more than necessary on Windows.
4. **Quadratic paths under the write lock.** Removing or renaming a directory scans the whole index once per affected file while search is blocked.
5. **Windows memory trimming.** The process empties its own working set every five seconds during extraction, which lowers reported memory at the cost of page faults.
6. **Crash exposure.** The crawl holds one SQLite transaction from start to finish while the content index commits every 512 documents, so an interrupted crawl repeats all extraction.

**What was not measured.** End-to-end latency in the running app, content-index query time on a real corpus, OCR throughput, crawl wall time, process memory and window-summon time. Findings in those areas come from reading the code and are labelled as such.

## Scope and method

This is a read-only analysis of Babbage 1.0.16 at commit `911f202` plus the uncommitted working tree. No source file was changed.

**Read in full or in their non-test part:** `search/mod.rs`, `search/fuzzy.rs`, `indexer/mod.rs`, `indexer/arena.rs`, `indexer/metadata.rs`, `indexer/metadata_db.rs` (schema, load, upsert, remove, rename, units), `indexer/content.rs`, `indexer/walker.rs`, `indexer/extract/mod.rs`, `lib.rs`, `commands.rs` (search, snippets, stats, home, thumbnails, preview), `embed/scanner.rs`, `embed/units.rs`, `embed/model.rs`, `embed/table.rs` (load, embed), `embed/queue.rs`, `util/memory.rs`, `ocr.rs` and `ocr_indra/mod.rs` (engine pooling), `src/App.tsx` (search, snippets, paging, status), `VirtualGrid.tsx`, `lib/thumbnails.ts`, `hooks/useStats.ts`, both `Cargo.toml` files, `vite.config.ts`, `tauri.conf.json`.

**Read by outline and targeted excerpts:** `indexer/watcher.rs`, `indexer/extract/image.rs`, `preview.rs`, `doctor.rs`, `agent/mod.rs`, `agent/mcp.rs`, `cli.rs`, `models.rs`, the vendored `paddleocr_rs_onnx` session setup.

**Measured on this machine** (Intel i7-11800H, 8 cores / 16 threads, 15.7 GB RAM, Windows 11, Defender real-time protection on):

- the project's own `bench`, `mem_probe` and `facet_bench` harnesses, release build;
- two small standalone programs written outside the repository: one reproduces the crawl/search thread-pool pattern with the same rayon version (1.12.0), one compares directory-walk strategies with the same jwalk version (0.9.0);
- sizes of the existing release build, installer and `dist/` bundle.

**How to read the findings.** Each finding carries a confidence label. *Measured* means a number from this machine. *Reproduced* means the mechanism was reproduced in a standalone model of the code, not inside the running app. *From code* means the cost follows directly from the code path but was not timed. Scale estimates ("at 1M files") are extrapolations from the code, and are labelled as such.

## Measurements

All numbers are from this machine (i7-11800H, 16 logical cores, Windows 11, Defender on), release builds, taken on 2026-11-10. The synthetic probes use no content index and no semantic units, so they measure the metadata and filename path only.

### Filename scan versus thread count (`mem_probe`, `BABBAGE_SEARCH_BENCH=1`)

The default 16-thread pool is the slowest configuration at 100k entries: about 15 times slower than 4 threads and 7 times slower than a single thread on a no-match query.

| Entries | Rayon threads | Fuzzy p50, no-match query `zzzz` | Fuzzy p50, `f0000001` | Full `search_response_fast` p50 |
| --- | --- | --- | --- | --- |
| 15,000 (sequential path) | default | 0.056 ms | 0.45 ms | 5-6 ms |
| 100,000 | 1 | 1.06 ms | 2.9 ms | 9 ms |
| 100,000 | 2 | 1.05 ms | 2.1 ms | 6 ms |
| 100,000 | 4 | 0.51 ms | 1.2 ms | 6 ms |
| 100,000 | 8 | 1.19 ms | 2.0 ms | 6-7 ms |
| 100,000 | 16 (default) | 7.6 ms | 8.2 ms | 9-11 ms |
| 1,000,000 | 1 | 15.1 ms | 36.9 ms | 42 ms |
| 1,000,000 | 4 | 4.8 ms | 10.4 ms | 16 ms |
| 1,000,000 | 16 (default) | 7.0 ms | 9.7 ms | 16-20 ms |

Two things stand out. At 16 threads, 100k and 1M entries cost about the same (7-8 ms), so the time is pool overhead, not scanning. And the full search has a floor of about 5 ms even when the fuzzy tier takes 0.06 ms; see *Query path, Q13*.

### Other harness results

| Measurement | Result |
| --- | --- |
| Materialise all entries from the arena (what a stale `entries_snapshot` costs) | 4 ms at 15k, 30 ms at 100k, 303-317 ms at 1M |
| Path lookup in the arena | p50 0.5-0.6 µs, p95 0.6-0.8 µs |
| `compute_facets`, 100k matched results | p50 3.7 ms, p95 4.1 ms |
| `compute_facets`, 1M matched results | p50 24.3 ms, p95 30.3 ms |
| `bench` on this project folder (14,817 entries): walk, first run | 2,782 ms (5,326 files/s, 188 µs per file) |
| Same, second run | 1,837 ms (8,066 files/s, 124 µs per file) |
| `bench` fuzzy, 14,817 entries, 8 queries | average 0.61 ms, worst 1.48 ms |
| `FileEntry` / arena `Node` size | 128 bytes / 64 bytes |

The probes print zero for RSS on Windows, so no memory figures were obtained from them.

### Standalone experiments

| Experiment | Result |
| --- | --- |
| Parallel query from outside the pool, idle pool | p50 0.10 ms |
| Same query while a crawl-shaped job occupies the pool (5 ms per item) | 13.4 s |
| Same, with the `query_in_flight` yield loop | did not finish |
| Walk of 8,943 entries: jwalk names only | 7.4 ms |
| Walk: std `read_dir` + `DirEntry::metadata`, one thread | 21-23 ms |
| Walk: jwalk + `entry.metadata()` (Babbage's approach) | 330-650 ms |

### Build outputs already on disk

| Artifact | Size |
| --- | --- |
| `babbage.exe` (release, stripped) | 39.6 MB |
| NSIS installer | 71.5 MB |
| Bundled resources: f32 embedding table | 30.2 MB |
| Bundled resources: Indra OCR pack | 26.8 MB |
| Bundled resources: PP-OCR pack | 6.3 MB |
| Frontend JS, one chunk | 587 KB (175 KB gzip) |
| Frontend CSS | 62 KB (14 KB gzip) |
| `Babbage.png` in `dist/`, present twice | 2.9 MB each |
| `setup-icon-paper.png` and `setup-icon-void.png` | 551 KB each, byte-identical |

### Not measured

End-to-end latency inside the running desktop app, Tantivy query time on a real index, OCR throughput, crawl wall time, process memory, and window-summon time were not measured. Doing so needs the real app running against a real index, which this analysis did not start.

## Query path

The filename tier itself is fast; most per-keystroke cost sits around it, in work that scales with corpus size rather than with result count. Everything below runs inside `SearchDispatcher::search_response_with_snippets` (`src-tauri/src/search/mod.rs:788`), called from the `search` command (`src-tauri/src/commands.rs:167`).

### How a keystroke executes

1. The frontend debounces 40 ms, then invokes `search` (`src/App.tsx:295`).
2. `search` is an `async fn` that runs the whole CPU-bound query inline on a Tauri async-runtime worker. It is not moved to a blocking thread, unlike `get_snippets` and `get_thumbnail`.
3. It sets `query_in_flight = true`, takes read locks on metadata, content index, unit store, config and visual store, and holds all five for the full query.
4. Intent rules, parse, route, candidate selection, then the tiers, fusion, sort, grouping, facets, truncate.

### Q1. Tiers run on the global rayon pool, which the crawl also occupies

Three query stages call `par_iter` on rayon's global pool from a non-pool thread: the fuzzy scan at 20,000+ candidates (`search/fuzzy.rs:116`), facets at 20,000+ results (`search/mod.rs:283`), and the semantic scan for any non-empty unit store (`embed/scanner.rs:56`). `get_snippets` does the same with no size threshold (`commands.rs:261`). The crawl runs its extraction loop on that same global pool (`indexer/mod.rs:899`). Rayon serves externally injected jobs last, after local and stolen work, so a query stage waits until a crawl worker runs out of work. See *Indexing pipeline, I1* for the measurement and the interaction with `query_in_flight`.

### Q2. Semantic tier builds two corpus-sized hash tables per keystroke

When semantic search is on (the default, `config.rs:459`) and any units exist, every text query builds `node_to_idx: HashMap<u64, usize>` over all candidates, then copies its keys into `allowed_node_ids: HashSet<u64>` (`search/mod.rs:1053-1071`). For an unfiltered query that is every entry in the index: two inserts per file, per keystroke. The comment above it says the map "only serves the semantic/visual tiers" and is skipped for filename queries, but `wants_semantic` is true for any query with text. The scanner then probes that set once per unit (`scanner.rs:66`). When candidates are the whole corpus, `CandidateSet` already has an `all` shortcut; the semantic path has no equivalent. *From code.* Cost grows linearly with index size.

### Q3. The semantic scan materialises every unit above 0.05 cosine

`scan_with_model` collects `(idx, sim)` for every unit with similarity above 0.05 into a `Vec`, then groups by node in a `HashMap`, then selects top-k (`scanner.rs:56-118`). Static-embedding cosines against a short query are mostly above 0.05, so the intermediate vector is close to the size of the unit store. The dot product is a scalar indexed loop over `i8` with an `i32` accumulator (`model.rs:365`), which LLVM may or may not vectorise well; it was not checked in the generated code. With the default cap of 48 units per document, a 100k-document corpus can hold several hundred thousand units, each 256 bytes. *From code.*

### Q4. Every content hit loads its full stored text

`run_query` calls `searcher.doc()` for each of up to `limit × 3` hits to read `path`, `filename` and `modified` (`content.rs:1083-1120`). The same stored document also carries `content_raw`, the full extracted text, up to the 4 MB cap (`content.rs:415`, `:643`). Tantivy's doc store decompresses the whole document, so reading a path costs decompressing the file's text. With the default `max_results = 50` that is up to 150 documents per keystroke. `modified` is already a fast field and is still read from the doc store. *From code.* This is likely the dominant cost of the BM25 tier on document-heavy indexes.

### Q5. The folded fallback runs on most narrow queries and is heavier than the tier it backs up

The folded tier runs whenever fused candidates number fewer than `limit` (`search/mod.rs:1408`). A specific query that matches a handful of files therefore always pays for it. It requests `limit × 3`, and `search_folded_filtered` doubles that again for its collector, so up to 300 stored documents are loaded with raw text (`content.rs:958`). Each candidate is then verified by `verify_folded`, which lowercases the entire raw text, splits it into words, and calls `fold()` on each word twice, allocating a `Vec<char>` and a `String` per call (`content.rs:89-131`). On a 1 MB document that is on the order of 150,000 words, per candidate, per term. *From code.*

### Q6. Scope filtering uses a regex over the path term dictionary

A single `in:` scope becomes `RegexQuery("(?s)^<escaped prefix>.*")` on the `path` field (`content.rs:1019-1023`). That walks the term dictionary of every indexed path with an automaton, on every keystroke of a scoped query. The same prefix is then re-checked in Rust on each hit. *From code.*

### Q7. Per-query allocations proportional to the corpus

| Where | What is built per query | When |
| --- | --- | --- |
| `search/mod.rs:950` | `candidate_indices: Vec<usize>` of `0..N` (8 bytes per file) | every unfiltered query |
| `search/mod.rs:848` | `HashMap<PathBuf, usize>` cloning every path | any query containing `in:` or starting with a path-like token |
| `search/mod.rs:1263` | `HashMap<PathBuf, MatchCandidate>` with one cloned `PathBuf` per candidate | filter-only queries (`type:image`, `ext:pdf`), i.e. browsing |
| `search/mod.rs:1461` | one `SearchResult` with two `String`s per candidate, then a full sort, burst grouping, version grouping and facets over all of them | filter-only queries, before truncating to 50 |
| `metadata.rs:731` | the full `Vec<FileEntry>` snapshot (path rebuild, `PathBuf` join, name copy, extension `String` per file) | first query after any metadata mutation; see Q8 |

A filter-only query on a large index is the worst case: it converts the whole matched set into owned result rows, runs four regexes per row in `group_versions` (`search/mod.rs:336`), and only then keeps 50.

### Q8. The entries snapshot is invalidated by every single mutation

`entries_snapshot()` is cached per `generation` (`metadata.rs:731`). `add`, `remove`, `set_content_pending`, `clear_content_pending` and the `mark_*` functions each bump `generation` (`metadata.rs:288`, `:561`, `:470`, `:490`). During a crawl or a burst of watcher events the cache is therefore stale on nearly every query, and each query re-materialises all N entries. `materialize` rebuilds the path by walking the parent chain, `PathBuf::join`s each component (an allocation per level), copies the name, allocates the extension as a `String` and takes the global `intern_extension` mutex (`metadata.rs:788`, `arena.rs:127`, `:171`). The arena exists to avoid exactly this; the query path undoes it whenever the index is changing. *From code.*

### Q9. Fuzzy scoring details

- The bloom prefilter tests the file *name*, while nucleo then scores the full *path* first and the name second (`fuzzy.rs:56-65`). Scoring the path is the more expensive call and its haystack is several times longer. `path.to_string_lossy()` is free on valid Unicode; the UTF-32 conversion into `char_buf` is not.
- For every entry nucleo accepts, the literal check allocates a lowercased copy of the path and a second copy from `replace` (`fuzzy.rs:68`).
- A query containing `/` or `\` disables the prefilter entirely (`fuzzy.rs:108`, `:56`), so path-style queries score every entry.
- The project's own numbers show the default 16-thread pool is 5 to 6 times slower than 4 threads for this scan on Windows (`docs/BENCHMARKS.md`, 17-19 ms versus 3.1 ms at 100k). The scan still uses the default pool. Measurements on this machine are in *Measurements*.

### Q10. Ranking and grouping

- `finalize_score` allocates a lowercased name per candidate and counts path components (`search/mod.rs:1604`, `:1626`).
- `metadata.get_usage` takes the usage `RwLock` and hashes a `PathBuf` per candidate (`search/mod.rs:1494`).
- `group_bursts` resolves each result path back through the path index, then takes the `id_index` *write* lock inside `get_index_by_node_id` for every image result (`metadata.rs:830`, `:868`).
- `group_versions` clones stem, parent and extension strings and runs four regex replacements per result (`search/mod.rs:320-354`).

These are bounded by `limit × 3 × tiers` for text queries and are small. They become significant only in the filter-only case in Q7.

### Q11. No cancellation, and paging repeats the whole query

`CURRENT_SEARCH_SEQ` is stored but never read to abandon a superseded query (`commands.rs:176`). Each debounced keystroke runs to completion; the frontend discards stale responses by sequence number. `loadMore` passes an `offset`, and the backend recomputes every tier, fuses, sorts and computes facets again before dropping the first `offset` rows (`search/mod.rs:1531`, `src/App.tsx:483`). Because text tiers only fetch `limit × 3` hits each, paging also cannot reach past roughly the first few hundred fused results.

### Q12. Snippets

`snippet_for_path` reads up to 64 KB per plain-text file, lossily decodes it, and lowercases the whole buffer to find the first term (`content.rs:1333-1340`, `:1206`). Up to 20 files per request, in parallel on the global rayon pool. The `SnippetCache` on `ContentIndex` is no longer used by this path. Non-text formats return a snippet only if the document is among the 16 most recently extracted (`extract/mod.rs:254`), so PDF and image hits usually show none.

### Q13. About 30 regexes are compiled on every query

`intent::compile_with_aliases` runs first on every query (`search/mod.rs:820`). It calls `Regex::new` inline for each rule: sort words, time windows, size phrases, type words, filler words, and one more per alias term (`query/intent.rs:81`, `:123`, `:151`, `:174`, `:190`, `:215`, `:230`, `:295`, `:309`, `:318`, `:371`). The file contains 32 case-insensitive patterns and none is cached; `group_versions` in the same crate shows the `OnceLock` pattern that avoids this. The probe measures a full-search floor of 5-6 ms on a 15,000-entry store where the fuzzy tier itself takes 0.06 ms. Regex compilation is the only stage in that path whose cost does not depend on corpus size, so it is the likely source of the floor. The floor is *measured*; the attribution is *from code*. Queries that contain an explicit `key:` token skip most of the rules.

## Indexing pipeline

The crawl is correct and streaming, but it shares a thread pool with search, repeats a full stat of every file on every launch, and has three operations whose cost is quadratic in index size. The crawl lives in `IndexerManager::start_indexing` (`src-tauri/src/indexer/mod.rs:580`).

### I1. Search work queues behind the crawl, and `query_in_flight` can turn the wait into a permanent stall

This is the most serious finding in the report.

**The setup.** The crawl calls `rayon::scope` on the global pool, spawns `to_extract.par_iter().for_each_with(...)` into it, and consumes results on the scope thread (`indexer/mod.rs:899-1105`). Every global-pool worker ends up inside a long sequential run of extraction closures. One worker is parked in the consumer loop for the whole crawl. The search path uses the same global pool (see *Query path, Q1*). Rayon's worker loop takes local work first, then steals from other workers, and only then looks at jobs injected from outside the pool, so a query's parallel stage waits for a crawl worker to run dry.

**The amplifier.** At the top of each extraction closure, `yield_to_queries` sleeps in a loop while `query_in_flight` is set (`indexer/mod.rs:540-549`, `:907`). The search command sets that flag before it starts and clears it after it returns (`commands.rs:192`, `:224`). So: the query sets the flag, every crawl worker finishes its current file and then sleeps waiting for the flag to clear, the query's parallel stage is waiting for one of those workers, and neither side can proceed. The query never returns, so the flag is never cleared. The stalled query also holds five read locks, so the next writer blocks, and with a waiting writer `parking_lot` blocks new readers as well.

**Evidence.** A standalone program with the same structure (rayon 1.12.0, global pool, `scope` + `spawn` + `par_iter().for_each_with(tx)` with a bounded channel consumer, a query thread calling `into_par_iter` from outside the pool) gave, on this machine:

| Scenario | Query latency |
| --- | --- |
| Idle pool | p50 0.10 ms, max 0.40 ms |
| Crawl running, 5 ms per file, no yield flag | 13.4 s (the query finished only when the 12.5 s crawl ended) |
| Crawl running, 200 ms per file (OCR-like), no yield flag | not finished after 15 s |
| Crawl running, 5 ms per file, with the yield flag held during the query | never finished; the crawl would otherwise have ended at about 13 s |

*Reproduced* in a model, not observed in the running app. The conditions in the app are: a crawl with files left to extract, and a query that reaches a global-pool stage. With semantic search on (the default) and at least one stored unit, every text query qualifies, because the semantic scan has no size threshold. With semantic off, it needs 20,000 or more candidates or results. `get_snippets` does not set the flag, so it cannot deadlock, but it waits for a crawl worker in the same way.

**Related defects in the same mechanism:**

- `query_in_flight` is a boolean shared by all queries. With two overlapping searches, the first to finish clears it while the second is still running.
- It is not cleared by a guard. If the search path panics, the flag stays set and `yield_to_queries` loops forever. The comment at `indexer/mod.rs:538` says a stuck flag "costs a slower crawl, never a hung one"; the loop has no timeout, so it is a hung one.
- `wait_while_paused` and `pace_on_battery` also sleep inside global-pool workers (`indexer/mod.rs:501`, `:554`), so a paused crawl keeps every global worker asleep and starves any other user of the pool in the same way.
- Blocking extraction is dispatched from a global worker to a dedicated extraction pool and awaited with `recv_timeout` (`extract/mod.rs:527-576`). Two pools' worth of threads are tied up per file: one working, one waiting.

### I2. Every launch walks and stats the whole tree, and the stat is the slow part

`start_indexing` runs at every launch (`lib.rs`, `manager.start_indexing()`), and the watcher is stopped for the duration (`indexer/mod.rs:605`). The walk calls `entry.metadata()` on every jwalk entry (`walker.rs`, main loop). On Windows that is a separate open-and-query per file, although the directory enumeration already returned size, timestamps and attributes.

Measured on this repository's `node_modules` (8,943 entries, warm cache, Defender on):

| Strategy | Time | Per entry |
| --- | --- | --- |
| jwalk, names only | 7.4 ms | 0.8 µs |
| `std::fs::read_dir` + `DirEntry::metadata`, single thread | 21-23 ms | 2.5 µs |
| jwalk + `entry.metadata()`, as Babbage does | 330-650 ms | 37-73 µs |

The per-file metadata call accounts for about 98% of walk time and makes the walk roughly 15 times slower than a single-threaded walk that reuses the enumeration data. This matches the project's own Windows figure of 9.3-10 s per 100k files, which `docs/BENCHMARKS.md` attributes to NTFS and antivirus. *Measured.*

The walk has further per-entry and per-directory overhead:

- `process_read_dir` builds `entry.path()` (a `PathBuf` allocation) and a lowercased name `String` for every child, then a second `std::fs::metadata` for every directory on Windows and macOS to detect cloud placeholders.
- `IgnoreSet::from_directories` probes every visited directory for `.gitignore` and `.cursorignore` after the walk. Ignored directories are still walked and stat'ed in full, then filtered out.
- All entries are collected into one `Vec<FileEntry>` before any processing (128 bytes plus two heap strings each), and `crawled_paths` then clones every path into a `HashSet<PathBuf>` (`indexer/mod.rs:713`, `:754`). At 1M entries this is a few hundred megabytes of transient memory.
- For each walked entry the incremental check calls `metadata.read().get(&path)`, which materialises a full `FileEntry` (path rebuild, name and extension allocations) only to compare `modified`, `size` and one flag (`indexer/mod.rs:760`).

### I3. Removing entries is quadratic and holds the metadata write lock

`MetadataStore::remove` swap-removes one node, then scans **every** node to repoint children of the moved slot (`metadata.rs:547-553`). It also calls `arena.path()` twice and rehashes two path-index entries. Callers remove in loops:

- the stale sweep after a crawl removes each deleted path under one write lock (`indexer/mod.rs:1255-1258`);
- the watcher's directory delete materialises all N entries to find descendants, then removes each (`watcher.rs:890-913`).

Deleting a directory of D files from an index of N costs about D × N node visits with search blocked. For D = 50,000 and N = 500,000 that is 2.5 × 10¹⁰ visits, tens of seconds. Each `remove` also issues two unprepared SQL statements in their own implicit transaction (`metadata_db.rs:361`). *From code.*

### I4. Directory renames are several full passes, and they do re-extract

`rename_prefix` makes four passes over all nodes, each calling `arena.path(idx)` (which allocates), then clears and rebuilds the entire path index (`metadata.rs:579-681`). The SQL side loads every path in the database into a `Vec<String>` and issues one unprepared `UPDATE` per moved row (`metadata_db.rs:382-440`). `handle_directory_rename` then deletes the old documents, re-walks the new directory, and calls `read_text_content` on every indexable file, plus a full-image decode for a perceptual hash on every image (`watcher.rs:642-745`). `docs/ARCHITECTURE.md` states that a rename "never re-extracts content"; for directories the code does. *From code.*

### I5. One SQLite transaction spans the whole crawl

`BEGIN` is issued before the walk and `COMMIT` after extraction (`indexer/mod.rs:719`, `:1172`), on the single shared connection. Consequences:

- Tantivy commits every 512 documents, SQLite once at the end. A crash or kill mid-crawl keeps the Tantivy segments and loses every metadata row, pending flag and semantic unit written so far, so the next launch redoes all extraction, including OCR.
- The WAL cannot be checkpointed past an open write transaction, so it grows for the length of the crawl. `journal_size_limit` only applies after a checkpoint.
- Every other user of the connection (dimension updates from extraction workers, `record_open`, stats queries) silently joins the crawl's transaction.

### I6. Writer and commit settings

- The Tantivy writer is one thread with a 16 MB heap (`content.rs:496`). The project's own numbers show the folded trigram field makes indexing 3.5 times slower than the baseline and that two and four writer threads cut 1M-document time from 17.6 s to 9.3 s and 5.3 s (`docs/BENCHMARKS.md`).
- `add_or_update` copies the document text three times before Tantivy sees it: `content.to_string()`, `fold(content)` (which first lowercases into a `String`, then collects a `Vec<char>` of the whole text, then builds the output), and `add_text(raw)` (`content.rs:634-645`). For a 4 MB document that is roughly 30 MB of short-lived allocation, on the single consumer thread.
- Each unit is inserted with its own unprepared statement, a per-unit `Vec<u8>` conversion, and a separate `unit_store.write()` lock acquisition (`indexer/mod.rs:1067-1078`, `metadata_db.rs:541`).
- Before extraction, `visual_store.write().remove_node(id)` is called once per re-extracted file, taking the write lock each time (`indexer/mod.rs:870`).

### I7. Status events are not rate-limited

`emit_status` fires every 5 extracted files, every 64 indexed documents, every 256 units, every 500 walked entries and at each commit (`indexer/mod.rs:963`, `:1011`, `:1080`, `:812`). Each emit takes the status write lock, formats a string, serialises the struct and posts it to the webview, where `App` and `IndexingBanner` each call `setState` (`src/App.tsx:251`, `IndexingBanner.tsx:99`). On a text-heavy crawl that extracts thousands of files per second this is hundreds of events per second, each re-rendering the 1,361-line `App` component. Extraction workers also contend on the status lock. *From code.*

### I8. Change handling on the watcher thread

`handle_file_upsert` extracts content, including OCR, synchronously on the single watcher thread (`watcher.rs:747-845`), so one slow document delays every queued event. The embedding queue later re-reads the same file through `read_text_content` (`embed/queue.rs:171`); the result is reused only if it is still among the 16 most recent extractions and under 256 KB. A burst of more than 16 changed images is therefore OCR'd twice. *From code.*

### I9. Smaller items

- `NameArena` is append-only. Every `add` of an existing path and every rename pushes the name again and never reclaims the old bytes (`metadata.rs:227`, `arena.rs:65`). Offsets are `u32`, so the arena is capped at 4 GiB. `docs/ARCHITECTURE.md` describes a delta buffer "merged at idle"; no such merge exists in `arena.rs`.
- `IdIndex::insert` and `remove` are `Vec::insert` / `Vec::remove` on sorted parallel vectors: O(n) memmove each (`arena.rs:439-454`). Cheap for monotonically increasing ids, expensive for removals.
- `arena.extension()` returns an owned `String` on every call (`arena.rs:127`); the `mark_*` passes call it for every node.
- `PathIndex::find` allocates a `Vec` of path components on every lookup, and `path_hash` runs over the lossy string of the path (`arena.rs:200-247`).
- `sniff_format` opens and reads the file header, and is called up to twice per document before the extractor opens the file again; images get a fourth open for dimensions and a thumbnail freshness check (`extract/mod.rs:503`, `:517`, `indexer/mod.rs:915-943`). On Windows every open passes through antivirus filters.
- Each extracted document under 256 KB is cloned into the 16-entry extraction cache, which the crawl itself never reads back (`extract/mod.rs:592-598`).

## Extraction and OCR

Extraction is bounded and panic-isolated, but on Windows it evicts the process's working set every five seconds, and image work outside the OCR path has no memory bound. None of the items in this section was timed.

### E1. `release_memory()` empties the working set on Windows

After every extracted document, and after every image in the perceptual-hash backfill, the code calls `release_memory()` (`extract/mod.rs:600`, `indexer/mod.rs:229`, `:267`). On Windows that is `EmptyWorkingSet(GetCurrentProcess())`, rate-limited to once per five seconds (`util/memory.rs:29-55`). `EmptyWorkingSet` removes every page of the process from its working set: the metadata arena, the entries snapshot, the embedding table, the unit vectors, Tantivy's mapped segments, the ONNX model weights. Each page is faulted back in on next touch. This lowers the number Task Manager shows, at the cost of a burst of page faults in the crawl and in the first search after each trim; under memory pressure those become disk reads. On Linux the equivalent call is `malloc_trim`, which only returns free heap. *From code.*

### E2. Image decodes outside OCR are unbudgeted

OCR decodes go through `decode_for_recognition`, which reserves the RGBA size against a process-wide decode budget (`extract/image.rs:777`). Thumbnails and perceptual hashes call `decode_guarded` directly and reserve nothing (`extract/image.rs:806`, `:879`). The frontend requests one thumbnail per visible image card, each on its own `spawn_blocking` thread (`commands.rs:690`, `:1720`; `src/lib/thumbnails.ts:47`). A grid of uncached photos therefore starts dozens of full-resolution decodes at once; a 12-megapixel image is about 48 MB decoded. The directory-rename path also runs a full decode per image for its hash (`watcher.rs:689`). *From code.*

### E3. Thumbnail freshness checks are heavy and repeated

`is_fresh` stats both files and parses the image header of both the thumbnail and the source on every call (`extract/image.rs:922-943`). It runs for every thumbnail request, including cached ones, and again per image during the crawl through `phash64_reusing_fresh_thumbnail`. The frontend keeps no thumbnail cache across mounts, and the grid is virtualised, so scrolling back remounts cards and repeats the request: four file operations, a file read, a base64 encode and an IPC string per card (`commands.rs:1720-1733`). The asset protocol is already configured for the `thumbs/` directory in `tauri.conf.json` but the code path returns data URLs instead.

### E4. OCR threading

- PP-OCR builds a recognition session pool of `cores / 2` sessions (8 on this machine) with the ONNX Runtime default thread settings; `configure_session_builder` sets no intra-op thread count on the CPU path (`vendor/paddleocr_rs_onnx/src/lib.rs:367-395`, `:570-590`). Up to 8 extraction workers can run sessions concurrently, each free to use every core. The detection model is a single session behind one mutex (`:594`), so detection is serialised across all workers. Whether this oversubscribes in practice was not measured.
- Indra loads `cores / 4` engines (clamped to 1-4), two threads each, and waiting workers poll with `try_lock` and a 10 ms sleep, up to 300 times (`ocr_indra/mod.rs:189-232`).
- Images are decoded at full resolution for OCR; no downscale step was found before recognition.

### E5. Extraction dispatch overhead

Every document, including plain text, goes through: `fs::metadata`, one or two `sniff_format` header reads, a cache lookup under a global mutex, a slot acquisition, a task spawn on the extraction pool, a channel round trip, a cache insert that clones the text, and `release_memory()` (`extract/mod.rs:475-602`). For small source files this fixed cost is likely larger than the read itself. The slot pool and the thread pool are both sized `clamp(cores, 2, 8)`.

### E6. Timeouts do not stop work

When `recv_timeout` expires, the caller moves on and marks the file for retry, but the task keeps running on the extraction pool and keeps its slot until it finishes (`extract/mod.rs:561-567`, `:536`). A document that hangs holds one of at most eight slots indefinitely. Other callers then wait up to 30 s for a slot before giving up on their file for this crawl (`extract/mod.rs:306`). Timeouts are 15 s for documents and 45 s for OCR candidates, growing with file size to 120 s.

### E7. Preview runs a full extraction

`get_file_preview` calls `extract_document(path, 128_000)` (`commands.rs:546-612`). Opening a preview for a PDF or image that is not in the 16-entry cache runs the PDF parser or OCR, competes with the crawl for extraction slots, and can wait up to 30 s for one.

### E8. Startup model work

The semantic table is loaded by reading the 30 MB f32 `model.safetensors`, then quantising it to i8 in memory, on first use in every process, including each CLI invocation (`embed/table.rs:72-150`). The quantised form (7.3 MiB) is never written to disk. `ensure_bundled_assets` copies about 63 MB of bundled models into the data directory on first run, on the main thread before the window exists (`models.rs:384-419`, `lib.rs`).

## Semantic tier

The semantic tier is an exact scan whose surrounding bookkeeping costs more than it should; the two query-time issues are described under *Query path, Q2 and Q3*. This section covers the store itself.

- **Storage layout.** `UnitStore` keeps a flat `Vec<i8>` of vectors (256 bytes per unit), a `Vec<Unit>` at 40 bytes per unit, and `node_to_units: HashMap<u64, Vec<usize>>`, which costs a separate heap allocation per document (`embed/units.rs`). The scan itself only reads `units[idx].node_id` and `model_id`, but pulls the 40-byte `Unit` into cache to do so.
- **Removal is a full rebuild.** `remove_units_for_nodes` rebuilds all three structures, copying every surviving vector (`embed/units.rs`). `remove_units_for_node` wraps a single id in a `HashSet` and calls it, so removing units one node at a time is O(total units) per call. The crawl batches this correctly; the watcher and embedding-queue paths were not checked.
- **Loading.** `load_units_into` allocates two `Vec`s per row, one for the blob and one for the `u8` to `i8` conversion (`metadata_db.rs:569-597`). The CLI and MCP path uses `load_all_units`, which additionally materialises every unit and vector in one `Vec` before copying them into the store (`agent/mod.rs:84`), roughly doubling peak memory at startup.
- **Units are always loaded.** Units are loaded at startup even when semantic search is off (`indexer/mod.rs:352-374`), by design, so the toggle is instant. The cost is 296 bytes per unit of resident memory for users who disabled the feature.
- **Embedding cost.** `pre_tokenize` allocates a `String` per word, and `tokenize_word` allocates a `Vec<char>` and a fresh `String` for every candidate piece in the greedy longest-match loop (`embed/table.rs:226-300`). Chunks are copied out of the document as owned `String`s before embedding (`embed/units.rs`, `chunk_text`). This runs for up to 48 chunks of 240 words per document, on the global rayon pool, nested inside the extraction closure (`indexer/mod.rs:951`).
- **Idle polling.** The embedding worker queries SQLite for pending nodes every 200 ms for as long as the app runs (`embed/queue.rs`, idle branch), and yields with a 50 ms sleep loop on `query_in_flight`.
- **Scale.** The architecture document says an ANN index is deliberately not built "below hundreds of thousands of units". With 48 units per document allowed, that threshold is reached at a few thousand long documents. Scan latency at that size was not measured here.

## Storage

SQLite is configured sensibly (WAL, `synchronous=NORMAL`, 256 MB mmap), but statements are re-prepared on every call, one index is redundant, and the Tantivy schema stores the full text in the same record as the fields search needs. None of this was timed.

### S1. SQLite statements are compiled on every call

Nearly all writes use `conn.execute(...)` or `conn.query_row(...)` with a SQL string, which parses and plans the statement each time. Only three call sites use `prepare_cached` (`metadata_db.rs:660`, `:729`, `:834`). The hot ones during a crawl are `upsert`, a 20-line `INSERT ... ON CONFLICT ... RETURNING` run once per new or changed file (`metadata_db.rs:325`), `insert_unit` once per semantic unit (`:541`), and `set_image_dimensions`, `set_image_text_sig`, `set_image_phash` and `assign_burst_for_path` per image (`:945-1037`). On a first scan of 1M files that is at least a million compilations of the upsert alone.

### S2. A redundant index on the largest column

`nodes.path` is declared `TEXT NOT NULL UNIQUE`, which creates an automatic unique index. The schema then adds `CREATE INDEX nodes_path ON nodes(path)` (`metadata_db.rs:69`, `:79`). Both index the same column; every insert, delete and path update maintains both, and the database stores every path three times (table plus two indexes). In total `nodes` carries eight indexes: the automatic one, `nodes_path`, `nodes_burst`, `nodes_phash`, `nodes_dev_ino`, two partial unembedded indexes and `nodes_burst_candidates` (`metadata_db.rs:130-190`).

### S3. One connection behind one mutex

A single `rusqlite::Connection` in a `parking_lot::Mutex` serves the crawl consumer, up to 16 extraction closures writing image dimensions, the perceptual-hash backfill, the embedding worker, the watcher, stats and the UI (`metadata_db.rs:16`). WAL's concurrent readers are not used inside the process. A slow statement such as `SUM(LENGTH(vec))` over all units, issued by the stats panel every two seconds (`metadata_db.rs:475`, `hooks/useStats.ts`), blocks every other database user for its duration.

### S4. Path-keyed lookups and unbatched deletes

- Image updates, pending flags, retries and removals all address rows by `path` text, not by the integer id the crawl already holds (`metadata_db.rs:361`, `:945`, `:963`, `:1312`, `:1330`).
- `remove` deletes units by subquery and then the node, although `units.node_id` already declares `ON DELETE CASCADE` with foreign keys on (`metadata_db.rs:361-371`, `:86`). The stale sweep and directory deletes call it once per path, each as its own implicit transaction, after the crawl transaction has committed.
- `rename_prefix` reads every path in the table into memory to find the ones under a prefix (`metadata_db.rs:400-410`).
- `usage` is keyed by path, so renames orphan open counts; `get_usage` hashes a full `PathBuf` per ranked candidate.

### S5. Maintenance at open

`open_at` runs `PRAGMA optimize`, about 13 `ALTER TABLE` attempts that are expected to fail with "duplicate column name", several `CREATE INDEX IF NOT EXISTS`, and `compact_if_bloated`, which can run `VACUUM` plus a truncating checkpoint (`metadata_db.rs:56-212`, `:224-260`). All of it runs on every open, on the GUI's main thread before the window exists, and again in every CLI or MCP process.

### S6. Tantivy schema and reader

- **Stored text sits with the identifying fields.** `content_raw` is stored in the same document as `path` and `filename` (`content.rs:395-420`), which is the cause of *Query path, Q4 and Q5*. The text is held three ways: the `content` postings with positions, the `content_folded` trigram postings, and the stored raw copy. The project measured index growth of 1.61 times from the stored copy and 9.7 times for CJK text.
- **`filename` is indexed with positions and stored**, while the identical value is also in SQLite and the arena.
- **Reader reload.** The reader uses `ReloadPolicy::OnCommitWithDelay`, which starts a background watcher on `meta.json`, and `commit()` also calls `reader.reload()` explicitly (`content.rs:501`, `:749`). In the GUI process the explicit reload makes the watcher redundant.
- **Commit cadence.** The crawl commits every 512 documents and the rename path every 1,000 (`indexer/mod.rs:1095`, `watcher.rs:729`). With a 16 MB writer heap and the default merge policy this produces many small segments and background merges during the crawl. The watcher commits after each settled batch or every 3 s.
- **Deletes.** `add_or_update` issues a `delete_term` for the path before every add, including on a first scan when nothing exists to delete (`content.rs:632`).
- **`num_docs()`** is documented in the code as including deleted-but-unmerged documents; it is what the stats panel shows.

## Startup, memory and background work

The window cannot appear until the whole index is loaded, the app is never fully idle, and hiding the window trades memory for a cold webview on every summon. Nothing in this section was timed.

### B1. Everything loads on the main thread before the window

`run()` does the following synchronously before `tauri::Builder` is constructed (`lib.rs`, `run`):

1. `ensure_bundled_assets` (first run: copies about 63 MB).
2. Pending rebuild/reset handling and stale-artifact cleanup.
3. Config load, OCR runtime probe.
4. `IndexerManager::new`: opens Tantivy, opens SQLite (see *Storage, S5*), then `load_all` reads every row into a `Vec<FileEntry>` and `load_from_db` builds the arena from it in two passes, then loads every semantic unit and visual unit (`indexer/mod.rs:295-383`, `metadata.rs:92-182`).
5. `start_indexing`, which spawns the crawl thread.

Startup time and peak memory therefore scale with index size. During the load the process holds both the full `Vec<FileEntry>` (128 bytes plus two heap strings per entry) and the arena being built from it. `compute_char_mask` is recomputed for every name on every load rather than stored.

### B2. A full crawl on every launch

Each launch re-walks and re-stats every watched root (see *Indexing pipeline, I2*), then sweeps the whole arena for stale entries by materialising every entry again (`indexer/mod.rs:1230-1244`). The watcher is not running until the crawl ends, so changes made during a long crawl are only picked up by the next one. There is no persisted change journal (for example the NTFS USN journal) to make a restart incremental.

### B3. Wake-ups while idle

| Source | Interval | What it does | Location |
| --- | --- | --- | --- |
| Window-request poll | 150 ms | tries to read `window.request` from disk | `lib.rs`, setup |
| Tray state poll | 250 ms | reads status and config locks, clones the theme `String` | `lib.rs`, setup |
| Embedding worker | 200 ms | SQL query for pending nodes | `embed/queue.rs` |
| Tantivy meta watcher | library default | polls `meta.json` | `content.rs:501` |
| Watcher loop | up to 500 ms | `recv_timeout` | `watcher.rs:312-320` |
| Updater | about 12 h | HTTPS request | `updater.rs` |

That is roughly 15 or more wake-ups per second with the window hidden and nothing to do, including about 6.7 file-open attempts per second for a file that almost never exists. `docs/BENCHMARKS.md` describes how to measure idle CPU but records no result.

### B4. Hiding the window

`hide` destroys the webview, terminates the WebView2 child processes and calls `release_memory()` (`main_window.rs:263-277`). Every summon then creates a new webview, starts new WebView2 processes, parses the 587 KB bundle, and issues `last_query`, `get_config`, `is_setup_complete`, `run_doctor` and `get_indexing_status` before the first search (`src/App.tsx:87-156`, `:244`). On Windows the working-set trim also means the index memory is paged back in on the first query. In launcher mode this cold path is the latency the user experiences most often. It is a deliberate memory trade-off; its cost has not been measured.

### B5. Memory held at rest

| Structure | Size | Notes |
| --- | --- | --- |
| Arena nodes | 64 bytes per entry | measured struct size |
| Name bytes | about 27 bytes per entry on the synthetic corpus | append-only, see *Indexing pipeline, I9* |
| Path index | 8 bytes per slot at up to 70% load | 2,097,152 slots at 1M entries |
| Entries snapshot | 128 bytes plus path and name heap strings per entry | held after the first query; a second copy exists briefly whenever it is rebuilt |
| Directory trie | built from a second full materialisation plus a `HashMap<PathBuf, usize>` of every entry | `metadata.rs:753-778`, rebuilt after any directory change |
| Semantic units | 296 bytes per unit plus the per-node `Vec` | loaded even with semantic off |
| Embedding table | 7.3 MiB (i8) after a 30 MB transient | when semantic is on |
| Usage map | one `PathBuf` key per opened file |  |

The entries snapshot is the largest item and is larger than the arena it is derived from. With it, the in-memory footprint per file is the arena cost plus roughly 250-350 bytes, not the 64-128 bytes the documentation quotes. No resident-set figure was measured.

## Tauri command layer and IPC

Several commands do corpus-sized work per call, and the heaviest one, `search`, runs on the async runtime rather than a blocking thread.

| Command | Cost per call | Location |
| --- | --- | --- |
| `search` | full query inline on an async worker; five read locks held throughout; no cancellation | `commands.rs:167-227` |
| `get_home_data` | materialises every entry, builds a `HomeItem` with two `String`s for every file, sorts all of them, keeps 12; inline on an async worker with the metadata read lock held. No call to it was found in the current frontend source, so this cost is latent | `commands.rs:1099-1180` |
| `get_stats` | loops over every node; walks the index directory on disk; two SQL aggregates including `SUM(LENGTH(vec))` over all units; polled every 2 s while the Stats tab is visible | `commands.rs:327-460`, `hooks/useStats.ts` |
| `run_doctor` | runs on every app mount and again in Settings; includes OCR status probes and directory listings; inline on an async worker | `commands.rs:1571`, `doctor.rs:1008`, `src/App.tsx:153` |
| `get_thumbnail_data` | freshness check (4 file operations), possible full decode, file read, base64, returned as a string through IPC | `commands.rs:1720` |
| `get_snippets` | up to 20 file reads of 64 KB on the global rayon pool | `commands.rs:247-276` |
| `get_file_preview` | full extraction, possibly OCR | `commands.rs:546` |
| `save_config` round trip | `setResultView` fetches the whole config and writes it back to change one field | `src/App.tsx:164-169` |
| `state_json` (daemon, CLI, MCP) | loops over every node and walks the index directory per call | `indexer/mod.rs:1723-1800` |

**Lock behaviour.** Search takes `metadata.read()` for the whole query. `parking_lot::RwLock` gives waiting writers priority, so a single queued `metadata.write()`, which the crawl issues once per changed file and the watcher once per event, blocks every new search until the in-flight ones finish and the writer runs. Long holders of the write lock are listed under *Indexing pipeline, I3 and I4*.

**Payload sizes.** A search response carries up to 50 results plus facets, including a `places` map with one key per distinct parent directory name in the matched set (`search/mod.rs:265`). For a broad or filter-only query on a large index that map can hold thousands of entries per response. `group_paths` carries every collapsed path of a group.

**Per-keystroke IPC.** `remember_query` is invoked on every change to the query, without debounce (`src/App.tsx:97`). It is a cheap synchronous command, but it is one extra IPC round trip per keystroke.

## Frontend

The React side is in reasonable shape: results are windowed, rows are memoised with a comparator that ignores callback identity, snippets load only for visible rows, and stats polling stops when the window is hidden. The issues are concentrated in status events, thumbnails and bundle weight. Nothing here was profiled in a browser.

### F1. Status events re-render the root component

`App` holds nearly all state in one 1,361-line component and subscribes to `indexing-status` with a plain `setIndexingStatus` (`src/App.tsx:251`). `IndexingBanner` subscribes separately and also fetches the status on mount (`IndexingBanner.tsx:89-112`). The backend emits without a time-based limit (see *Indexing pipeline, I7*), so during a fast crawl `App` re-renders at the event rate. Row components are protected by their memo comparator (`ResultItem.tsx`, end of file), but `App`'s own body, its `useMemo` dependencies, `VirtualGrid`'s row construction and every non-memoised child run each time.

### F2. Search effect

- Debounce is 40 ms (`src/App.tsx:347`). There is no backend cancellation, so on an index where a query takes longer than the typing interval, queries overlap and all run to completion.
- A sort change or the first keystroke after a pause each trigger a full backend query. `loadMore` re-runs the full query for every page (see *Query path, Q11*).
- Clearing facets on every keystroke creates two new `Set`s and forces `displayResults` to recompute (`src/App.tsx:264-268`, `:713`). The filter calls `classify(r)` for every loaded result on each recompute.

### F3. Snippet effect

The effect depends on `results`, so each snippet patch (which replaces the `results` array) tears down and recreates the `IntersectionObserver` and a `MutationObserver` with `subtree: true` (`src/App.tsx:355-465`). `observeRows` runs `querySelectorAll('[data-index]')` on every DOM mutation in the grid, including each scroll-driven remount. Inside the response handler, `results.find` is called per snippet, which is quadratic in page size but bounded at 20. This is small in absolute terms.

### F4. Thumbnails

`useThumbnail` issues `get_thumbnail_data` once per mount with no shared cache (`src/lib/thumbnails.ts:44-60`). The grid mounts the visible rows plus four overscan rows on each side (`VirtualGrid.tsx:16`), so a first render requests thumbnails for every image in roughly a dozen rows at once, and scrolling away and back repeats the requests. Each response is a base64 data URL held in component state and in the DOM, which is about 33% larger than the JPEG and is not shared between the grid card and the preview dialog. Backend cost is under *Extraction and OCR, E2 and E3*. `THUMBNAIL_LAZY_LIMIT = 30` is exported but the hook itself does not apply it.

### F5. Bundle and assets

| Item | Observation |
| --- | --- |
| JavaScript | one 587 KB chunk (175 KB gzip). No route or modal splitting: `SettingsModal` (1,789 lines), `SetupModal` (959 lines), `DuplicatesModal`, the Stats and Doctor panels and all three locales load on every window creation |
| Locales | English, Chinese and Russian are statically imported in `src/i18n/config.ts`; only one is used per session |
| `lucide-react` | imported by name per icon; tree-shaking applies, not checked per icon |
| `Babbage.png` | 1254 × 1254 RGB PNG, 2.9 MB, imported by `SetupModal.tsx:11` and also copied from `public/`, so `dist/` ships it twice |
| Setup icons | `setup-icon-paper.png` and `setup-icon-void.png` are byte-identical, 551 KB each |
| Fonts | three families (Inter variable, JetBrains Mono variable, Instrument Serif) across Latin, Latin-ext, Cyrillic, Greek and Vietnamese subsets; about 20 font files, loaded on demand by `unicode-range` |
| CSS | 62 KB (14 KB gzip) |
| Build config | `vite.config.ts` sets no `build` options: default target, no manual chunks, no asset inlining limits |

Because the launcher destroys and recreates the webview on every hide and show, the cost of parsing this bundle is paid on every summon, not once per session.

### F6. Other

- `React.StrictMode` wraps the app (`src/main.tsx`); this only doubles effects in development builds.
- `main.tsx` awaits a dynamic import of the mock layer only outside Tauri, so it adds nothing in the desktop app.
- `useMeasuredHeight` uses a `ResizeObserver` on the search bar; each height change re-renders `App` and shifts the grid inset.
- `VirtualGrid.onScroll` updates state only after half a row of movement, which keeps scroll re-renders low (`VirtualGrid.tsx:115-118`).
- `tauri.conf.json` sets `csp: null`. That is a security setting, not a performance one, and is noted only because it was in scope of the files read.

## CLI, daemon and MCP

On Windows there is no daemon, so every CLI call and every MCP server process loads its own full copy of the index. Nothing here was timed on this machine; the project's own figures are quoted where they exist.

- **Cold start per CLI call.** `HeadlessIndex::open` reads the config, opens SQLite (running the open-time maintenance in *Storage, S5*), loads every node into a `Vec<FileEntry>` and builds the arena, loads every semantic unit through the double-copy `load_all_units`, loads visual units, opens the Tantivy reader, and loads and quantises the 30 MB embedding table on first use (`agent/mod.rs:60-106`). `babbage-cli search` pays all of this for one query and then exits. Cost scales with index size.
- **A daemon probe precedes every local search.** `search_page` first attempts `daemon::request("search", ...)` and falls back to the local index on failure (`agent/mod.rs:116-135`). On Windows the request always fails, so each search pays a failed named-pipe connection first.
- **MCP sessions hold a snapshot.** A local MCP session opens the index once (`agent/mcp.rs:51`). No reload of the metadata arena or unit store was found in the request path, so a long-lived session serves the file list from its start time while the Tantivy reader follows new commits. That is a freshness issue with a performance side: results from the content tier that are not in the stale arena are dropped during fusion.
- **Duplicate memory.** Each MCP server process and the GUI hold separate copies of the arena, entries snapshot, unit vectors and embedding table.
- **Per-request state.** `index_state` loops over all nodes and walks the index directory on each call (`agent/mod.rs`, state JSON; `indexer/mod.rs:1723`).
- **Snippets** for agent results use `par_iter` on the global rayon pool (`agent/mod.rs:341`); in a headless process there is no crawl competing for it.
- **Project figures.** `docs/BENCHMARKS.md` records MCP search over stdio with a daemon at p50 37.4 ms on a one-file index, and the local read-only fallback at p50 5.9 ms on a 100-file fixture. Neither reflects a large index, and the first includes about 30 ms of transport overhead on an index where the search itself is negligible.

## Build configuration

The release profile is tuned for build time over runtime, and the installer carries about 63 MB of model files, half of which is a table the runtime immediately converts.

| Setting | Value | Effect |
| --- | --- | --- |
| `opt-level` | 3 | standard |
| `lto` | `"thin"` | cross-crate inlining is partial; the comment in `Cargo.toml` states this was chosen for 2-3× faster release builds |
| `codegen-units` | 16 | limits optimisation across units inside each crate, including `babbage_lib` itself, where the scan and fusion hot loops live |
| `panic` | unwind (default) | required, because extraction and OCR rely on `catch_unwind` |
| `strip` | true | smaller binary; no symbols for profiling a release build |
| Target CPU | default x86-64 baseline | no AVX2 for the i8 dot product in the semantic scan or for nucleo |
| Allocator | system | on Windows this is the process heap; the code works around its retention with `EmptyWorkingSet` (see *Extraction and OCR, E1*) |
| `crate-type` | `staticlib`, `cdylib`, `rlib` | the library is built in three forms; desktop only needs `rlib`. Affects build time, not runtime |
| Dev profile | dependencies at `opt-level = 2`, own code unoptimised | hot loops in `babbage_lib` run at debug speed under `tauri dev`, so interactive testing understates release performance |
| `cli/Cargo.toml` | same release profile, separate workspace and lockfile | the CLI compiles `babbage_lib` and all its dependencies a second time, including Tauri |

**Dependency weight.** `babbage_lib` is one crate containing Tauri, Tantivy, ONNX Runtime bindings, four tree-sitter grammars, `lopdf`, `image` with six codecs, `ureq` with TLS, and `rusqlite` with bundled SQLite. The CLI and daemon link all of it, including the GUI framework, because they depend on the same crate. Tree-sitter grammars are used only by the `find_symbol` agent tool. The release executable is 39.6 MB.

**Installer contents.** `tauri.conf.json` bundles `resources/**/*`: the f32 embedding table (30.2 MB), tokenizer (0.7 MB), the Indra OCR pack (26.8 MB, an engine that is not the default) and the PP-OCR pack (6.3 MB). The NSIS installer is 71.5 MB. On first launch these files are copied into the data directory, so they exist twice on disk. `Cargo.toml`'s comment says the OCR model pack "is downloaded only when the user opts in", but the packs are also bundled.

**ONNX Runtime** is loaded dynamically (`ort` with `load-dynamic`). If the library is missing, OCR silently falls back to Tesseract, which runs as a child process per image (about 0.11 s per image including process start, per the project's benchmark notes).

**Frontend build.** `npm run build` is `tsc && vite build` with default settings; see *Frontend, F5*.

## Ranked findings

Ordered by expected user impact. "Scales" means the cost grows with the number of indexed files. IDs refer to the sections above.

### Critical

| # | Finding | Where | Impact | Confidence |
| --- | --- | --- | --- | --- |
| 1 | Search stages and the crawl share rayon's global pool; with `query_in_flight` set, crawl workers sleep waiting for the query while the query waits for a crawl worker | I1, Q1 | Search can hang permanently during a crawl, holding read locks; at best it waits for a crawl worker to run dry (13.4 s in the model) | Reproduced in a standalone model |
| 2 | `query_in_flight` is an unguarded shared boolean | I1 | A panic in search stalls the crawl forever; overlapping queries clear each other's flag | From code |

### High

| # | Finding | Where | Impact | Confidence |
| --- | --- | --- | --- | --- |
| 3 | Fuzzy scan uses the default 16-thread pool | Q9, Measurements | 7.6 ms versus 0.5 ms at 100k entries on this machine; 100k costs the same as 1M | Measured |
| 4 | Per-file `metadata()` call in the walk | I2 | 98% of walk time; about 15× slower than reusing enumeration data; paid on every launch | Measured |
| 5 | About 30 regexes compiled per query in the intent pass | Q13 | 5-6 ms floor on every keystroke regardless of index size | Floor measured, cause from code |
| 6 | `remove` scans every node; called in loops under the write lock | I3 | Deleting a large folder blocks search for seconds to minutes. Scales quadratically | From code |
| 7 | Every content hit loads its full stored text to read the path | Q4, S6 | Up to 150 full-document decompressions per keystroke | From code |
| 8 | Folded fallback runs on most narrow queries and verifies up to 300 raw documents | Q5 | Likely the slowest tier for specific queries on text-heavy indexes | From code |
| 9 | Semantic tier builds two corpus-sized hash tables per query | Q2 | Two inserts per indexed file per keystroke with semantic on (the default). Scales | From code |
| 10 | Entries snapshot invalidated by every mutation | Q8 | 30 ms at 100k and about 310 ms at 1M added to the next query, repeatedly, while the index is changing | Rebuild cost measured |
| 11 | `EmptyWorkingSet` every 5 s during extraction on Windows | E1 | Page-fault bursts in crawl and search; trades latency for a lower Task Manager number | From code |
| 12 | Full crawl on every launch; watcher off during the crawl | B2, I2 | Disk and CPU load after each start, proportional to tree size | From code, walk rate measured |
| 13 | Index fully loaded on the main thread before the window exists | B1 | First-window delay and peak memory scale with index size | From code |
| 14 | One SQLite transaction for the whole crawl | I5 | A crash mid-crawl discards all metadata progress while Tantivy keeps its commits, forcing full re-extraction | From code |

### Medium

| # | Finding | Where | Impact | Confidence |
| --- | --- | --- | --- | --- |
| 15 | Filter-only queries materialise, sort and group the whole matched set before keeping 50 | Q7 | Browsing by type on a large index is the slowest query shape. Scales | From code |
| 16 | Directory rename: several full arena passes, full path-index rebuild, all paths loaded from SQL, and re-extraction of the moved tree | I4 | Seconds of write-lock time plus re-OCR for a rename | From code |
| 17 | SQL statements re-prepared on every call | S1 | Avoidable CPU on every upsert, unit insert and image update | From code |
| 18 | Duplicate index on `nodes.path`; eight indexes on `nodes` | S2 | Slower inserts and a larger database | From code |
| 19 | Unbudgeted full-resolution decodes for thumbnails and hashes, one thread per request | E2, F4 | Memory spikes of hundreds of MB to GB on a grid of large photos | From code |
| 20 | Status events not rate-limited; root component re-renders per event | I7, F1 | UI thread load during fast crawls; lock contention among workers | From code |
| 21 | Webview destroyed on hide and rebuilt on show | B4, F5 | Summon latency in launcher mode; bundle re-parsed each time | From code |
| 22 | Thumbnail freshness check and data-URL transport repeated on every mount | E3, F4 | Repeated file operations and IPC while scrolling | From code |
| 23 | Semantic scan collects nearly all units, then groups and selects | Q3 | Allocation proportional to unit count per query | From code |
| 24 | Walk collects all entries, then clones every path into a set; incremental check materialises each entry | I2 | Hundreds of MB transient at 1M files; avoidable allocations per file | From code |
| 25 | Text copied three times per document in `add_or_update`; single writer thread | I6 | Caps indexing throughput on text-heavy trees | From code; thread scaling from project benchmark |
| 26 | Timed-out extractions keep their slot | E6 | A few hung documents can stall extraction for 30 s per file | From code |
| 27 | `search` runs inline on the async runtime with no cancellation; paging re-runs the query | Q11, IPC | Overlapping full queries while typing on a slow index | From code |
| 28 | CLI and MCP load the whole index per process; MCP snapshot is never refreshed | CLI | Slow one-shot CLI calls on Windows; duplicated memory; stale results | From code |
| 29 | Scope filter as a regex over the path dictionary | Q6 | Per-keystroke cost proportional to indexed paths for `in:` queries | From code |
| 30 | Preview runs full extraction including OCR | E7 | Preview can take seconds and compete with the crawl | From code |

### Low

| # | Finding | Where | Impact | Confidence |
| --- | --- | --- | --- | --- |
| 31 | About 15 idle wake-ups per second | B3 | Background CPU and battery | From code |
| 32 | Release profile: thin LTO, 16 codegen units, baseline CPU target | Build | Unquantified loss in hot loops | Not measured |
| 33 | Embedding table read as 30 MB f32 and quantised at each process start | E8 | Startup time and transient memory per process | From code |
| 34 | 63 MB of models bundled and then copied; JS in one chunk; 2.9 MB PNG shipped twice | Build, F5 | Installer size, disk use, bundle parse time | Sizes measured |
| 35 | Append-only name arena; O(n) `IdIndex` edits; allocating `extension()` and `path_eq` | I9 | Slow memory growth and small per-operation costs | From code |
| 36 | Stats panel issues a full-table SQL aggregate and a directory walk every 2 s | IPC, S3 | Blocks other database users while the panel is open | From code |
| 37 | OCR session pool sized `cores / 2` with default ONNX threading | E4 | Possible oversubscription | Not measured |
| 38 | Open-time SQLite maintenance, including a possible `VACUUM`, on the main thread | S5 | Occasional long startup | From code |
| 39 | Changed files extracted twice (watcher, then embedding queue) beyond a 16-entry cache | I8 | Duplicate OCR for bursts of changed images | From code |
| 40 | Per-candidate allocations in ranking and grouping; write lock taken in `get_index_by_node_id` | Q10 | Small for text queries | From code |

### Documentation that does not match the code

These matter because they are the basis for the project's performance claims.

- `ARCHITECTURE.md` says a rename "never re-extracts content". Directory renames do (I4).
- `ARCHITECTURE.md` describes "a small delta buffer for watcher mutations, merged at idle". No such buffer exists in `arena.rs` (I9).
- `BENCHMARKS.md` attributes the slow Windows walk to NTFS and antivirus. The measured cost sits in the per-file metadata call, which a walk that reuses the enumeration data avoids (I2).
- `BENCHMARKS.md` describes the 16-thread slowdown as "a known scheduling cost". It is the default configuration that ships (Q9).
- The comment at `search/mod.rs:1045` says the node-id map is skipped for plain filename queries. It is built for every text query when semantic search is on (Q2).
- The comment at `indexer/mod.rs:538` says a stuck `query_in_flight` cannot hang the crawl. It can (I1).
- `get_thumbnail_data`'s comment says previews are 240 px; `ARCHITECTURE.md` says 320 px.
- `Cargo.toml` says the OCR pack is downloaded only on opt-in; both OCR packs are bundled in the installer.

## Not covered and open questions

**Not analysed in depth:**

- `indexer/extract/pdf.rs`, `pdf_image.rs`, `office.rs`, `delimited.rs`, `text.rs`, `external.rs`: only their entry points and limits were read. Per-format parsing cost, and how scanned PDF pages are rendered for OCR, were not examined.
- `ocr_indra/` internals (image operations, deskew, script routing) and the vendored PP-OCR recognition code beyond session setup.
- `indexer/tokenizer.rs` (CJK tokenizer), `query/parser.rs`, `query/scope_trie.rs`, `indexer/ignore.rs`, `indexer/burst.rs`: read by reference only. The directory trie's build cost is noted; its lookup cost is not.
- `agent/http.rs`, `agent/symbols.rs`, `agent/connect.rs`, `updater.rs`, `crashlog.rs`, `maintenance.rs`, `config.rs` beyond defaults.
- `SettingsModal.tsx`, `SetupModal.tsx`, the settings tabs and `DuplicatesModal.tsx`: not read for render cost.
- macOS and Linux behaviour, including the daemon path. All measurements are from Windows, where the daemon is disabled.
- The `smart-embeddings` feature and the visual tier, which is not active in shipped builds.
- The uncommitted working-tree changes were read as part of the files but not analysed separately from the last commit.

**Open questions that need the running app to answer:**

- [ ] Does a search during a crawl hang in the real app as it does in the model? A crawl over a folder with a few thousand files and one typed query would show it.
- [ ] What does a keystroke cost end to end on a real index with content and semantic units, split by tier?
- [ ] How long do startup and the launch crawl take at 100k and 1M files?
- [ ] What is the resident memory with the entries snapshot loaded, and how much does the working-set trim cost in page faults?
- [ ] How long does a window summon take in launcher mode?
- [ ] How much does the OCR session pool oversubscribe the CPU during an image-heavy crawl?
- [ ] What is idle CPU with the window hidden?

**Artifacts left on disk by this analysis.** The release builds of the `bench`, `mem_probe` and `facet_bench` examples under `src-tauri/target/release/examples/`, which is a build directory. The two standalone experiment programs are in the session's temporary folder, outside the repository. No tracked file was modified.
