# Babbage (`v1.0.16`) — Complete Codebase Review Report

**Scope:** Exhaustive, read-only audit of all source, build, configuration, CLI, vendor, test, benchmark, locale, and documentation files in the repository.

---

## System Trace: Data, UI State & Image Processing Flow

### 1. Indexing & Data Flow (`walker` → `metadata` / `metadata_db` → `extract` → `content` → `embed`)
1. **Startup & Process Model (`src-tauri/src/lib.rs:334-700`, `cli/src/bin/babbage-daemon.rs:94-224`)**:
   - On Unix, the Tauri GUI (`DaemonClient::ensure`, `src-tauri/src/daemon.rs:146-185`), CLI (`cli/src/main.rs`), and MCP server (`src-tauri/src/agent/mcp.rs`) attempt to connect to `babbage-daemon` over a Unix domain socket authenticated via `<config_dir>/daemon.token`. On Windows (`daemon.rs:149`), `DaemonClient::ensure()` returns `false`, so the GUI's in-process `IndexerManager` (`src-tauri/src/indexer/mod.rs:97-118`) is the sole writer while CLI/MCP open read-only views (`IndexerManager::new_readonly`).
2. **Metadata Arena & SQLite WAL (`src-tauri/src/indexer/arena.rs`, `metadata.rs`, `metadata_db.rs`)**:
   - `MetadataDb` (`metadata_db.rs:73-204`) persists `roots`, `nodes`, `semantic_units`, `visual_units`, `file_usage`, and `meta` in SQLite WAL mode.
   - At startup (`indexer/mod.rs:412-563`), `MetadataStore` loads rows into a compact in-memory `NodeArena` (`arena.rs:69-103`: 64-byte `Node` structs, concatenated folded UTF-8 filenames, parent pointers, `PathIndex`, and a 64-bit `char_bloom` prefilter mask per basename).
3. **Directory Walk & Reconcile (`src-tauri/src/indexer/walker.rs:321-432`, `ignore.rs:51-203`, `indexer/mod.rs:845-1035`)**:
   - `walker::walk_directories` traverses `config.watched_dirs` using `jwalk` on `crawl_pool` (`src-tauri/src/util/pools.rs:53-79`), pruning `excluded_dirs`, `SENSITIVE_NAMES`, hidden entries, and `.gitignore`/`.cursorignore`/`.babbageignore` rules via `IgnoreMatcher`.
   - `MetadataStore::reconcile_root` diffs walked `FileEntry` records against the arena by `(dev, ino, size, modified)`, classifying files into unchanged, renamed (by `(dev, ino)`), modified, or newly added, and schedules eligible files for content extraction.
4. **Content Extraction & Tantivy Indexing (`src-tauri/src/indexer/extract/mod.rs:716-845`, `content.rs:356-682`)**:
   - Up to 8 extraction workers (`extract/mod.rs:451-534`) sniff file magic bytes and extensions (`sniff_format`, `extract/mod.rs:127-205`), routing to plain-text (`text.rs`), delimited (`delimited.rs`), Office/EPUB/ODF XML (`office.rs`), PDF text + scanned-page OCR (`pdf.rs`, `pdf_image.rs`), Image OCR (`image.rs`), or opt-in external commands (`external.rs`).
   - Extracted text is indexed into Tantivy schema v4 (`content.rs:320-354`) with custom CJK unigram+bigram tokenization (`tokenizer.rs:99-245`) and look-alike folded trigrams (`content_folded`), then forwarded to `EmbeddingQueue`.
5. **Semantic & Visual Embedding (`src-tauri/src/embed/queue.rs`, `table.rs`, `smart.rs`, `units.rs`, `visual.rs`)**:
   - `EmbeddingQueue` chunks extracted text into 240-token windows (48-token overlap, up to `content_max_semantic_units = 48`), embeds them via `StaticTable` (`potion-base-8M` i8/f32 lookup table), `SmartEmbedder` (`bge-small-en-v1.5` ONNX), or `FallbackEmbedder` (feature hashing), and writes `i8`-quantized vectors to SQLite `semantic_units` and in-memory `UnitStore`.
   - A separate single-threaded pass (`start_phash_backfill`, `indexer/mod.rs:157-285`) computes 64-bit dHashes (`image::phash64`), screenshot burst signatures (`burst.rs`), and optional CLIP visual vectors (`visual.rs`).

### 2. Query & UI State Flow (`SearchBar` → `App.tsx` → `commands.rs` → `query` → `search` → `VirtualGrid`)
1. **Frontend State & Input (`src/App.tsx:249-634`, `src/components/SearchBar.tsx`)**:
   - `App.tsx` holds top-level state (`query`, `results`, `facets`, `interpretation`, `activeFilters`, `facetInclude`, `facetExclude`, `sortMode`, `viewMode`, `selectedIndex`, `previewResult`, `detailsResult`, `config`, `status`).
   - Keystrokes in `SearchBar` update `query`; a debounced effect (`App.tsx:314-400`: 0 ms for `<= 2` chars, 16 ms for `<= 4` chars, 35 ms otherwise) invokes `search_files` with a monotonic `querySeq` counter to discard out-of-order responses.
2. **Query Compilation & Four-Tier Retrieval (`src-tauri/src/query/`, `src-tauri/src/search/mod.rs:849-1658`)**:
   - `intent::compile_with_aliases` (`query/intent.rs:158-401`) translates natural-language phrases and user aliases into structured filter tokens and removable `InterpretationChip`s.
   - `lexer::tokenize` and `parser::parse_query` build a `ParsedQuery` AST; `router::build_plan` selects execution tiers; `DirectoryTrie::resolve` (`scope_trie.rs`) resolves `in:` scopes.
   - `search_response_on_pool` (`search/mod.rs:849`) executes Tier 1 (`FuzzySearcher` with `nucleo_matcher`), Tier 2 (`ContentIndex` Tantivy BM25 + regex + proximity), Tier 3 (`ExactScanner` cosine similarity over `UnitStore`), Tier 3b (Visual search), and Tier 4 (`search_folded_shortfall` trigram OCR-misread verification), fuses scores via Reciprocal Rank Fusion (RRF, $k = 60$), applies filename/recency/frecency/depth adjustments (`finalize_score`), and computes server-side facets (`compute_facets`).
3. **Virtualization, Snippets & Preview (`src/components/VirtualGrid.tsx`, `src/App.tsx:465-512`, `src/lib/thumbnails.ts`)**:
   - `displayResults` (`App.tsx:764-773`) applies client-side category chip filters (`facetInclude` / `facetExclude`) and feeds `VirtualGrid` (1-column `FileRow` list or multi-column `ResultItem` grid).
   - Visible rows lacking snippets are observed via `IntersectionObserver` (`App.tsx:465-512`) and batched in 30 ms windows to `get_snippets`.

### 3. Image Processing, OCR & Thumbnail Pipeline
1. **Thumbnail & Preview Generation (`src-tauri/src/indexer/extract/image.rs:883-947`, `src-tauri/src/preview.rs:73-205`)**:
   - `useThumbnail` (`src/lib/thumbnails.ts:83-113`) calls `get_thumbnail_data`, which decodes the source image under `decode_budget()` (256 MiB semaphore, `util/budget.rs`) with `image::Limits` (`MAX_IMAGE_DIMENSION = 16_384`, `MAX_IMAGE_PIXELS = 50_000_000`), downscales to 240 px (`image.thumbnail(240, 240)`), converts via `.to_rgb8()`, JPEG-encodes into `<data_dir>/thumbs/`, and base64-encodes a `data:image/jpeg;base64,...` URI. PDF first pages are rendered via external `pdftoppm`/`qlmanage`/`sips` (`preview.rs:113-170`).
2. **OCR Pipeline (`src-tauri/src/indexer/extract/image.rs:582-720`, `src-tauri/vendor/paddleocr_rs_onnx/`, `src-tauri/src/ocr_indra/`)**:
   - `extract_image_text` checks `OCR_CACHE` (4,096-entry FNV-1a `(path, len, mtime, engine)` cache), decodes the image under `decode_budget()`, and dispatches to:
     - **PP-OCRv6 (`"accurate"`, `vendor/paddleocr_rs_onnx/`)**: resizes to `[32, 960]` multiple of 32 (`det.rs`), runs DBNet detection (`det.onnx`), crops/deskews bounding boxes in parallel via Rayon (`rec.rs:preprocess_region`), optionally runs 180° orientation classification (`cls.rs`), and runs CTC beam/greedy decoding (`rec.rs`, `decode.rs`) over a `Mutex<Vec<Session>>` pool.
     - **Indra OCR (`"indra"`, `src-tauri/src/ocr_indra/`)**: converts RGBA→Gray (`imageops.rs`), detects coarse 0°/90°/180°/270° orientation (`orient.onnx`), estimates fine skew via projection profile search (`skew.rs`), detects text boxes (`det.onnx`), classifies script per box (`script.onnx`), and routes batches to specialized recognizers (`rec.onnx`, `rec_cjk.onnx`, `rec_rtl.onnx`, `rec_world.onnx`) with Latin fallback (`engine.rs:260-313`).
     - **Tesseract (`"tesseract"`, `image.rs:722-776`)**: spawns external `tesseract <path> stdout -l <langs>` with a 15 s watchdog.

---

## 1. Critical Bugs & Edge Cases

### 1.1. Session Pool Leak & Permanent Deadlock on Inference Error or Panic in `paddleocr_rs_onnx`
- **File & Lines**: `src-tauri/vendor/paddleocr_rs_onnx/src/lib.rs:697-718`
- **Why it happens**:
  In `OcrEngine::recognize_text`, a recognition `Session` is popped from `self.rec_sessions`:
  ```rust
  let session = {
      let mut pool = self.rec_sessions.lock().unwrap();
      loop {
          if let Some(s) = pool.pop() {
              break s;
          }
          pool = self.rec_sessions_cvar.wait(pool).unwrap();
      }
  };
  let (text, confidence) = rec::run_recognition(&session, &tensor, &self.dictionary, &self.config.recognition)?;
  {
      let mut pool = self.rec_sessions.lock().unwrap();
      pool.push(session);
  }
  self.rec_sessions_cvar.notify_one();
  ```
  Line 708 uses the `?` early-return operator *before* pushing `session` back into `self.rec_sessions`. If `rec::run_recognition` returns an `Err(...)` (e.g., ONNX Runtime tensor error, out-of-memory, or invalid dimensions) or panics, `session` is dropped on early exit and `notify_one()` is never called. Once `rec_pool_size` (`1..=4`) errors occur across the lifetime of the process, `self.rec_sessions` is permanently empty and every subsequent OCR call deadlocks forever inside `self.rec_sessions_cvar.wait(pool)` at line 704.
- **Concrete Solution**:
  Wrap the checked-out `Session` in an RAII drop guard so `Drop` unconditionally returns the session to `self.rec_sessions` and signals `self.rec_sessions_cvar`:
  ```rust
  struct SessionLease<'a> {
      engine: &'a OcrEngine,
      session: Option<Session>,
  }
  impl<'a> Drop for SessionLease<'a> {
      fn drop(&mut self) {
          if let Some(s) = self.session.take() {
              if let Ok(mut pool) = self.engine.rec_sessions.lock() {
                  pool.push(s);
              }
              self.engine.rec_sessions_cvar.notify_one();
          }
      }
  }
  ```

---

### 1.2. Infinite Background Loop in `start_phash_backfill` & Broken Burst Grouping for ~50% of All Images (`raw_phash < 0` Signedness Bug)
- **File & Lines**:
  - `src-tauri/src/indexer/metadata_db.rs:1045-1047, 1094-1096`
  - `src-tauri/src/indexer/mod.rs:241-271`
- **Why it happens**:
  `image::phash64` computes an unsigned 64-bit dHash (`u64`) where bit 63 is `1` whenever the top-left pixel is brighter than its right neighbor (~50% of all images). SQLite stores `phash` bit-cast as signed `i64` (`phash as i64`), reserving `u64::MAX` (`-1` in `i64`) as the sentinel for "decode failed" (`metadata_db.rs:956`).
  `MetadataDb::image_paths_missing_burst` (`metadata_db.rs:990`) selects `WHERE is_dir = 0 AND phash IS NOT NULL AND phash != -1 AND burst IS NULL`.
  However, `MetadataDb::assign_burst_for_path` (`metadata_db.rs:1045` and `1094`) checks:
  ```rust
  if raw_phash < 0 {
      return Ok(Vec::new());
  }
  ```
  and for neighbors (`metadata_db.rs:1094`):
  ```rust
  if candidate_phash < 0 || Path::new(&candidate_path).parent() != Some(parent.as_path()) {
      continue;
  }
  ```
  Every valid 64-bit dHash with MSB = 1 (`0x8000_0000_0000_0000..=0xFFFF_FFFF_FFFF_FFFE`) is negative when read as `i64` (`raw_phash < -1`). Consequently:
  1. `assign_burst_for_path` returns `Ok(Vec::new())` early without updating `burst` in SQLite (leaving `burst = NULL`).
  2. `start_phash_backfill` (`indexer/mod.rs:241-271`) loops infinitely: `image_paths_missing_burst(50)` returns the exact same 50 images on every iteration, `set_image_text_sig` sets `burst = NULL` (`metadata_db.rs:1022`), and `assign_burst_for_path` leaves `burst = NULL`, causing `start_phash_backfill` to spin in a continuous Tantivy-read + SQLite-write loop for the entire lifetime of the application.
  3. Half of all images can never be grouped into bursts or matched as burst neighbors.
- **Concrete Solution**:
  In `src-tauri/src/indexer/metadata_db.rs:1045` and `1094`, check `raw_phash == -1` (the `u64::MAX` failure sentinel) instead of `raw_phash < 0`:
  ```rust
  if raw_phash == -1 {
      return Ok(Vec::new());
  }
  // and at line 1094:
  if candidate_phash == -1 || Path::new(&candidate_path).parent() != Some(parent.as_path()) {
      continue;
  }
  ```

---

### 1.3. Transparent PNG / WebP / GIF / ICO Thumbnails & Previews Render With Solid Black Backgrounds (Plus Missing `.ico` Support)
- **File & Lines**:
  - `src-tauri/src/indexer/extract/image.rs:38-43, 893`
  - `src-tauri/src/preview.rs:63-70, 82-87, 160-163`
  - `src-tauri/src/embed/visual.rs:220`
  - `src-tauri/src/config.rs:545-547`
  - `src-tauri/Cargo.toml:44`
  - `src/lib/thumbnails.ts:10-13`
- **Why it happens**:
  1. **Alpha channel stripped without compositing**: `generate_thumbnail` (`image.rs:893`: `let thumb = image.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8();`), `render_external` (`preview.rs:161`: `img.thumbnail(THUMB_EDGE, THUMB_EDGE).to_rgb8()`), and `preprocess_clip_image` (`visual.rs:220`: `img.resize_exact(...).to_rgb8()`) call `.to_rgb8()` directly on `DynamicImage`. In the `image` crate, `to_rgb8()` drops the alpha channel `[R, G, B, A] -> [R, G, B]` without alpha blending. Because transparent pixels in PNG, WebP, GIF, and ICO files are `[0, 0, 0, 0]`, JPEG thumbnails turn transparent backgrounds into pitch black (`#000000`), rendering dark-text transparent screenshots, black icons, diagrams, and logos completely invisible (unlike `ocr_indra/imageops.rs:25-36`, which explicitly alpha-blends over white: `(r * a + 255 * (255 - a)) / 255`).
  2. **Missing `.ico` support**: `src-tauri/Cargo.toml:44` enables `features = ["png", "jpeg", "gif", "webp", "bmp", "tiff"]` without `"ico"` (it only compiles in today by accident via `vendor/paddleocr_rs_onnx/Cargo.toml:62`), and `.ico` is missing from `preview::kind_for` (`preview.rs:63-70`), `is_supported_image_extension` (`image.rs:38-43`), `NOW_INDEXABLE` (`config.rs:545-547`), and `THUMB_EXTS` (`src/lib/thumbnails.ts:10-13`).
- **Concrete Solution**:
  1. Add a shared helper that composites images where `img.color().has_alpha()` over a white (`[255, 255, 255]`) or neutral background before converting to `RgbImage` and saving as JPEG:
     ```rust
     pub fn to_rgb8_composited(img: &image::DynamicImage, bg: [u8; 3]) -> image::RgbImage {
         if !img.color().has_alpha() {
             return img.to_rgb8();
         }
         let rgba = img.to_rgba8();
         let (w, h) = rgba.dimensions();
         let mut out = image::RgbImage::new(w, h);
         for (src, dst) in rgba.pixels().zip(out.pixels_mut()) {
             let a = src[3] as u32;
             let inv = 255 - a;
             dst[0] = ((src[0] as u32 * a + bg[0] as u32 * inv + 127) / 255) as u8;
             dst[1] = ((src[1] as u32 * a + bg[1] as u32 * inv + 127) / 255) as u8;
             dst[2] = ((src[2] as u32 * a + bg[2] as u32 * inv + 127) / 255) as u8;
         }
         out
     }
     ```
  2. Add `"ico"` explicitly to `src-tauri/Cargo.toml:44`, `preview::kind_for` (`preview.rs:64`), `is_supported_image_extension` (`image.rs:41`), `NOW_INDEXABLE` (`config.rs:545`), and `THUMB_EXTS` (`src/lib/thumbnails.ts:11`).

---

### 1.4. Windows PDF Previews Always Fail (`find_tool` Omits `.exe`), Non-Atomic JPEG Writes Serve Corrupted Previews, and Shared `.render` Work Dir Races/Leaks
- **File & Lines**: `src-tauri/src/preview.rs:95-100, 113-170, 212-222`
- **Why it happens**:
  1. **Missing `.exe` suffix on Windows (`preview.rs:212-222`)**: `find_tool("pdftoppm")` checks `d.join(name).is_file()` across `PATH`. On Windows, the executable is `pdftoppm.exe`, so `d.join("pdftoppm").is_file()` always returns `false`, completely breaking PDF thumbnails and previews on Windows.
  2. **Non-atomic JPEG writes (`preview.rs:95-100, 160-163`)**: Unlike `image.rs:894-902` (which writes `.jpg.tmp.{pid}` and renames atomically), `render_external` writes directly to `&target` (`thumb.save_with_format(&target, ImageFormat::Jpeg)`). Because `is_fresh(source, &target)` (`preview.rs:95-100`) only checks `t.len() > 0` before acquiring the render slot, a concurrent caller for the same PDF sees the partially written file (`len() > 0`), skips rendering, and returns a truncated, corrupted JPEG to the frontend.
  3. **Shared `.render-{pid}-{hash}` race and directory leak (`preview.rs:119-165`)**: `work` (`cache_dir.join(format!(".render-{}-{:016x}", std::process::id(), hash))`) has no per-request counter and `create_dir_all(&work)` runs *before* `acquire()` (`preview.rs:125-127`). Two concurrent calls for the same PDF share the same `work` directory; the first to finish executes `let _ = fs::remove_dir_all(&work);` (`preview.rs:159`) while the second is running `pdftoppm`. Additionally, if `fs::read_dir(&work)` fails on macOS (`preview.rs:144`: `let Ok(entries) = ... else { return None; }`), `work` is never deleted.
- **Concrete Solution**:
  1. In `find_tool` (`preview.rs:212-222`), check `let bin = if cfg!(windows) && !name.ends_with(".exe") { format!("{name}.exe") } else { name.to_string() };`.
  2. Create `work` *after* acquiring `_permit` using a monotonic `AtomicU64` sequence counter and wrap it in a `Drop` guard that calls `fs::remove_dir_all(&work)`.
  3. Save the JPEG to `work.join("out.jpg")` and atomically `fs::rename(work.join("out.jpg"), &target)`.

---

### 1.5. Text, Markdown, Code, and Audio/Video Files Starting With `"BM"` or `"RIFF"` Misclassified as Images and Dropped from Content Index
- **File & Lines**: `src-tauri/src/indexer/extract/mod.rs:127-150, 741-765`
- **Why it happens**:
  `sniff_format()` checks magic bytes *before* checking file extensions:
  ```rust
  if header.starts_with(b"\x89PNG")
      || header.starts_with(b"\xff\xd8\xff")
      || header.starts_with(b"GIF8")
      || header.starts_with(b"RIFF")
      || header.starts_with(b"BM")
  {
      return DocumentFormat::Image;
  }
  ```
  - Any plain-text, Markdown, CSV, or source code file whose first two bytes are ASCII `"BM"` (e.g., a note starting with `"BM25 ranking specification"` or `"BMI calculation"`) matches `header.starts_with(b"BM")` and is classified as `DocumentFormat::Image`. `extract_document_inner` (`extract/mod.rs:741-765`) sends it to `image::extract_image_text`, which fails `image::image_dimensions` and drops 100% of the file's text content!
  - Any RIFF container such as `.wav` audio or `.avi` video starts with `b"RIFF"` (`extract/mod.rs:147` never checks `&header[8..12] == b"WEBP"`) and is also misclassified as `DocumentFormat::Image`.
- **Concrete Solution**:
  In `sniff_format` (`src-tauri/src/indexer/extract/mod.rs:141-150`):
  1. Check WebP with `n >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP"`.
  2. Check GIF with `n >= 6 && (header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a"))`.
  3. Check BMP only when `n >= 14 && header.starts_with(b"BM") && header[6..10] == [0, 0, 0, 0]` *and* the file extension is not in `text::is_text_file(path)` or `delimited::is_delimited(path)`.

---

### 1.6. `ModelRegistry::reload()` Leaves `EmbeddingQueue` and `UnitStore` Out of Sync, Silently Dropping All New and Smart Embeddings
- **File & Lines**:
  - `src-tauri/src/embed/model.rs:165-175, 243-261`
  - `src-tauri/src/embed/queue.rs:70, 198, 236, 261`
  - `src-tauri/src/embed/units.rs:104-124, 171-194`
  - `src-tauri/src/embed/scanner.rs:78-81`
  - `src-tauri/src/indexer/mod.rs:287-302`
  - `src-tauri/src/models.rs:646-660, 936-953`
- **Why it happens**:
  Three compounding bugs break semantic search after model downloads, model switches, or deletions:
  1. **`EmbeddingQueue` captures a stale `Arc<ModelRegistry>` (`queue.rs:70`)**: `EmbeddingQueue::start` executes `let model = self.model.clone();` once when the worker thread spawns at startup. On first run (before `potion-base-8M` is downloaded), `self.model` has `model_id = 1` (`FALLBACK_MODEL_ID`). When the user downloads `potion-base-8M` in Setup or Settings, `models.rs:647` calls `ModelRegistry::reload()`, updating `GLOBAL` to `REAL_MODEL_ID = 2`, and `on_semantic_models_ready` (`indexer/mod.rs:2017-2050`) resets `indexed_gen = 0`. However, `EmbeddingQueue`'s worker loop continues using its captured `model` (`model_id = 1`), tagging all re-embedded vectors with `model_id = 1` and reusing `hash_cache` entries keyed only by `u64` content hash (`queue.rs:198`). At query time, `ExactScanner::scan_with_model` (`scanner.rs:78-81`) filters by `ModelRegistry::global().model_id` (`Some(2)`), rejecting 100% of the vectors!
  2. **`UnitStore` dimension hardcoded to 256 drops all 384-d `smart` embeddings (`indexer/mod.rs:289`, `units.rs:114`)**: `IndexerManager` initializes `UnitStore::new(EMBEDDING_DIM)` where `EMBEDDING_DIM = 256`. When `semantic_model == "smart"` (`bge-small-en-v1.5`, `SMART_DIM = 384`), `UnitStore::add_unit` (`units.rs:114`: `if vector.len() != self.dim { return; }`) silently discards every 384-dimensional vector in memory (and `retain_only_model` at `units.rs:171-194` never updates `self.dim`).
  3. **Missing `ModelRegistry::reload()` calls (`models.rs:646, 936-953`)**: `run_download` (`models.rs:646`) only calls `ModelRegistry::reload()` for `Pack::Models`, omitting `Pack::Smart`; and `models::delete_all` (`models.rs:936-953`) deletes the model files from disk without calling `ModelRegistry::reload()`.
- **Concrete Solution**:
  1. Inside `EmbeddingQueue::start`'s loop (`queue.rs:108`), read `let model = ModelRegistry::global();` on each batch instead of capturing `self.model` once at thread start, and key `hash_cache` by `(model.model_id, content_hash)`.
  2. In `UnitStore::add_unit` and `UnitStore::retain_only_model` (`units.rs:114, 171`), update `self.dim = vector.len()` when the store is empty or when switching `model_id`, and initialize `UnitStore::new(model.dim)` in `indexer/mod.rs:289`.
  3. Call `ModelRegistry::reload()` after `Pack::Smart` download (`models.rs:646`) and inside `models::delete_all()` (`models.rs:951`).

---

### 1.7. `SmartEmbedder::load` Conflicts With Process-Wide `ort` Environment and Bypasses Dynamic Runtime Loader
- **File & Lines**:
  - `src-tauri/src/embed/smart.rs:85-95`
  - `src-tauri/src/ocr.rs:106-109, 146-199`
  - `src-tauri/vendor/paddleocr_rs_onnx/src/lib.rs:198-205`
- **Why it happens**:
  `ort` (with `load-dynamic`) allows only a single global `Environment` and requires `ort::init_from(&runtime_dll)` before creating any `Environment` so that Windows does not load the incompatible `C:\Windows\System32\onnxruntime.dll` (which panics on load; see comment at `ocr.rs:106-109`). `SmartEmbedder::load` (`smart.rs:85-95`) defines its own separate `static ENVIRONMENT: OnceLock<Arc<Environment>>`, calls bare `ort::init().with_name("babbage-smart").build()` without calling `crate::ocr::runtime_loadable()`, and does not wrap session initialization in `std::panic::catch_unwind`.
- **Concrete Solution**:
  In `SmartEmbedder::load` (`src-tauri/src/embed/smart.rs:85-95`), call `if !crate::ocr::runtime_loadable() { return Err(...); }`, reuse `paddleocr_rs_onnx::shared_environment()?`, and wrap `SessionBuilder::new(env)...` in `std::panic::catch_unwind`.

---

### 1.8. Indra OCR Fallback Bug: Empty/Low-Confidence Specialized Script Recognition Blocks Base Recognizer
- **File & Lines**: `src-tauri/src/ocr_indra/engine.rs:260-313`
- **Why it happens**:
  In `Engine::recognize`, lines are partitioned by script (`None` for Latin, `Some(0..2)` for CJK, RTL, and World). In the first pass (`engine.rs:269-289`):
  ```rust
  for (j, (text, conf)) in indices.iter().zip(out.into_iter()) {
      let clean = if conf >= MIN_CONF { text } else { String::new() };
      best[*j] = (clean, conf);
  }
  ```
  If a specialized script recognizer (`rec_cjk`, `rec_rtl`, `rec_world`) misclassifies an English/numeric text line and either produces `conf = 0.44` (`< MIN_CONF = 0.45`) or decodes to an empty/whitespace string with high confidence (`("", 0.85)`), `best[*j]` is set to `(String::new(), 0.44)` or `(String::new(), 0.85)`.
  Then in the fallback pass over `empty_lines` (`engine.rs:294-312`), the base recognizer (`self.rec`) runs on all lines where `best[i].0.trim().is_empty()`, but line 306 checks:
  ```rust
  if conf >= MIN_CONF && !text.trim().is_empty() && conf > best[*j].1 {
      best[*j] = (text, conf);
  }
  ```
  Because `best[*j].1` holds the confidence of the *empty* specialized result (e.g. `0.85`), valid non-empty text from `self.rec` (e.g. `("Invoice #1042", 0.78)`) is discarded because `0.78 > 0.85` is `false`! Furthermore, lines that were *already* in `group == None` (`self.rec`) and returned empty are redundantly run through `self.rec` a second time.
- **Concrete Solution**:
  In `src-tauri/src/ocr_indra/engine.rs:283-308`:
  1. When `clean.trim().is_empty()`, store `best[*j] = (String::new(), -1.0)` so any valid non-empty fallback result with `conf >= MIN_CONF` wins.
  2. Only include lines in `empty_lines` where `group.is_some()` so `self.rec` is not re-run on lines it already processed in the first pass.

---

### 1.9. `preprocess_region` in `paddleocr_rs_onnx` Panics on `NaN`/`Inf` Before `is_valid_quad`, Corrupts Tilted Wide Boxes, and Underflows in `cls.rs`/`det.rs`
- **File & Lines**:
  - `src-tauri/vendor/paddleocr_rs_onnx/src/rec.rs:90-129, 161-165`
  - `src-tauri/vendor/paddleocr_rs_onnx/src/det.rs:31-43, 215, 247-272`
  - `src-tauri/vendor/paddleocr_rs_onnx/src/cls.rs:113-122, 175`
  - `src-tauri/vendor/paddleocr_rs_onnx/src/decode.rs:69, 91`
- **Why it happens**:
  1. **Panic on `NaN` before validation (`rec.rs:161-165`)**: `preprocess_region` calls `let bbox = order_bbox_points(region.bbox);` at line 161 *before* calling `if !is_valid_quad(&bbox)` at line 164 (which checks `p.iter().all(|v| v.is_finite())` at line 91). Inside `order_bbox_points` (`rec.rs:107-128`), four `.partial_cmp(...).unwrap()` calls on `p[0] + p[1]` and `p[0] - p[1]` panic immediately if any coordinate is `NaN`. Similarly, `.partial_cmp().unwrap()` is used on floats in `det.rs:215`, `cls.rs:175`, and `decode.rs:69, 91`.
  2. **Self-intersecting bowtie on tilted wide boxes (`rec.rs:104-129`)**: `order_bbox_points` assigns `tl = min(x+y)`, `br = max(x+y)`, `tr = max(x-y)`, `bl = min(x-y)`. For a wide text box of aspect ratio $W/H > 1$ rotated by angle $\theta > \arctan(H/W)$ (e.g., $200 \times 20$ tilted by $10^\circ$), two corners share the same argmin/argmax under $x+y$ and $x-y$, producing duplicate vertices and a self-intersecting quad that `is_valid_quad` rejects, dropping tilted text lines.
  3. **Zero-dimension resize & unsigned subtraction underflow (`det.rs:35-36`, `cls.rs:120-121`)**: In `det.rs:35-36`, `((orig_w as f32 * scale) as u32 / 32) * 32` truncates to `0` without `.max(32)` on extreme aspect ratios (`1000x1`), creating a 0-width tensor. In `cls.rs:120-121`, `let start_x = ((new_w - CROP_SIZE) / 2).max(0);` subtracts unsigned `u32` values (`CROP_SIZE = 224`), panicking or wrapping to `u32::MAX / 2` if `resize_short` rounding yields `new_w < 224`.
- **Concrete Solution**:
  1. Call `if !is_valid_quad(&region.bbox) { return Ok(None); }` *before* `order_bbox_points(region.bbox)` in `rec.rs:161`, and replace all `.partial_cmp(...).unwrap()` calls in `rec.rs`, `det.rs`, `cls.rs`, and `decode.rs` with `.total_cmp(...)`.
  2. In `order_bbox_points`, sort points by $x$ (left pair vs. right pair), pick `tl`/`bl` by $y$ from the left pair and `tr`/`br` by $y$ from the right pair (standard PaddleOCR point ordering).
  3. Clamp `new_w` and `new_h` with `.max(32)` in `det.rs:35-36` and use `new_w.saturating_sub(CROP_SIZE) / 2` in `cls.rs:120-121`.

---

### 1.10. Live File Watcher Bypasses `excluded_extensions` and `SENSITIVE_NAMES`, Indexing Private Keys (`.key`, `.pem`, `.pfx`) in Plain Text
- **File & Lines**:
  - `src-tauri/src/indexer/watcher.rs:86-97, 488-513, 700-766, 774-892`
  - `src-tauri/src/indexer/content.rs:1516`
  - `src-tauri/src/indexer/extract/mod.rs:777`
- **Why it happens**:
  1. `watcher::is_path_excluded` (`watcher.rs:488-513`) and `handle_directory_rename` (`watcher.rs:700-766`) check `prunes_for_roots`, `is_active_ignored`, `skip_hidden`, and `is_dir_excluded`, but **never check `cfg.is_extension_excluded(path)` or `walker::SENSITIVE_NAMES`** (`1password`, `bitwarden`, `keepass`, `keepassxc`).
  2. Worse, `ContentIndex::is_text_Readable` (`content.rs:1516`) lists `"key"` in `TEXT_EXTENSIONS`, and `sniff_format` (`extract/mod.rs:204`) returns `DocumentFormat::Unknown` for `.key`, `.pem`, and `.env`, which `extract_document_inner` (`extract/mod.rs:777`) extracts as plain text! Thus, if any `.key`, `.pem`, `.pfx`, `.p12`, or file inside `1Password`/`Bitwarden`/`KeePass` is created or modified while Babbage is running, the live watcher indexes its raw contents into Tantivy and SQLite!
  3. Additionally, `PendingUpserts::due` (`watcher.rs:86-97`) drains a `HashMap` in arbitrary hash order, returning child files before their newly created parent directories. When `MetadataStore::add` processes `/a/b/c.txt` before `/a/b`, `/a/b/c.txt` receives `parent = NO_PARENT`, so deleting `/a/b` later fails to remove `/a/b/c.txt` via `descendant_paths`.
- **Concrete Solution**:
  1. In `watcher::is_path_excluded` (`src-tauri/src/indexer/watcher.rs:488-513`) and `handle_directory_rename` (`watcher.rs:716`), check `cfg.is_extension_excluded(path)` for files and check `walker::SENSITIVE_NAMES` across all path components.
  2. Remove `"key"` from `TEXT_EXTENSIONS` in `src-tauri/src/indexer/content.rs:1516` and explicitly reject credential extensions (`pem`, `key`, `pfx`, `p12`, `kdbx`, `keystore`, `jks`, `asc`, `ovpn`) in `extract/mod.rs:sniff_format`.
  3. Sort `ready` paths in `PendingUpserts::due` (`watcher.rs:95`) by `path.components().count()` ascending so parent directories are always inserted before their children.

---

### 1.11. Arbitrary Unscoped File Read & Credential Denylist Bypasses in Daemon, Agent HTTP, MCP `find_symbol`, and `agent_read_allowed`
- **File & Lines**:
  - `cli/src/bin/babbage-daemon.rs:385-428`
  - `src-tauri/src/agent/mod.rs:797-858`
  - `src-tauri/src/agent/http.rs:505-554`
  - `src-tauri/src/agent/symbols.rs:254-285`
- **Why it happens**:
  1. **Daemon `"get_snippets"` and `"read_snippet"` bypasses (`cli/src/bin/babbage-daemon.rs:385-428`)**:
     - `"get_snippets"` (`babbage-daemon.rs:394`) calls `indexer.get_snippet(Path::new(p), query)` directly without checking `indexer.metadata.get(Path::new(p)).is_some()` (unlike `commands.rs:306-312`), allowing any local client to extract text from arbitrary unindexed filesystem paths.
     - `"read_snippet"` (`babbage-daemon.rs:413`) checks `indexer.metadata.get(Path::new(path)).is_none()` but **never calls `agent_read_allowed(path, &cfg)`**, bypassing `agent_scoped_roots`, credential filename blocks, and `excluded_extensions` whenever the daemon is running!
  2. **`agent_read_allowed` symlink & filename bypasses (`src-tauri/src/agent/mod.rs:797-858`)**:
     - When `cfg.agent_scoped_roots` is empty (the default), `canonicalize` (`mod.rs:845`) is never called. A symlink inside a watched folder (`notes.txt -> ~/.ssh/id_ed25519`) passes all component checks on the symlink path and reads the target secret file.
     - Lines 812–820 only check exact equality (`lower == ".env"`, `lower == "id_rsa"`, etc.), so `.env.local`, `.env.production`, `.env.development`, `id_rsa.pub`, `id_ed25519.bak`, `.npmrc`, `.pypirc`, `.git-credentials`, and `.netrc` all bypass the denylist.
  3. **HTTP `/search` bypasses `agent_scope_paths` (`src-tauri/src/agent/http.rs:525-526`)**:
     - `GET /search` calls `state.backend.search(&effective, limit)` without calling `agent_scope_paths(path, &cfg)?`, exposing search hits from outside `agent_scoped_roots`, and always returns `snippet: null`.
  4. **`tool_find_symbol` bypasses `agent_scope_paths` and `agent_read_allowed` (`src-tauri/src/agent/symbols.rs:254-285`)**:
     - When `path` is `None` (`symbols.rs:257-260`), `agent_scope_paths` is never called! Furthermore, `format!("content:\"{}\" {}", query, ...)` (`symbols.rs:265`) does not escape `"` in `query` (allowing query filter injection), and `extract_symbols_from_file(file_path, ...)` (`symbols.rs:285`) reads and parses files from disk without checking `agent_read_allowed`.
- **Concrete Solution**:
  1. In `babbage-daemon.rs:394`, filter `"get_snippets"` by `indexer.metadata.get(Path::new(p)).is_some()`, and in `"read_snippet"` (`babbage-daemon.rs:407`), call `babbage_lib::agent::agent_read_allowed(Path::new(path), &indexer.config.read())?`.
  2. In `agent_read_allowed` (`src-tauri/src/agent/mod.rs:797-858`), always `dunce::canonicalize(path)` first and run the component/filename/extension checks on *both* the requested path and the canonical path; block `lower.starts_with(".env.")`, `lower.starts_with("id_rsa")`, `lower.starts_with("id_ed25519")`, `lower.starts_with("id_ecdsa")`, `.npmrc`, `.pypirc`, `.git-credentials`, and `.netrc`.
  3. Enforce `agent_scope_paths(path, &cfg)?` in `http.rs:525` and unconditionally in `symbols.rs:257` (plus `agent_read_allowed` at `symbols.rs:284`).

---

### 1.12. Default Alias Expansion & Quoted Phrases Break Filename, Semantic, Visual, and Phrase Search
- **File & Lines**:
  - `src-tauri/src/query/intent.rs:332-382`
  - `src-tauri/src/config.rs:60-68`
  - `src-tauri/src/search/mod.rs:1050-1083, 1170`
  - `src-tauri/src/indexer/content.rs:829-849, 868, 1560-1568`
- **Why it happens**:
  1. **Default aliases disable filename, semantic, and visual search (`intent.rs:336-368`, `search/mod.rs:1050-1083`)**:
     `BabbageConfig::default()` (`config.rs:60-68`) ships with 6 default aliases (`resume`, `invoice`, `receipt`, `screenshot`, `contract`, `photo`). When a user types `resume` or `invoice`, `compile_with_aliases` (`intent.rs:359-360`) strips `"resume"` from `work` and appends `re:"(?i)(resume|cv|curriculum vitae)"`.
     In `search/mod.rs:1050-1083`, `t.is_regex` terms are excluded from `fuzzy_terms`, `bm25_groups`, and `search_text`. Thus for single-word alias queries, `fuzzy_text` and `search_text` become empty `""`:
     - Tier 1 (`FuzzySearcher` over filenames/folders) is **completely skipped** (`!fuzzy_text.is_empty()` is `false` at `search/mod.rs:1170`), so a file named `resume.pdf` with no extracted text (or an unindexed format) is never found!
     - Tier 3 (Semantic search), Tier 3b (Visual search), and filename ranking bonuses (`finalize_score`) are **completely skipped**.
     - Worse, Tantivy's `RegexQuery` (`content.rs:868`) matches against single tokens in the inverted index, so multi-word alias expansions like `"curriculum vitae"` (`config.rs:62`) and `"screen shot"` (`config.rs:65`) can **never** match any token!
     - In addition, the alias loop in `intent.rs:336-368` is accidentally nested *inside* `if let Ok(re) = cached_regex(r"(?i)\bin\s+...")` before `re.captures(&work)`.
  2. **Quoted phrases (`"exact phrase"`) are silently stripped of quotes (`search/mod.rs:1050-1071`, `content.rs:833, 1560-1568`)**:
     `lexer::tokenize` strips quotes into `Token::Quoted`, and `parser.rs` sets `Term { phrase: true }`. However, `search/mod.rs:1050-1071` ignores `t.phrase` and pushes bare `t.text` into `bm25_groups`, and `sanitize_query_text` (`content.rs:1564`) replaces `"` with `" "`. Thus `"exact phrase"` is executed as an unquoted boolean `exact AND phrase` at arbitrary positions in the document.
- **Concrete Solution**:
  1. Move the alias loop out of the `in <place>` block in `intent.rs:332-382`. Instead of stripping the original term and emitting a token-only `re:` clause, keep the original term in `fuzzy_terms`/`search_text` and expand aliases into a boolean `Should` group of parsed terms/phrases in `ContentIndex` and `FuzzySearcher`.
  2. In `search/mod.rs:1050-1071` and `content.rs:833`, when `t.phrase == true`, wrap the sanitized inner terms in double quotes (`format!("\"{}\"", sanitize_query_text(&t.text).trim())`) so Tantivy constructs a `PhraseQuery`.

---

### 1.13. `type:media` Emitted by Intent Compiler, Slash Menu, and Category Chips Returns Zero Results
- **File & Lines**:
  - `src-tauri/src/query/intent.rs:281-289`
  - `src-tauri/src/search/mod.rs:69, 1956-2013`
  - `src/App.tsx:698`
  - `src/lib/categories.ts:32`
- **Why it happens**:
  `intent::compile` maps plain-language queries containing `"videos"`, `"movies"`, `"clips"`, `"audio"`, `"music"`, or `"songs"` to `type:media` (`intent.rs:282, 287`), and the `/media` slash command (`src/App.tsx:698`) inserts `type:media`.
  However, `matches_type_category` (`src-tauri/src/search/mod.rs:1956-2013`) only handles `"video"` (line 2000) and `"audio"` (line 2003) and has **no branch for `"media"`**! It falls through to line 2012 (`ext.eq_ignore_ascii_case("media")`), returning **0 results** for every `/media` command and every natural-language video/audio query. (Also `categories.ts:32` includes `"m4v"` and `"opus"` in `MEDIA_EXTS`, which are missing from `search/mod.rs:69, 2000-2004`).
- **Concrete Solution**:
  Add a `"media"` branch in `matches_type_category` (`src-tauri/src/search/mod.rs:2000`) and include `"m4v"` and `"opus"` in both `compute_facets` (`search/mod.rs:69`) and `matches_type_category`:
  ```rust
  if ext_is(typ, &["media"]) {
      return ext_is(
          ext,
          &[
              "mp4", "mkv", "mov", "avi", "webm", "flv", "wmv", "m4v",
              "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus",
          ],
      );
  }
  ```

---

### 1.14. Frontend Category Facet Filtering Breaks for `"recent"` and `"file"`, Snippet Observer Indexes Wrong Array, and Alt-Click Fails in `StrictMode`
- **File & Lines**:
  - `src/App.tsx:474-493, 636-661, 764-773`
  - `src/lib/categories.ts:38-70`
- **Why it happens**:
  1. **Clicking `"Recent"` category chip always displays 0 results (`src/App.tsx:764-773`, `src/lib/categories.ts:49-58`)**:
     `CategoryChips` renders a `"recent"` chip whose badge count comes from `facets.Modified.last_7d` (`categories.ts:68`). Clicking it adds `"recent"` to `facetInclude`. `displayResults` (`App.tsx:764-773`) then filters `results` by `facetInclude.has(classify(r))`. Because `classify(r)` (`categories.ts:49-58`) only returns `"folder" | "image" | "doc" | "code" | "media" | "archive" | "file"` and **never returns `"recent"`**, `facetInclude.has(classify(r))` is `false` for every item!
  2. **Clicking `"Files"` category chip mismatches its badge count (`src/lib/categories.ts:57, 61`)**:
     The `"file"` chip displays `facets.types.file` (which counts *all* non-directory files at `search/mod.rs:58`), but `classify(r)` only returns `"file"` as the final fallback for unknown extensions, hiding all images, docs, code, media, and archives when clicked.
  3. **Snippet `IntersectionObserver` indexes `results` using `displayResults`'s `data-index` (`src/App.tsx:478-479`)**:
     `VirtualGrid` sets `data-index={idx}` to the item's index in `displayResults`. However, the observer callback at `App.tsx:479` reads `const r = results[index];` (the unfiltered array), requesting snippets for the wrong files whenever `facetInclude` or `facetExclude` is active.
  4. **Alt-click category exclusion is a no-op in `React.StrictMode` (`src/App.tsx:644-660`)**:
     In `handleFacetToggle`, `setFacetExclude` is called *inside* `setFacetInclude`'s state updater function (`setFacetInclude((prev) => { ... setFacetExclude((e) => ...); })`). Under `<React.StrictMode>` (`src/main.tsx:10`), React invokes state updater functions twice, toggling `e.has(id) ? next.delete(id) : next.add(id)` twice and canceling the exclusion!
- **Concrete Solution**:
  1. Replace `classify(r)` in `displayResults` (`src/App.tsx:764-773`) with `matchesCategory(r, catId, nowSec)`:
     ```ts
     export function matchesCategory(r: SearchResult, id: CategoryId, nowSec = Date.now() / 1000): boolean {
       if (id === "all") return true;
       if (id === "file") return !r.is_dir;
       if (id === "recent") return r.modified > 0 && nowSec - r.modified <= 7 * 86400;
       return classify(r) === id;
     }
     ```
  2. In `src/App.tsx:479`, index `displayResults[index]` instead of `results[index]`.
  3. In `handleFacetToggle` (`src/App.tsx:644-660`), call `setFacetInclude` and `setFacetExclude` as separate top-level calls outside each other's updater closures.

---

### 1.15. Frontend Keyboard Navigation Stale Closure (`navList` & `handleCopyPath` Missing From Deps), Unclamped `selectedIndex`, and `loadMore` Updater Side-Effect
- **File & Lines**: `src/App.tsx:537-545, 764-789, 944-989`
- **Why it happens**:
  1. `handleKeyDown` (`src/App.tsx:944-989`) reads `navList` (derived from `displayResults`, which depends on `facetInclude` and `facetExclude`) and calls `handleCopyPath`, yet its dependency array at line 988 lists `results` instead of `navList` and omits `facetInclude`, `facetExclude`, and `handleCopyPath`. After clicking a category chip, `ArrowDown`, `Enter`, `Space`, `Cmd+C`, and `Cmd+I` operate on a stale `navList` and open or copy the wrong file!
  2. When `displayResults` shrinks after toggling a category chip, `selectedIndex` is not clamped, leaving it out of bounds (`>= displayResults.length`).
  3. In `loadMore` (`src/App.tsx:537-545`), `let added = 0;` is mutated inside `setResults((prev) => { ... added = fresh.length; ... })` and immediately checked on line 545 (`if (added === 0) setHasMore(false);`). In React 19 concurrent rendering, state updaters are not guaranteed to run synchronously inside `setResults(...)`, so `added` can still be `0` at line 545, prematurely setting `hasMore = false`.
- **Concrete Solution**:
  1. Replace `results` with `navList` and include `handleCopyPath` in `handleKeyDown`'s dependency array (`src/App.tsx:988`), and clamp `selectedIndex` when `displayResults.length` changes.
  2. Track `results` in a `resultsRef` (or compute `seen` from current state) before calling `setResults` in `loadMore` (`src/App.tsx:537-545`).

---

### 1.16. `useThumbnail` Never Resets `failed`/`src` When `path` Changes & `HoverDetails` Retains Stale Image Dimensions
- **File & Lines**:
  - `src/lib/thumbnails.ts:83-113`
  - `src/components/HoverDetails.tsx:27-43`
- **Why it happens**:
  1. In `useThumbnail` (`src/lib/thumbnails.ts:86-111`), `failed` is initialized to `false` once and **never reset** when `path` or `thumbable` changes. If `PreviewDialog` or a virtualized card renders a file whose thumbnail fails (`failed = true`) and then navigates to another file, `failed` remains `true` forever (`!thumb.failed && thumb.src` in `ResultItem.tsx:76` and `PreviewDialog.tsx:161` stays `false`). Likewise, if `thumbable` becomes `false` or `path` changes to a cache miss, `src` continues displaying the previous file's thumbnail!
  2. In `HoverDetails.tsx:31-43`, `useEffect` checks `if (!active || details.width || details.height) return;`. If `result.path` changes while the component stays mounted, `details.width` is already truthy from the previous image, so it displays the old image's dimensions.
- **Concrete Solution**:
  1. In `useThumbnail` (`src/lib/thumbnails.ts:88-95`):
     ```ts
     useEffect(() => {
       setFailed(false);
       if (!thumbable) {
         setSrc(null);
         return;
       }
       const hit = cached(path);
       if (hit) {
         setSrc(hit);
         return;
       }
       setSrc(null);
       ...
     ```
  2. In `HoverDetails.tsx:31`, add `useEffect(() => setDetails({}), [result.path]);`.

---

### 1.17. `.gitignore` / `.cursorignore` / `.babbageignore` Broken on Windows & macOS for Uppercase Patterns and Negated Directories (`!dir/`)
- **File & Lines**: `src-tauri/src/indexer/ignore.rs:84-100, 137-158, 184-203, 213-249`
- **Why it happens**:
  1. **Uppercase patterns never match on Windows/macOS (`ignore.rs:140-144` vs `213-249`)**: When `self.case_insensitive` is `true` (Windows and macOS), `is_ignored` lowercases every path component via `normalize_component` (`"Thumbs.db"` → `"thumbs.db"`, `"Build"` → `"build"`). However, `parse()` (`ignore.rs:213-249`) never lowercases `rule.pattern`, and `glob_match` compares characters case-sensitively. Consequently, any pattern with an uppercase letter (`.DS_Store`, `Thumbs.db`, `Desktop.ini`, `Build/`, `Pods/`, `*.PDF`) **never matches on Windows or macOS**!
  2. **Negated directory rules (`!src/`) ignore everything inside the directory (`ignore.rs:153-158, 184-188`)**: The parent prefix loop at line 154 calls `self.evaluate(rules, &prefix, true)`, which returns `rules.iter().any(|rule| self.rule_matches(rule, relative, is_dir))` **without checking `rule.negated`**. A rule `!src/` causes `evaluate` to return `true`, excluding `src/main.rs`!
  3. **`dir_only` rules (`logs/`) match nested files (`ignore.rs:191-194`)**: `if rule.dir_only && !is_dir && !relative.contains('/') { return false; }` allows a `dir_only` rule `logs/` to match a regular file at `sub/logs` (`is_dir = false`, `relative.contains('/') == true`).
- **Concrete Solution**:
  1. Pass `self.case_insensitive` into `parse(&raw, self.case_insensitive)` and lowercase `pattern` when `case_insensitive` is `true`.
  2. Evaluate ancestor prefixes across rule groups using `evaluate_last` (honoring `rule.negated`) instead of `self.evaluate`.
  3. Change line 191 of `src-tauri/src/indexer/ignore.rs` to `if rule.dir_only && !is_dir { return false; }`.

---

### 1.18. Query Lexer & Parser Bugs: Broken Unquoted `re:` With Parentheses, Ignored Negation on `-path:`/`-name:`/`-content:`/`-re:`, `path:` `char_bloom` Rejection, and Size Range Overflow
- **File & Lines**:
  - `src-tauri/src/query/lexer.rs:100-125`
  - `src-tauri/src/query/parser.rs:287-300, 404-419, 552`
  - `src-tauri/src/search/fuzzy.rs:9, 56-65, 106-108`
  - `src-tauri/src/search/mod.rs:1066-1068`
- **Why it happens**:
  1. **Unquoted `re:` strips parentheses (`lexer.rs:114`)**: `let trimmed_val = val.trim_matches(|c| c == '(' || c == ')').to_string();` runs *before* `if key.eq_ignore_ascii_case("re")`, turning `re:(?i)todo` into `?i)todo` and `re:foo(bar|baz)` into `foo(bar|baz` (invalid regexes). Both `re:` branches in `lexer.rs:101, 116` also ignore `negated` (`-re:pattern`).
  2. **Negated field queries become positive searches (`parser.rs:287-300, 404-419`)**: In `parse_query`, `-path:foo`, `-name:foo`, and `-content:foo` ignore `negated` and push positive `Term`s to `terms`.
  3. **`path:dirname` fails `char_bloom` prefilter (`search/fuzzy.rs:56-65`, `search/mod.rs:1066-1068`)**: `entry.char_bloom` is computed only from `entry.name` (the basename, e.g. `"mod.rs"`). `path:search` pushes `TermField::Path` to `fuzzy_terms`, where `match_path = trimmed.contains('/') || trimmed.contains('\\')` is `false` for `"search"`. `score_entry` (`fuzzy.rs:56`) then checks `(entry.char_bloom & query_mask) != query_mask` against `"mod.rs"`, rejecting `/repo/src/search/mod.rs`! Also `compute_char_mask` (`fuzzy.rs:9`) uses `ch.to_ascii_lowercase()` instead of Unicode `ch.to_lowercase()`, breaking case-insensitive `char_bloom` for non-ASCII letters (`É`/`é`, `Б`/`б`).
  4. **Integer overflow in `parse_size_range` (`parser.rs:552`)**: `Some(size + 1024)` overflows `u64` if `size > u64::MAX - 1024`.
- **Concrete Solution**:
  1. Do not strip `'('` or `')'` when `key.eq_ignore_ascii_case("re")` in `lexer.rs:114`, and carry `negated` on `Token::Regex`.
  2. In `parser.rs:287-300, 404-419`, emit `Filter::Not(Box::new(Filter::Path(value)))` / `Filter::Not(Box::new(Filter::Name(value)))` when `negated` is `true`, emit `Filter::Path(value)` for positive `path:<value>`, and use `size.saturating_add(1024)` at `parser.rs:552`.
  3. In `search/fuzzy.rs:9`, use `for lower in ch.to_lowercase()` when building `compute_char_mask`.

---

### 1.19. Arena Self-Cycle & Orphaned Nodes on Directory Conflict Removal in `MetadataStore::remove` / `rename_prefix`, Plus Unupdated Extension & Leaked `visual_units`
- **File & Lines**:
  - `src-tauri/src/indexer/metadata.rs:648-706, 758-765, 777-800`
  - `src-tauri/src/indexer/metadata_db.rs:330-369, 410, 434-457`
  - `src-tauri/src/indexer/mod.rs:1049-1056, 1286-1290`
- **Why it happens**:
  1. **Self-cycle in `MetadataStore::remove` (`metadata.rs:665-700, 758-765`)**: `scan_rename` collects `conflicts` in arena index order (parent directories before children) and calls `self.remove(&path)` in forward order. When a conflicting directory at `idx` is removed and its child happens to be at `last_idx`, `self.arena.nodes[idx] = last_node` moves the child into `idx` with `node.parent == idx` (a self-cycle!). Subsequent `self.remove(&child_path)` cannot find the child, leaving a corrupted self-looping node in `arena.nodes`.
  2. **Stale extension on file rename (`metadata.rs:777-800`, `metadata_db.rs:434-442`)**: `rename_prefix` updates `name_off`, `name_len`, and `char_bloom`, but never updates `ext_id` in `Node` or `extension` in SQLite when a file is renamed (e.g. `draft.tmp` → `report.pdf`).
  3. **Leaked/deleted `visual_units` (`metadata_db.rs:330-369`, `indexer/mod.rs:1049-1056`)**: `MetadataDb::remove`, `remove_many`, `rename_prefix`, and `clear` never delete from `visual_units`; conversely, `indexer/mod.rs:1049` deletes `visual_units` before incremental text re-extraction even when `phash` is non-null (so `start_phash_backfill` never regenerates them).
- **Concrete Solution**:
  1. In `MetadataStore::rename_prefix` (`metadata.rs:758`), call `self.remove_many(&conflicts)` instead of looping `self.remove`, and in `MetadataStore::remove`, clear `node.parent = NO_PARENT` for any child whose `node.parent == idx`.
  2. Recompute `ext_id` / `extension` in `MetadataStore::rename_prefix` and `MetadataDb::rename_prefix`.
  3. Add `DELETE FROM visual_units` to `MetadataDb::{remove, remove_many, rename_prefix, clear}` and only delete `visual_units` during crawl when `job.phash_needs_refresh` is `true`.

---

### 1.20. UTF-8 Byte-Offset Mismatch After `.to_lowercase()` in Both `content.rs` and `agent/mcp.rs`
- **File & Lines**:
  - `src-tauri/src/indexer/content.rs:1250-1293`
  - `src-tauri/src/agent/mcp.rs:1034-1068`
- **Why it happens**:
  Both `extract_snippet_from_text_cached` (`content.rs:1264`) and `read_snippet_window` (`mcp.rs:1044`) compute `let lower = text.to_lowercase();` and use byte offsets from `lower.find(...)` to slice `text`. Characters whose UTF-8 byte length changes under `to_lowercase()` (such as `'İ'` 2 bytes → `"i\u{307}"` 3 bytes, `'ẞ'` 3 bytes → `'ß'` 2 bytes, `'K'` 3 bytes → `'k'` 1 byte) shift all subsequent byte offsets, causing `safe_start..safe_end` to misalign or panic if `safe_start > safe_end`. In addition, `mcp.rs:1050` subtracts a *character* count (`cap_chars / 4`) from a *byte* index (`match_byte`).
- **Concrete Solution**:
  Locate matches directly on `text` using a case-insensitive regex (`regex::RegexBuilder::new(&regex::escape(q)).case_insensitive(true).build()`) or char-index mapping so byte offsets are guaranteed to be valid UTF-8 boundaries in `text`.

---

### 1.21. Destructive Startup Operations Before Single-Instance Lock, Missing Windows Retry in `apply_pending_rebuild_in`, Tauri Config Resource Wipe, and Read-Only `models_dir()` Fallback
- **File & Lines**:
  - `src-tauri/src/lib.rs:276-290, 336-371`
  - `src-tauri/tauri.onnxruntime.windows.conf.json:1-5`
  - `src-tauri/tauri.onnxruntime.linux.conf.json:1-5`
  - `src-tauri/tauri.onnxruntime.macos.conf.json:1-5`
  - `src-tauri/src/models.rs:363-381, 448-464, 936-953`
- **Why it happens**:
  1. **Destructive startup steps run before instance lock (`lib.rs:336-371`)**: `lib.rs` executes `ensure_bundled_assets()`, `maybe_apply_pending_rebuild()`, `maybe_apply_pending_reset()`, and `cleanup_stale_artifacts()` *before* calling `acquire_instance_lock_or_exit()` at line 371. Launching a second instance (or `babbage --toggle`) while Babbage is running can delete or mutate files under the active instance.
  2. **`apply_pending_rebuild_in` lacks `reset_with_retries` (`lib.rs:276-290`)**: Unlike `apply_pending_reset_in` (`lib.rs:248-262`), `apply_pending_rebuild_in` calls `remove_index_Entry` once without `reset_with_retries`. On Windows, when `app.restart()` spawns the new process before the old process releases its Tantivy/SQLite memory maps, `fs::remove_dir_all(index)` fails with `ERROR_SHARING_VIOLATION` after deleting `rebuild.pending`.
  3. **Platform ONNX config overlays wipe `bundle.resources` (`tauri.onnxruntime.*.conf.json:1-5`)**: In Tauri v2, JSON arrays in `--config` overlays replace the base array in `tauri.conf.json` (`"resources": ["resources/**/*"]`), so `tauri.onnxruntime.{windows,linux,macos}.conf.json` replace `"resources"` with only the ONNX Runtime library, dropping bundled `resources/models/` from release installers.
  4. **`models_dir()` falls back to read-only install directory (`models.rs:363-381, 448-464, 936-953`)**: When bundled models exist at `<exe_dir>/resources/models`, `models_dir()` returns that read-only directory (`C:\Program Files\...` or `/usr/lib/...`). `smart_dir()` (`models_dir().join("smart")`), `visual_dir()`, and `delete_all()` then fail with `PermissionDenied` when downloading or deleting packs.
- **Concrete Solution**:
  1. Move `acquire_instance_lock_or_exit()` in `src-tauri/src/lib.rs` before `ensure_bundled_assets()`, `maybe_apply_pending_rebuild()`, `maybe_apply_pending_reset()`, and `cleanup_stale_artifacts()`.
  2. Wrap `remove_index_entry` in `apply_pending_rebuild_in` (`lib.rs:284-286`) with `reset_with_retries`.
  3. Include `"resources/**/*"` alongside the ONNX Runtime library in `src-tauri/tauri.onnxruntime.{windows,linux,macos}.conf.json`.
  4. Have `models_dir()`, `smart_dir()`, and `visual_dir()` always use `BabbageConfig::config_dir().join("models")` for writable storage and only read from `bundled_models_candidates()` as a read-only fallback.

---

### 1.22. Settings "Save & Reindex" Overwrites `ocr_engine`/`app_mode`, Missing `reembed_all` Command, and `crashlog::last_failure()` Backtrace Bug
- **File & Lines**:
  - `src/components/SettingsModal.tsx:133-139, 977-985`
  - `src-tauri/src/crashlog.rs:230-251`
- **Why it happens**:
  1. In `SettingsModal.tsx:134-138`, clicking **Save & reindex** calls `invoke("complete_setup", { dirs, shortcut, ocrEnabled, maxResults })` without passing `ocrEngine: config.ocr_engine` or `appMode: config.app_mode`, silently clobbering any unsaved change to `ocr_engine` or `app_mode`!
  2. In `SettingsModal.tsx:980`, the **Re-embed all** button calls `invoke("reembed_all")`, which is not registered in `src-tauri/src/lib.rs` or `commands.rs` (it always rejects and shows `"Queued…"` as a false facade).
  3. In `src-tauri/src/crashlog.rs:230-251`, `is_failure_line` returns `true` for any non-empty line that does not start with `[...] INFO `. When a panic logs a multi-line stack trace (`RUST_BACKTRACE=1`), `last_failure()` iterates in reverse and returns the final backtrace frame (`"at /rustc/.../backtrace.rs:123"`) instead of the `[timestamp] PANIC ...` header line.
- **Concrete Solution**:
  1. Replace `invoke("complete_setup", ...)` in `SettingsModal.tsx:134-138` with `invoke("reindex")` (since `save_config` already ran on line 132 and persisted all fields, or pass `ocrEngine: config.ocr_engine, appMode: config.app_mode`).
  2. Register a `reembed_all` Tauri command in `commands.rs` and `lib.rs` that calls `indexer.on_semantic_models_ready()`.
  3. In `crashlog::is_failure_line` (`crashlog.rs:230-236`), require the line to start with `[` and contain `] WARN `, `] ERROR `, or `] PANIC `.

---

## 2. Architecture & Performance

### 2.1. `preprocess_region` Clones the Entire Full-Resolution Page Buffer for Every Single Text Box in Parallel
- **File & Lines**:
  - `src-tauri/vendor/paddleocr_rs_onnx/src/rec.rs:191-206`
  - `src-tauri/vendor/paddleocr_rs_onnx/src/lib.rs:779-805`
- **Why it happens**:
  In `OcrEngine::recognize_all` (`lib.rs:780`), `regions.par_iter()` calls `rec::preprocess_region(image, region, ...)` across all Rayon threads *before* acquiring a `rec_sessions` permit. Inside `preprocess_region` (`rec.rs:191`):
  ```rust
  let rgb = image.to_rgb8();
  let (img_w, img_h) = (rgb.width(), rgb.height());
  ```
  If `image` is `DynamicImage::ImageRgba8` (which `image::open` produces for all PNG screenshots and `pdf_image.rs` produces for PDF bitmaps), `image.to_rgb8()` allocates and converts a brand-new full-page `RgbImage` buffer **for every single text box on the page simultaneously**! On a 4K screenshot (25 MB) or 50 MP scan (150 MB) with 100 text boxes across 16 Rayon threads, this allocates **2.5 GB to 15 GB of transient buffers** outside `decode_budget()`.
- **Concrete Solution**:
  Convert `let rgb = image.to_rgb8();` **once** at the start of `OcrEngine::recognize_all` (`lib.rs:771`) and pass `&RgbImage` into `preprocess_region` (or sample directly from `image` within the axis-aligned bounding box of `region.bbox`).

---

### 2.2. Severe ONNX Runtime Thread Oversubscription, Unbounded Memory Arenas, and 28 Duplicate Indra Sessions
- **File & Lines**:
  - `src-tauri/vendor/paddleocr_rs_onnx/src/lib.rs:367-402, 524-592`
  - `src-tauri/src/ocr_indra/mod.rs:180-232`
  - `src-tauri/src/embed/smart.rs:91-95`
- **Why it happens**:
  1. `paddleocr_rs_onnx` (`lib.rs:536-573`) and `smart.rs:91-95` create ONNX Runtime sessions without calling `.with_intra_threads(...)`, `.with_inter_threads(1)`, or `.with_memory_pattern(false)` / disabling the CPU arena allocator. By default, each ORT session spawns `num_physical_cores` intra-op threads and retains a growing memory arena that is never released to the OS. With `rec_pool_size = 4` concurrent sessions inside Rayon's `par_iter`, ORT spawns up to $4 \times 16 = 64$ competing threads.
  2. `ocr_indra` (`ocr_indra/mod.rs:180-200`) creates up to `pool_size = 4` complete `Engine` instances, each loading 7 separate ONNX models (`28` total sessions, duplicating ~110 MB of weights in RAM), and `recognize_with_timeout` (`ocr_indra/mod.rs:218-232`) polls `slot.try_lock()` in a 10 ms sleep spin-loop instead of using a `Condvar`.
- **Concrete Solution**:
  1. Call `.with_intra_threads(2)?.with_inter_threads(1)?` on `det_session` and `.with_intra_threads(1)?.with_inter_threads(1)?` on `rec_sessions`, `cls_session`, and `SmartEmbedder` session builders.
  2. Share single `Session` instances in `ocr_indra::Engine` (since `ort::Session::run` takes `&self` and is `Sync`) gated by a `Semaphore`/`Condvar` instead of duplicating `Engine` 4 times and spin-polling.

---

### 2.3. `ocr_indra::skew::estimate_skew` Allocates 84 Full-Image Buffers Per Page
- **File & Lines**: `src-tauri/src/ocr_indra/skew.rs:17-114`
- **Why it happens**:
  `estimate_skew` (`skew.rs:17-41`) runs 61 coarse angles (`-15.0°..=+15.0°`) + 21 fine angles + 2 verification calls = **84 calls** to `rotate(ink, degrees, false)` per pass (168 calls when rotated 90°). Each `rotate()` (`skew.rs:64-114`) allocates a new `w * h` `Gray` buffer and a `Vec<f64>` of height `h`, and computes `f64` trigonometry for every pixel just to sum horizontal row projections in `row_sharpness`. Additionally, `rotate(gray, 0.0, ...)` does not fast-path `degrees.abs() < 1e-9`.
- **Concrete Solution**:
  1. Add `if degrees.abs() < 1e-6 { return img.clone(); }` at the top of `rotate` (`skew.rs:65`).
  2. In `row_sharpness`, accumulate projected row sums directly into a single reusable `Vec<u32>` of length `h` by iterating only over non-zero ink pixels (`val > 0`) at $y' = \text{round}((x - cx)\sin\theta + (y - cy)\cos\theta + cy)$ without allocating a rotated image buffer.

---

### 2.4. `util::priority::apply` Demotes the Entire GUI Process & Interactive Search Threads on Windows and macOS
- **File & Lines**: `src-tauri/src/util/priority.rs:35-88`
- **Why it happens**:
  When `crawl_mode` is `"balanced"` (the default) or `"eco"`, `start_indexing` calls `crate::util::priority::apply(true)`:
  - On Windows (`priority.rs:86`), `SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS)` demotes the **entire process**—including the main UI thread, Tauri IPC handlers, and `query_pool` search threads!
  - On macOS (`priority.rs:66`), `setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG)` puts the **entire process** into Darwin background state (throttling CPU, disk I/O, and QoS for all threads, including interactive search).
  - On Linux (`priority.rs:50`), `sched_setscheduler(0, SCHED_IDLE, ...)` only demotes the single `start_indexing` coordinator thread, leaving the `crawl_pool` and `extraction_pool` worker threads at normal priority!
- **Concrete Solution**:
  Apply thread-level background priority (`SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL)` on Windows, `setpriority(PRIO_DARWIN_THREAD, 0, PRIO_DARWIN_BG)` on macOS, and `sched_setscheduler(0, SCHED_IDLE, ...)` on Linux) inside the `.start_handler` of `crawl_pool` (`util/pools.rs:65`) and `extraction_pool` workers rather than mutating process-wide priority.

---

### 2.5. `EmbeddingQueue` Holds `UnitStore` Write Lock Across 320 Synchronous SQLite Inserts & Marks Nodes `indexed_gen = 1` Before Writing Units
- **File & Lines**: `src-tauri/src/embed/queue.rs:244-268`
- **Why it happens**:
  In `EmbeddingQueue::start`:
  1. Line 244 calls `db.update_node_indexed_gen(item.node_id, 1, item.content_hash)` *before* the node's semantic units are inserted at lines 254–260. If the app exits or crashes between line 244 and line 260, those nodes remain marked `indexed_gen = 1` with zero semantic units in SQLite and are never re-embedded on next launch.
  2. Lines 251–268 acquire `let mut store = self.unit_store.write();` and hold that write lock while executing `db.delete_units_for_nodes` and up to 320 individual auto-commit SQLite `db.insert_unit(...)` statements on disk, blocking all concurrent semantic search queries (`unit_store.read()`) for the duration of the disk I/O.
- **Concrete Solution**:
  Execute `delete_units_for_nodes`, all `insert_unit` calls, and `update_node_indexed_gen` inside a single SQLite `BEGIN IMMEDIATE ... COMMIT` transaction **before** acquiring `self.unit_store.write()`, then hold `self.unit_store.write()` only for the in-memory vector updates.

---

### 2.6. Unconditional `DirectoryTrie` & `HashMap<PathBuf, usize>` Rebuild on Every Query and `browse_mode` String Allocations
- **File & Lines**:
  - `src-tauri/src/search/mod.rs:881-900, 1681-1690`
  - `src-tauri/src/indexer/metadata.rs:908-933`
  - `src-tauri/src/query/scope_trie.rs:114-140`
- **Why it happens**:
  1. `search_response_on_pool` (`search/mod.rs:881`) calls `let trie = metadata.get_directory_trie();` on *every* query before checking `let needs_trie = query_needs_trie(effective_query);` at line 891. Whenever `dir_generation` advances (e.g., during indexing or watcher updates), `get_directory_trie()` materializes every node in the arena and builds a full `HashMap<PathBuf, usize>` even for plain queries without `in:` or `/`.
  2. When `needs_trie` is `true` (`search/mod.rs:892-897`), it clones all $N$ `PathBuf`s into a brand-new `HashMap<PathBuf, usize>` on *every keystroke*, even though `MetadataStore` already maintains `self.path_index`.
  3. Inside `DirectoryTrie::resolve` (`scope_trie.rs:114-140`), a linear scan over all `entries` checks `if entry_path_str.ends_with(target_clean)` *before* consulting the exact trie (`self.find_exact`). Not only is this $O(N)$ with two `String` allocations per entry, but because `ends_with` does not check for a `'/'` boundary, `in:src` matches both `/repo/src` and `/repo/generated-src` and returns `TrieMatchResult::Ambiguous` instead of the exact match `/repo/src`!
  4. In `finalize_score` (`search/mod.rs:1681`), `let name_lower = entry.name.to_lowercase();` is allocated unconditionally for every candidate even when `search_text_lower.is_empty()` (`browse_mode`).
- **Concrete Solution**:
  1. Move `metadata.get_directory_trie()` inside `if needs_trie || !parsed.scopes.is_empty()` and look up path indices via `metadata.get_index(path)` instead of cloning a `HashMap<PathBuf, usize>` per query.
  2. In `DirectoryTrie::resolve` (`scope_trie.rs:114`), only run the suffix scan when `target_clean.contains('/')` and require a `'/'` boundary (`entry_path_str.ends_with(&format!("/{target_clean}"))`).
  3. Guard `let name_lower = entry.name.to_lowercase();` in `finalize_score` (`search/mod.rs:1681`) with `if !search_text_lower.is_empty()`.

---

### 2.7. Blocking Socket I/O, SQLite Queries, and Full Corpus Sorts on Tauri Async Runtime Threads
- **File & Lines**: `src-tauri/src/commands.rs:387-535, 570-573, 945-968, 1053-1088, 1206-1290, 1358-1367, 1686-1688`
- **Why it happens**:
  In Tauri v2, `#[tauri::command] pub async fn` runs directly on Tokio worker threads unless offloaded with `spawn_blocking`. Several commands perform heavy synchronous I/O directly on the async executor:
  - `run_doctor` (`commands.rs:945`): runs `doctor::run`, which executes Tantivy test queries, SQLite integrity checks, and spawns `tesseract --version`.
  - `find_duplicates` (`commands.rs:1053`) and `find_similar_images` (`commands.rs:1070`): run full-table SQLite scans, union-find clustering over all images, and image decoding.
  - `get_stats` (`commands.rs:387`), `get_indexing_status` (`commands.rs:570`), `get_home_data` (`commands.rs:1285`), and `get_scope_suggestions` (`commands.rs:1358`): perform synchronous daemon socket RPCs, recursive `dir_size_bytes` directory walks over `index/` and `models/`, and full arena snapshots.
  - Furthermore, `home_data_for` (`commands.rs:1206-1240`) materializes and sorts **every non-directory `FileEntry` in the entire corpus** (`recent.sort_by(...)`) just to keep the top 12 (`recent.truncate(HOME_RECENT_FILES_LIMIT)`).
- **Concrete Solution**:
  Wrap the bodies of `run_doctor`, `find_duplicates`, `find_similar_images`, `get_stats`, `get_home_data`, `get_scope_suggestions`, `read_file_content`, and `open_file` in `tauri::async_runtime::spawn_blocking`, and replace the full-vector sort in `home_data_for` (`commands.rs:1224-1240`) with a bounded `BinaryHeap` of capacity `HOME_RECENT_FILES_LIMIT` (`12`).

---

### 2.8. State Desynchronization Between GUI and Daemon (`set_theme`, `run_setup_again`, `get_home_data`, `doctor::run`, and Tray Status)
- **File & Lines**:
  - `src-tauri/src/commands.rs:538-567, 1115-1121, 1285-1290`
  - `src-tauri/src/doctor.rs:621-650, 1008-1035`
  - `src-tauri/src/lib.rs:638-657`
- **Why it happens**:
  When `daemon.is_enabled()` is `true` on Unix:
  1. `set_theme` (`commands.rs:538-567`) and `run_setup_again` (`commands.rs:1115-1121`) save `config.json` from the GUI process without notifying the daemon; the next time the daemon saves its in-memory config, it overwrites the GUI's changes.
  2. `get_home_data` (`commands.rs:1285-1290`) proxies to the daemon's `home_data`, which reads the daemon process's `RECENT_QUERIES` (`vec![]`), while `record_recent_query` (`commands.rs:254`) records searches in the GUI process's `RECENT_QUERIES`—so Recent Searches on the home screen are always empty when the daemon is active!
  3. `doctor::run` (`doctor.rs:621-650, 1008-1035`) and `repair_start_watcher` inspect the GUI's idle read-only `IndexerManager` (`watcher_running() == false`), always reporting a false `"Watcher is not running"` warning whose repair button fails because the daemon holds `babbage.lock`.
  4. The tray icon status loop (`lib.rs:638-657`) only reads `state.indexer.get_status()` (the GUI's idle read-only indexer) instead of polling `state.daemon.get_status()`.
- **Concrete Solution**:
  1. In `get_home_data` (`commands.rs:1286`), populate `recent_queries: recent_queries()` in the GUI process before returning the daemon's response.
  2. In `doctor::run` (`doctor.rs:621`) and the tray status loop (`lib.rs:640`), query `daemon::request("health", ...)` / `state.daemon.get_status()` when `daemon.is_enabled()` is `true`.

---

### 2.9. Frontend Render & IPC Bottlenecks: Defeated `FileRow` Memoization, Snippet Observer Thrashing, and Unthrottled Thumbnail IPC Storm
- **File & Lines**:
  - `src/App.tsx:402-512, 1309-1335`
  - `src/components/FileRow.tsx:21-93`
  - `src/components/ResultItem.tsx:31-113`
  - `src/lib/thumbnails.ts:15, 58-113`
- **Why it happens**:
  1. **`FileRow` memoization defeated (`src/App.tsx:1311-1317`, `src/components/FileRow.tsx:21`)**: `FileRow` is wrapped in `memo(...)` without a custom comparator, and `App.tsx:1312` passes a fresh inline object literal `item={{ path: res.path, name: res.name, ... }}` and inline closures (`onClick`, `onDoubleClick`, `onContextMenu`, `onShowDetails`) on every render, forcing every visible `FileRow` to re-render on any state change in `App` ( unlike `ResultItem.tsx:96-112`, which has a custom `memo` comparator). Even in `ResultItem.tsx:107`, `(prev.result.snippet ?? null) === (next.result.snippet ?? null)` is checked even though `ResultItem` never renders `result.snippet`!
  2. **Snippet observer recreated on every snippet batch (`src/App.tsx:465-512`)**: The `useEffect` setting up `IntersectionObserver` + `MutationObserver` depends on `[results, query]`. Every time a batch of snippets returns and calls `setResults(...)` (`App.tsx:444`), `results` gets a new array reference, tearing down and recreating both observers and re-observing all DOM nodes in a tight cascade.
  3. **Unthrottled thumbnail IPC storm (`src/lib/thumbnails.ts:88-111`)**: `THUMBNAIL_LAZY_LIMIT` (`thumbnails.ts:15`) is exported but never used anywhere in `src/`. During fast scrolling through `VirtualGrid`, every newly mounted `ResultItem` or `FileRow` immediately fires `invoke("get_thumbnail_data", { path })` on mount before its cleanup can cancel it, flooding the backend with dozens of concurrent image decodes and base64 transfers.
- **Concrete Solution**:
  1. Add a custom `memo` comparator to `FileRow.tsx` (comparing `item.path`, `item.modified`, `item.size`, `item.snippet`, `selected`, `query`, `showThumbnails`), and remove the unused `snippet` comparison from `ResultItem.tsx:107`.
  2. Store `displayResults` in a ref read by the `IntersectionObserver` callback in `src/App.tsx:465-512` so the observers only re-initialize when `query` or facet filters change, not when snippets are patched in.
  3. Add a 30–40 ms mount debounce and a client-side concurrency queue (e.g., max 6 concurrent `invoke("get_thumbnail_data")` calls) in `src/lib/thumbnails.ts:58-111`.

---

## 3. UI/UX & Accessibility Issues

### 3.1. Summon Shortcut (`Alt+Space`) and Tray Toggle Hide an Unfocused or Minimized Window Instead of Focusing It
- **File & Lines**: `src-tauri/src/main_window.rs:287-294`
- **Why it happens**:
  In `main_window::toggle`:
  ```rust
  pub fn toggle<R: Runtime>(app: &AppHandle<R>) {
      let visible = app
          .get_webview_window(MAIN_LABEL)
          .and_then(|w| w.is_visible().ok())
          .unwrap_or(false);
      if visible {
          hide(app);
      } else {
          show(app);
      }
  }
  ```
  `toggle` only checks `w.is_visible()`, ignoring `w.is_focused()` and `w.is_minimized()`. In `"app"` mode (or on Linux/Windows when a window is open behind another application or minimized), `w.is_visible()` is `true`. Pressing `Alt+Space` or clicking **Show / Hide** in the tray destroys/hides the backgrounded window instead of bringing it to the foreground; the user has to press `Alt+Space` twice.
- **Concrete Solution**:
  In `src-tauri/src/main_window.rs:287-294`, only hide when the window is visible, not minimized, and currently focused:
  ```rust
  let should_hide = app
      .get_webview_window(MAIN_LABEL)
      .map(|w| {
          w.is_visible().unwrap_or(false)
              && !w.is_minimized().unwrap_or(false)
              && w.is_focused().unwrap_or(false)
      })
      .unwrap_or(false);
  if should_hide {
      hide(app);
  } else {
      show(app);
  }
  ```

---

### 3.2. Live Window Mode Switch (`apply_mode`) Leaves Mismatched `transparent` State & Resets Window Size/Position
- **File & Lines**: `src-tauri/src/main_window.rs:154-172, 207-221, 226-265`
- **Why it happens**:
  1. `WebviewWindowBuilder::transparent(...)` can only be set at window creation time (`transparent(!decorated)` at line 161). When the user finishes first-run setup in `"launcher"` mode (the default) or switches between `"launcher"` and `"app"` mode in Settings, `apply_mode` (`main_window.rs:207-221`) toggles `set_decorations`, `set_always_on_top`, and `set_skip_taskbar` on the existing window without recreating it. Because setup started with `decorated = true` (`transparent(false)`), the launcher window has an opaque rectangular surface causing dark/white corner artifacts around its rounded CSS border!
  2. When `hide()` destroys the window (`main_window.rs:277`), `show()` (`main_window.rs:154`) always recreates it at hardcoded `920 × 640` centered on the primary monitor, losing the user's window size and monitor position in `"app"` mode.
- **Concrete Solution**:
  When switching between `"app"` and `"launcher"` mode in `apply_mode`, recreate the window if the target `transparent` state differs from creation, and save/restore the window's inner size and position before `w.destroy()` in `"app"` mode.

---

### 3.3. Non-Existent Tailwind Utility Classes `hover:bg-surface-2` and `bg-surface`
- **File & Lines**:
  - `src/components/KeyHintStrip.tsx:70, 81`
  - `src/components/SettingsModal.tsx:412`
  - `src/App.css:112-143`
- **Why it happens**:
  The `@theme` block in `src/App.css:112-143` defines `--color-canvas`, `--color-elevated`, `--color-input`, `--color-accent`, `--color-accent-soft`, etc., but **never defines `--color-surface` or `--color-surface-2`**. As a result:
  - `hover:bg-surface-2` on the Settings and Help (`?`) buttons in `KeyHintStrip.tsx:70, 81` generates no CSS, so those buttons have no hover background state.
  - `bg-surface` on the Settings sidebar container in `SettingsModal.tsx:412` generates no CSS (rendering transparent instead of a surface background).
- **Concrete Solution**:
  Replace `hover:bg-surface-2` in `src/components/KeyHintStrip.tsx:70, 81` with `hover:bg-accent-soft`, and replace `bg-surface` in `src/components/SettingsModal.tsx:412` with `bg-elevated` (or define `--color-surface` and `--color-surface-2` in `src/App.css:112-143`).

---

### 3.4. Missing Modal Dialog Semantics (`role="dialog"`, `aria-modal`, Focus Trapping) & Keyboard Navigation Gaps in `ContextMenu`, `ResultControls`, and `VirtualGrid`
- **File & Lines**:
  - `src/components/HelpDialog.tsx:51-60`
  - `src/components/DuplicatesModal.tsx:72`
  - `src/components/ContextMenu.tsx:24-78`
  - `src/components/ResultControls.tsx:259-301`
  - `src/components/VirtualGrid.tsx:100-142`
  - `src/components/FileRow.tsx:52-55`
  - `src/components/ResultItem.tsx:53-56`
- **Why it happens**:
  1. `HelpDialog.tsx:51-60` and `DuplicatesModal.tsx:72` lack `role="dialog"` and `aria-modal="true"`; `HelpDialog` also lacks backdrop click-to-close and an `aria-label` on its `×` close button (`HelpDialog.tsx:56-59`). None of the modals (`SettingsModal`, `PreviewDialog`, `DetailsDialog`, `DuplicatesModal`, `HelpDialog`) trap `Tab` focus inside the modal, allowing `Tab` to move focus to hidden search results behind the overlay.
  2. `ContextMenu.tsx:24-78` never moves focus into the menu on open and has no `ArrowDown`/`ArrowUp`/`Home`/`End` keyboard navigation across its `role="menuitem"` buttons.
  3. In `ResultControls.tsx:259-301`, the Saved Searches dropdown (`role="menu"`) has no keyboard handler (unlike the Sort dropdown at lines 102–134) and uses invalid `role="listitem"` children inside `role="menu"`.
  4. `VirtualGrid.tsx:135` gives each row wrapper `role="row"` inside a plain `<div>` with no `role="grid"`/`role="listbox"`, while `FileRow.tsx:53` puts `role="row"` and `aria-selected` on a `<button>` and `ResultItem.tsx:55` puts `aria-selected` on a plain `<button>` (invalid ARIA).
- **Concrete Solution**:
  1. Add `role="dialog" aria-modal="true"`, backdrop click dismissal, `aria-label={t("actions.close")}`, and a shared focus-trap hook to `HelpDialog`, `DuplicatesModal`, `DetailsDialog`, `PreviewDialog`, and `SettingsModal`.
  2. Auto-focus the first menu item in `ContextMenu.tsx` on mount and handle `ArrowDown`/`ArrowUp`/`Enter`/`Escape`.
  3. Add keyboard navigation to the Saved Searches menu in `ResultControls.tsx:259-301` and use `role="menuitem"`.
  4. Give `VirtualGrid`'s scroll container `role="listbox"` and each `FileRow` / `ResultItem` `role="option"` (where `aria-selected` is valid).

---

### 3.5. Windows Path Separator Bug in `FoldersSection`, Malformed `file://` Drag URI on Windows, Missing `"Visual"` Match Label, Untranslated `AboutSection`, and Unblocked `Esc` in `SetupModal`
- **File & Lines**:
  - `src/components/SettingsModal.tsx:558, 1580-1589`
  - `src/components/FileRow.tsx:66`
  - `src/components/ResultItem.tsx:67`
  - `src/components/DetailsDialog.tsx:76-93`
  - `src/components/SetupModal.tsx:143-176`
  - `src/locales/{en,ru,zh}/details.json:9-15`
- **Why it happens**:
  1. **Windows watched-folder display broken (`SettingsModal.tsx:558`)**: `FoldersSection` computes `const parts = dir.split("/").filter(Boolean);`. On Windows (`C:\Users\Nihil\Documents`), `dir.split("/")` does not split on `\`, so `name` is the entire path and `parent` is `""`.
  2. **Invalid `text/uri-list` on Windows drag-and-drop (`FileRow.tsx:66`, `ResultItem.tsx:67`)**: `e.dataTransfer.setData("text/uri-list", `file://${item.path}`)` produces `file://C:\Users\...` (which is parsed as hostname `C:` rather than local drive `C:/`).
  3. **Missing `"Visual"` case in `DetailsDialog.tsx:76-93`**: `matchTypeLabel` handles `"FileName"`, `"Content"`, `"ContentFolded"`, `"Semantic"`, and `"Both"`, but omits `"Visual"` (which `MatchType` in `src/types.ts:1` includes), rendering raw untranslated `"Visual"`.
  4. **Hardcoded English strings in `AboutSection` (`SettingsModal.tsx:1580-1589`)**: `"Local, instant desktop & agent search."` and `"Built for speed and privacy. Runs 100% locally with zero telemetry or cloud dependencies."` are hardcoded in English instead of using `t(...)`.
  5. **Shortcut capture in `SetupModal` does not set `document.body.dataset.ksCapture` (`SetupModal.tsx:143-176`)**: Unlike `SettingsModal.tsx:450-455`, `SetupModal` never sets `document.body.dataset.ksCapture = "1"` while capturing a shortcut, so pressing `Escape` to cancel shortcut capture bubbles to `App.tsx`'s global keydown listener.
- **Concrete Solution**:
  1. Split watched directory paths in `SettingsModal.tsx:558` with `dir.split(/[\\/]/).filter(Boolean)`.
  2. Format drag URIs in `FileRow.tsx:66` and `ResultItem.tsx:67` with:
     ```ts
     const fileUri = path.startsWith("/") ? `file://${encodeURI(path)}` : `file:///${encodeURI(path.replace(/\\/g, "/"))}`;
     ```
  3. Add `case "Visual": return t("match.visual");` to `DetailsDialog.tsx:89` and `match.visual` to `src/locales/{en,ru,zh}/details.json`.
  4. Move the two hardcoded strings in `SettingsModal.tsx:1581-1585` into `locales/{en,ru,zh}/settings.json`, and set `document.body.dataset.ksCapture = "1"` during `capturingShortcut` in `SetupModal.tsx:143-176`.

---

### 3.6. Backend-Driven UI Bugs: Dropped `is_embedding`/`semantic_ready` Transitions, `content_indexed: 0` Display, Garbled Office XML Entities, and Empty `tessdata` False Negative
- **File & Lines**:
  - `src-tauri/src/indexer/mod.rs:472-480, 638-661, 1551`
  - `src-tauri/src/indexer/extract/office.rs:54-66, 123-148, 275-286, 337-345, 387-395`
  - `src-tauri/src/indexer/extract/image.rs:363-365`
- **Why it happens**:
  1. **`emit_status` drops final embedding transition (`indexer/mod.rs:638-661`)**: `emit_status` throttles events within 100 ms unless `(phase, is_indexing, paused)` changed. Because `is_embedding` and `semantic_ready` are omitted from the state-change tuple at line 644, when `EmbeddingQueue` finishes within 100 ms of a previous `"idle"` status emission, the final `is_embedding: false, semantic_ready: true` event is dropped and `IndexingBanner` stays stuck showing `"Building the semantic index"`.
  2. **`status.content_indexed` shows `0` after startup and incremental crawls (`indexer/mod.rs:472-480, 1551`)**: `spawn_loader` leaves `st.content_indexed = 0` at startup, and line 1551 sets `st.content_indexed = content_count` (the number of files extracted *in that crawl pass*, which is `0` when nothing changed) instead of `content_index.num_docs()`.
  3. **Office/EPUB/ODT XML entity splitting (`extract/office.rs:54-66, 123-148, 275-286, 337-345, 387-395`)**: `quick-xml` 0.42 emits `Event::GeneralRef` as a separate event between `Event::Text` fragments. Because `extract_docx`, `extract_xlsx` (worksheets), `extract_epub`, and `extract_odt` append `' '` after *every* `Event::Text` and `Event::GeneralRef`, `<w:t>don&apos;t</w:t>` is extracted as `"don ' t "` (splitting words and breaking search/snippets), while `extract_pptx` (`office.rs:123-148`) ignores `Event::GeneralRef` completely!
  4. **Empty `tessdata` directory hides working system Tesseract (`extract/image.rs:363-365`)**: `ocr_status` sets `TESSDATA_PREFIX` if `tessdata_dir().is_dir()` even when that directory contains no `.traineddata` files (whereas `tesseract_ocr` at line 750 checks `private_tessdata(&tessdata_dir())`), causing `tesseract --list-langs` to report 0 languages in Settings.
- **Concrete Solution**:
  1. Include `status.is_embedding` and `status.semantic_ready` in the `LAST` comparison tuple in `emit_status` (`indexer/mod.rs:644`), and set `st.content_indexed = content_index.read().as_ref().map(|c| c.num_docs() as usize).unwrap_or(content_count)` in `spawn_loader` and at crawl completion.
  2. In `extract/office.rs`, handle `Event::GeneralRef` in `extract_pptx` and only push `' '` on the closing tag (`Event::End` for `w:t`, `a:t`, `v`, `t`, `p`) rather than after each `Event::Text`/`Event::GeneralRef` fragment.
  3. In `extract/image.rs:363`, replace `if dir.is_dir()` with `if let Some(dir) = private_tessdata(&dir)`.

---

## 4. Code Quality & Cleanup

### 4.1. Complete Duplication (With Behavioral Divergence) of WordPiece Tokenizer Between `embed/table.rs` and `embed/wordpiece.rs`
- **File & Lines**:
  - `src-tauri/src/embed/table.rs:253-490`
  - `src-tauri/src/embed/wordpiece.rs:19-210`
- **Why it happens**:
  `table.rs` and `wordpiece.rs` each implement their own copy of `pre_tokenize`, `tokenize_word`, `token_ids`, `is_cjk`, `is_word_Connector`, `l2_normalize`, and `parse_tokenizer(tokenizer.json)`. The two copies have already drifted:
  - `wordpiece.rs:76-94` reads `model.unk_token`, `model.max_input_chars_per_word`, and `normalizer.lowercase` dynamically from `tokenizer.json`, whereas `table.rs:321-334` hardcodes `"[UNK]"` and always lowercases.
  - `table.rs:306` validates token IDs against `vocab_size`, whereas `wordpiece.rs` does not.
- **Concrete Solution**:
  Delete the duplicate tokenizer implementation from `src-tauri/src/embed/table.rs:253-490` and compose `WordPieceTokenizer` (from `src-tauri/src/embed/wordpiece.rs`) inside `StaticTable`, adding optional `vocab_size` bounds checking to `WordPieceTokenizer::from_json_bytes`.

---

### 4.2. Broken Windows `cli_command()` Lookup & Triplicate Pushes in `agent/connect.rs`, Stray XML Tag & Duplicate Conditions in `config.rs` and `daemon.rs`
- **File & Lines**:
  - `src-tauri/src/agent/connect.rs:108-133, 182`
  - `src-tauri/src/config.rs:211, 273-274, 502`
  - `src-tauri/src/daemon.rs:50`
  - `src-tauri/src/indexer/metadata.rs:222-225`
- **Why it happens**:
  1. In `src-tauri/src/agent/connect.rs:112-125`, `candidates` pushes the exact same path 3 times in a row in two places:
     ```rust
     candidates.push(dir.join("babbage-cli"));
     candidates.push(dir.join("babbage-cli"));
     candidates.push(dir.join("babbage-cli"));
     ```
     and twice at lines 123–124 (`home.join(".local/bin/babbage-cli")`), while **never checking `"babbage-cli.exe"` on Windows**! Thus one-click MCP setup on Windows always falls back to bare `"babbage-cli"` even when `babbage-cli.exe` sits right next to `babbage.exe`.
  2. In `connect.rs:182`, `servers.get("babbage").or_else(|| servers.get("babbage"))` checks `"babbage"` twice instead of `.or_else(|| servers.get("Babbage"))`.
  3. `src-tauri/src/config.rs:211` has a stray `</arg_value>` artifact in a comment; `config.rs:273-274` checks `!ext.eq_ignore_ascii_case("ts") && !ext.eq_ignore_ascii_case("ts")` twice; `config.rs:502` checks `preset == "all" || preset == "all"` twice; `src-tauri/src/daemon.rs:50` checks `method == "get_status" || method == "get_status"` twice; and `src-tauri/src/indexer/metadata.rs:222-225` checks `self.arena.node(idx).content_pending()` twice.
- **Concrete Solution**:
  1. In `connect.rs:112-125`, deduplicate the candidate pushes and use `const CLI_BIN: &str = if cfg!(windows) { "babbage-cli.exe" } else { "babbage-cli" };`, and change line 182 to `.or_else(|| servers.get("Babbage"))`.
  2. Remove the duplicate boolean conditions in `config.rs:273-274, 502`, `daemon.rs:50`, and `metadata.rs:223-225`, and remove `</arg_value>` from `config.rs:211`.

---

### 4.3. Unintended `image` Feature Unification in `vendor/paddleocr_rs_onnx/Cargo.toml`, Unused `staticlib`/`cdylib` Crate Types, `cli/Cargo.toml` Version Drift, and 4x Duplicated Helpers
- **File & Lines**:
  - `src-tauri/vendor/paddleocr_rs_onnx/Cargo.toml:10-12, 52-63`
  - `src-tauri/Cargo.toml:11, 44`
  - `cli/Cargo.toml:3`
  - `src-tauri/src/commands.rs:577-598`
  - `src-tauri/src/doctor.rs:1076-1091`
  - `src-tauri/src/maintenance.rs:202-220`
  - `src-tauri/src/agent/mod.rs:892-907`
- **Why it happens**:
  1. `src-tauri/Cargo.toml:44` carefully sets `image = { version = "0.25", default-features = false, features = ["png", "jpeg", "gif", "webp", "bmp", "tiff"] }`, but `src-tauri/vendor/paddleocr_rs_onnx/Cargo.toml:62` declares `image = "0.25"` with default features enabled. Cargo feature unification compiles in `rayon` (inside `image`), `hdr`, `exr`, `qoi`, `farbfeld`, `pnm`, `dds`, and `tga` across the entire workspace.
  2. Both `src-tauri/Cargo.toml:11` and `vendor/paddleocr_rs_onnx/Cargo.toml:11` specify `crate-type = ["staticlib", "cdylib", "rlib"]`, linking redundant `.a` and `.dll`/`.so`/`.dylib` artifacts on every build.
  3. `cli/Cargo.toml:3` has `version = "1.0.15"` while `package.json`, `src-tauri/Cargo.toml`, and `tauri.conf.json` are at `1.0.16`.
  4. `dir_size_bytes` is duplicated in 4 files (`commands.rs:577`, `doctor.rs:1076`, `maintenance.rs:202`, `agent/mod.rs:892`), and 64-bit `fnv1a` is duplicated in 3 files (`extract/image.rs:640`, `preview.rs:207`, `models.rs:467`).
  5. `IndexerManager` (`src-tauri/src/indexer/mod.rs:174, 1081, 1716, 1720, 1766, 1886, 1983-1991, 2308, 2329`) stores `self.data_dir`, yet those 9 methods hardcode `BabbageConfig::config_dir()` / `BabbageConfig::index_dir()` instead of using `self.data_dir`.
- **Concrete Solution**:
  1. Set `default-features = false, features = ["png", "jpeg", "gif", "webp", "bmp", "tiff", "ico"]` on `image` in `src-tauri/vendor/paddleocr_rs_onnx/Cargo.toml:62`, and change `crate-type` to `["rlib"]` in `vendor/paddleocr_rs_onnx/Cargo.toml:11`.
  2. Bump `cli/Cargo.toml:3` to `version = "1.0.16"`.
  3. Extract `dir_size_bytes` and `fnv1a_64` into `src-tauri/src/util/`, and replace hardcoded `BabbageConfig::config_dir()` calls inside `IndexerManager` with `self.data_dir`.

---

### 4.4. Dead Frontend Modules (`updater.ts`, `version.ts`, `githubIssue.ts`, `FileTypeIcon.tsx`), Unused `home.json` Locale Namespace, Unreachable `gridColumnCount` DOM Fallback, and Stale `mockTauri.ts` Schema
- **File & Lines**:
  - `src/lib/updater.ts:1-65`
  - `src/lib/version.ts:1-17`
  - `src/lib/githubIssue.ts:1-31`
  - `src/components/FileTypeIcon.tsx:1-60`
  - `src/components/EmptyState.tsx:41-55`
  - `src/locales/{en,ru,zh}/home.json:1-14`
  - `src/i18n/config.ts:8, 17, 27, 37, 46`
  - `src/App.tsx:218-233`
  - `src/dev/mockTauri.ts:149-150`
- **Why it happens**:
  1. `src/lib/updater.ts`, `src/lib/version.ts`, `src/lib/githubIssue.ts`, and `src/components/FileTypeIcon.tsx` are never imported anywhere in `src/` (and `EmptyState` in `src/components/EmptyState.tsx:41-55` is never imported; only `InlineNotice` is used).
  2. `src/locales/{en,ru,zh}/home.json` is loaded and bundled into `i18n/config.ts`, but no component ever calls `useTranslation("home")` or references `home:*` keys.
  3. In `src/App.tsx:218-233`, `gridColumnCount` returns unconditionally on line 219 (`if (containerWidth && containerWidth > 0) return ...; return 4;`), making lines 220–232 (`document.querySelector(".ks-results-grid")` measurement) unreachable dead code.
  4. In `src/dev/mockTauri.ts:149-150`, the `get_stats` mock returns `{ total_files, content_indexed, is_indexing }` instead of the `SystemStats` schema (`src/types.ts:126-152`), causing `StatsSection` (`src/components/settings/stats.tsx`) to crash on `undefined` properties in browser dev mode.
- **Concrete Solution**:
  1. Delete unused `src/lib/updater.ts`, `src/lib/version.ts`, `src/lib/githubIssue.ts`, `src/components/FileTypeIcon.tsx`, the unused `EmptyState` export in `src/components/EmptyState.tsx`, and the unused `home` locale namespace.
  2. Remove the unreachable lines 220–232 in `src/App.tsx`, and update `get_stats` in `src/dev/mockTauri.ts:149-150` to return a valid `SystemStats` object.
