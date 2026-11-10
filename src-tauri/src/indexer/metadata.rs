use crate::indexer::arena::{IdIndex, NO_PARENT, Node, NodeArena, PathIndex};
use crate::indexer::metadata_db::MetadataDb;
use crate::indexer::walker::FileEntry;
use crate::query::scope_trie::DirectoryTrie;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub struct MetadataStore {
    arena: NodeArena,
    path_index: PathIndex,
    id_index: RwLock<Option<IdIndex>>,
    /// Parallel to `arena`: burst id per arena slot, 0 means unassigned.
    bursts: RwLock<Option<Vec<u64>>>,
    db: Option<Arc<MetadataDb>>,
    generation: AtomicU64,
    dir_generation: AtomicU64,
    usage: RwLock<HashMap<PathBuf, (u32, u64)>>,
    cached_trie: RwLock<Option<(u64, Arc<DirectoryTrie>)>>,
    /// Materialized entries for the current generation.
    ///
    /// Building one is several allocations per file: the path is rebuilt from
    /// the arena, a PathBuf is joined, the name is copied. Search needs the
    /// whole list on every query, so it is cached here, per store, next to the
    /// trie cache and invalidated the same way. Per store rather than global
    /// because more than one store can exist in a process (the daemon, a
    /// headless agent index, a test fixture), and a global cache would hand one
    /// store's rows to another.
    cached_entries: RwLock<Option<(u64, Arc<Vec<crate::indexer::walker::FileEntry>>)>>,
    /// Slots changed in place since the snapshot was built (flag flips, an
    /// existing file's size/mtime). These patch the cached snapshot instead
    /// of bumping `generation`, which would force a full re-materialisation
    /// of every entry on the next query: ~30 ms at 100k files, ~300 ms at 1M,
    /// repeatedly during a crawl.
    dirty_slots: parking_lot::Mutex<Vec<u32>>,
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataStore {
    pub fn without_db() -> Self {
        Self {
            arena: NodeArena::new(),
            path_index: PathIndex::new(),
            id_index: RwLock::new(None),
            bursts: RwLock::new(None),
            db: None,
            generation: AtomicU64::new(1),
            dir_generation: AtomicU64::new(1),
            usage: RwLock::new(HashMap::new()),
            cached_trie: RwLock::new(None),
            cached_entries: RwLock::new(None),
            dirty_slots: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub fn new() -> Self {
        let db = match MetadataDb::open() {
            Ok(db) => Some(Arc::new(db)),
            Err(e) => {
                eprintln!("[Babbage] Failed to open metadata database: {}", e);
                None
            }
        };

        let mut store = Self::without_db();
        store.db = db;
        if store.db.is_some() {
            let loaded = store.load_from_db();
            if loaded > 0 {
                println!("[Babbage] Loaded {} metadata entries from database", loaded);
            }
        }
        store
    }

    pub fn arena(&self) -> &NodeArena {
        &self.arena
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn with_db(db: MetadataDb) -> Self {
        let mut store = Self::without_db();
        store.db = Some(Arc::new(db));
        store.load_from_db();
        store
    }

    pub fn load_from_db(&mut self) -> usize {
        let db = match self.db.as_ref() {
            Some(db) => Arc::clone(db),
            None => return 0,
        };

        let entries = match db.load_all() {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("[Babbage] Failed to load metadata from database: {}", e);
                return 0;
            }
        };

        if let Ok(loaded_usage) = db.load_usage() {
            *self.usage.write() = loaded_usage;
        }

        self.arena = NodeArena::new();
        self.path_index.clear();
        *self.id_index.write() = None;
        *self.bursts.write() = None;

        let count = entries.len();
        // First pass: push nodes (parents resolved later).
        let mut idxs = Vec::with_capacity(entries.len());
        for entry in &entries {
            let ext_id = self.arena.intern_ext(entry.extension);
            let (name_off, name_len) = self.arena.names.push(&entry.name);
            let mut flags = 0u8;
            if entry.is_dir {
                flags |= Node::FLAG_DIR;
            }
            if entry.content_pending {
                flags |= Node::FLAG_CONTENT_PENDING;
            }
            let node = Node {
                parent: NO_PARENT,
                name_off,
                name_len,
                ext_id,
                flags,
                size: entry.size,
                mtime: entry.modified,
                dev: entry.dev,
                ino: entry.ino,
                char_bloom: entry.char_bloom,
                node_id: entry.id,
            };
            let idx = self.arena.push(node);
            self.path_index.insert(&self.arena, &entry.path, idx);
            idxs.push(idx);
        }
        // Second pass: resolve parent pointers.
        for (entry, &idx) in entries.iter().zip(idxs.iter()) {
            let parent = self.parent_index_for(&entry.path);
            if parent == NO_PARENT {
                let full = entry.path.to_string_lossy().into_owned();
                let (off, len) = self.arena.names.push(&full);
                let node = self.arena.node_mut(idx);
                node.parent = parent;
                node.name_off = off;
                node.name_len = len;
            } else {
                let node = self.arena.node_mut(idx);
                node.parent = parent;
            }
        }

        if let Ok(loaded_bursts) = db.load_bursts() {
            if !loaded_bursts.is_empty() {
                let mut bursts = vec![0u64; self.arena.len()];
                for (node_id, burst) in loaded_bursts {
                    if let Some(idx) = self.get_index_by_node_id(node_id)
                        && (idx as usize) < bursts.len()
                    {
                        bursts[idx as usize] = burst;
                    }
                }
                *self.bursts.write() = Some(bursts);
            } else {
                *self.bursts.write() = None;
            }
        }

        self.generation.fetch_add(1, Ordering::SeqCst);
        self.dir_generation.fetch_add(1, Ordering::SeqCst);
        *self.cached_trie.write() = None;

        count
    }

    fn insert_path_index(&mut self, path: &Path, idx: u32) {
        self.path_index.insert(&self.arena, path, idx);
    }

    fn find_path_index(&self, path: &Path) -> Option<u32> {
        self.path_index.find(&self.arena, path)
    }

    pub fn clear(&mut self) {
        self.arena = NodeArena::new();
        self.path_index.clear();
        *self.id_index.write() = None;
        *self.bursts.write() = None;
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.dir_generation.fetch_add(1, Ordering::SeqCst);
        *self.cached_trie.write() = None;

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.clear()
        {
            eprintln!("[Babbage] Failed to clear metadata database: {}", e);
        }
    }

    pub fn add(&mut self, entry: FileEntry) -> u64 {
        let is_dir = entry.is_dir;
        let mut entry_id = entry.id;
        // find existing
        let existing = self.find_path_index(&entry.path);
        let idx = if let Some(idx) = existing {
            // preserve content_pending
            let mut pending = self.arena.node(idx).content_pending();
            if self.arena.node(idx).content_pending() {
                pending = true;
            }
            // update node
            let ext_id = self.arena.intern_ext(entry.extension);
            let parent = self.parent_index_for(&entry.path);
            let node_name = if parent == NO_PARENT {
                entry.path.to_string_lossy().into_owned()
            } else {
                entry.name.clone()
            };
            // Reuse the stored name when unchanged: the name arena is
            // append-only, so re-pushing on every update leaked its bytes.
            let (name_off, name_len) = {
                let old = self.arena.node(idx);
                if self.arena.name(idx) == node_name {
                    (old.name_off, old.name_len)
                } else {
                    self.arena.names.push(&node_name)
                }
            };
            let mut flags = 0u8;
            if entry.is_dir {
                flags |= Node::FLAG_DIR;
            }
            if pending || entry.content_pending {
                flags |= Node::FLAG_CONTENT_PENDING;
            }
            let node = Node {
                parent,
                name_off,
                name_len,
                ext_id,
                flags,
                size: entry.size,
                mtime: entry.modified,
                dev: entry.dev,
                ino: entry.ino,
                char_bloom: entry.char_bloom,
                node_id: entry.id,
            };
            *self.arena.node_mut(idx) = node;
            idx
        } else {
            let ext_id = self.arena.intern_ext(entry.extension);
            let parent = self.parent_index_for(&entry.path);
            let name = if parent == NO_PARENT {
                entry.path.to_string_lossy().into_owned()
            } else {
                entry
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.name.clone())
            };
            let (name_off, name_len) = self.arena.names.push(&name);
            let mut flags = 0u8;
            if entry.is_dir {
                flags |= Node::FLAG_DIR;
            }
            if entry.content_pending {
                flags |= Node::FLAG_CONTENT_PENDING;
            }
            let node = Node {
                parent,
                name_off,
                name_len,
                ext_id,
                flags,
                size: entry.size,
                mtime: entry.modified,
                dev: entry.dev,
                ino: entry.ino,
                char_bloom: entry.char_bloom,
                node_id: entry.id,
            };
            let idx = self.arena.push(node);
            self.path_index.insert(&self.arena, &entry.path, idx);
            idx
        };

        if existing.is_some() && !is_dir {
            self.mark_slot_dirty(idx);
        } else {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }

        if is_dir {
            self.dir_generation.fetch_add(1, Ordering::SeqCst);
            *self.cached_trie.write() = None;
        }

        if let Some(db) = self.db.as_ref() {
            // Need a FileEntry view for db upsert
            let view = self.materialize(idx);
            match db.upsert(&view) {
                Ok(id) => {
                    self.arena.node_mut(idx).node_id = id;
                    entry_id = id;
                    if let Some(index) = self.id_index.write().as_mut()
                        && id > 0
                    {
                        index.insert(id, idx);
                    }
                    if let Some(bursts) = self.bursts.write().as_mut()
                        && idx as usize >= bursts.len()
                    {
                        bursts.resize(self.arena.len(), 0);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[Babbage] Failed to persist metadata entry '{}': {}",
                        view.path.display(),
                        e
                    );
                }
            }
        }

        entry_id
    }

    fn parent_index_for(&mut self, path: &Path) -> u32 {
        if let Some(parent) = path.parent()
            && let Some(idx) = self.find_path_index(parent)
        {
            return idx;
        }
        NO_PARENT
    }

    /// Marks every image and PDF for a content re-read. Used when what OCR
    /// produces has changed (an engine became usable, an OCR fix shipped).
    pub fn mark_ocr_targets_pending(&mut self) -> usize {
        let mut marked = 0;
        for i in 0..self.arena.len() {
            let idx = i as u32;
            if self.arena.node(idx).is_dir() {
                continue;
            }
            let ocr_target = self.arena.extension(idx).as_deref().is_some_and(|ext| {
                crate::indexer::extract::image::is_supported_image_extension(ext)
                    || ext.eq_ignore_ascii_case("pdf")
            });
            if ocr_target {
                self.arena.node_mut(idx).set_content_pending(true);
                marked += 1;
            }
        }
        if marked > 0 {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.mark_ocr_targets_pending()
        {
            eprintln!("[Babbage] Failed to mark OCR targets pending: {}", e);
        }
        marked
    }

    pub fn mark_images_pending(&mut self) -> usize {
        let mut marked = 0;
        for i in 0..self.arena.len() {
            let idx = i as u32;
            let node = self.arena.node(idx);
            if node.is_dir() {
                continue;
            }
            // extension check
            let ext = self.arena.extension(idx);
            let is_img = ext
                .as_deref()
                .is_some_and(crate::indexer::extract::image::is_supported_image_extension);
            if is_img {
                self.arena.node_mut(idx).set_content_pending(true);
                marked += 1;
            }
        }

        if marked > 0 {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.mark_images_pending()
        {
            eprintln!("[Babbage] Failed to mark images pending: {}", e);
        }

        marked
    }

    /// Marks files with any of `extensions` for a content re-read. Used when
    /// external extractors are added, removed or changed.
    pub fn mark_extensions_pending(&mut self, extensions: &[String]) -> usize {
        let wanted: std::collections::HashSet<String> = extensions
            .iter()
            .map(|extension| {
                extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase()
            })
            .filter(|extension| !extension.is_empty())
            .collect();
        if wanted.is_empty() {
            return 0;
        }

        let mut marked = 0;
        for i in 0..self.arena.len() {
            let idx = i as u32;
            if self.arena.node(idx).is_dir() {
                continue;
            }
            let matches = self
                .arena
                .extension(idx)
                .is_some_and(|extension| wanted.contains(&extension.to_ascii_lowercase()));
            if matches {
                self.arena.node_mut(idx).set_content_pending(true);
                marked += 1;
            }
        }
        if marked > 0 {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }

        if let Some(db) = self.db.as_ref() {
            let extensions: Vec<String> = wanted.into_iter().collect();
            if let Err(e) = db.mark_extensions_pending(&extensions) {
                eprintln!("[Babbage] Failed to mark extensions pending: {}", e);
            }
        }

        marked
    }

    /// Forces a content re-read for every file (used after a content-index
    /// schema migration). Directories keep their metadata.
    pub fn mark_all_content_pending(&mut self) -> usize {
        let mut marked = 0;
        for i in 0..self.arena.len() {
            let idx = i as u32;
            if !self.arena.node(idx).is_dir() {
                self.arena.node_mut(idx).set_content_pending(true);
                marked += 1;
            }
        }
        if marked > 0 {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.mark_all_content_pending()
        {
            eprintln!("[Babbage] Failed to mark content pending: {}", e);
        }
        marked
    }

    /// Flags one file for a content re-read on the next crawl.
    pub fn set_content_pending(&mut self, path: &Path) {
        if let Some(idx) = self.find_path_index(path) {
            let node = self.arena.node(idx);
            if !node.content_pending() {
                self.arena.node_mut(idx).set_content_pending(true);
                self.mark_slot_dirty(idx);
            }
        }

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.set_content_pending(path)
        {
            eprintln!(
                "[Babbage] Failed to mark '{}' pending: {}",
                path.display(),
                e
            );
        }
    }

    pub fn clear_content_pending(&mut self, path: &Path) {
        if let Some(idx) = self.find_path_index(path) {
            let node = self.arena.node(idx);
            if node.content_pending() {
                self.arena.node_mut(idx).set_content_pending(false);
                self.mark_slot_dirty(idx);
            }
        }

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.clear_content_pending(path)
        {
            eprintln!(
                "[Babbage] Failed to clear pending content for '{}': {}",
                path.display(),
                e
            );
        }
    }

    /// Every indexed path strictly below `dir`, found by following parent
    /// links: no path is materialised for nodes outside the subtree.
    pub fn descendant_paths(&self, dir: &Path) -> Vec<PathBuf> {
        let Some(root) = self.find_path_index(dir) else {
            return Vec::new();
        };
        let n = self.arena.len();
        // 0 = unknown, 1 = inside, 2 = outside; memoised per slot so each
        // parent chain is walked once.
        let mut state = vec![0u8; n];
        state[root as usize] = 1;
        let mut chain: Vec<u32> = Vec::new();
        for start in 0..n as u32 {
            let mut cur = start;
            chain.clear();
            let verdict = loop {
                let s = state[cur as usize];
                if s != 0 {
                    break s;
                }
                chain.push(cur);
                let parent = self.arena.node(cur).parent;
                if parent == crate::indexer::arena::NO_PARENT || parent as usize >= n {
                    break 2;
                }
                cur = parent;
            };
            for &c in &chain {
                state[c as usize] = verdict;
            }
        }
        (0..n as u32)
            .filter(|&i| i != root && state[i as usize] == 1)
            .map(|i| self.arena.path(i))
            .collect()
    }

    /// Removes many paths in one O(N) compaction instead of one swap-remove
    /// (with its index fix-ups and SQL round trip) per path.
    pub fn remove_many(&mut self, paths: &[PathBuf]) {
        if paths.len() < 16 {
            // Deepest first: `remove` swap-removes, and removing a directory
            // before its children would leave them pointing at whichever
            // node is swapped into the directory's slot.
            let mut ordered: Vec<&PathBuf> = paths.iter().collect();
            ordered.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
            for path in ordered {
                self.remove(path);
            }
            return;
        }
        let n = self.arena.len();
        let mut removed = vec![false; n];
        let mut count = 0usize;
        let mut any_dir = false;
        for path in paths {
            if let Some(idx) = self.find_path_index(path)
                && !removed[idx as usize]
            {
                removed[idx as usize] = true;
                count += 1;
                any_dir |= self.arena.node(idx).is_dir();
            }
        }
        if count > 0 {
            let mut remap = vec![u32::MAX; n];
            let mut next = 0u32;
            for (i, gone) in removed.iter().enumerate() {
                if !gone {
                    remap[i] = next;
                    next += 1;
                }
            }
            let mut bursts = self.bursts.write();
            let old_bursts = bursts.take();
            let mut new_bursts = old_bursts
                .as_ref()
                .map(|_| Vec::with_capacity(next as usize));
            let mut nodes = Vec::with_capacity(next as usize);
            for (i, gone) in removed.iter().enumerate() {
                if *gone {
                    continue;
                }
                let mut node = self.arena.nodes[i];
                if node.parent != crate::indexer::arena::NO_PARENT {
                    node.parent = remap
                        .get(node.parent as usize)
                        .copied()
                        .unwrap_or(crate::indexer::arena::NO_PARENT);
                }
                nodes.push(node);
                if let (Some(nb), Some(ob)) = (new_bursts.as_mut(), old_bursts.as_ref()) {
                    nb.push(ob.get(i).copied().unwrap_or(0));
                }
            }
            *bursts = new_bursts;
            drop(bursts);
            self.arena.nodes = nodes;
            self.path_index.clear();
            for i in 0..self.arena.len() {
                let idx = i as u32;
                let p = self.arena.path(idx);
                self.insert_path_index(&p, idx);
            }
            *self.id_index.write() = None;
            self.dirty_slots.lock().clear();
            self.generation.fetch_add(1, Ordering::SeqCst);
            if any_dir {
                self.dir_generation.fetch_add(1, Ordering::SeqCst);
                *self.cached_trie.write() = None;
            }
        }
        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.remove_many(paths)
        {
            eprintln!(
                "[Babbage] Failed to delete {} metadata entries: {}",
                paths.len(),
                e
            );
        }
    }

    pub fn remove(&mut self, path: &Path) {
        if let Some(idx) = self.find_path_index(path) {
            let was_dir = self.arena.node(idx).is_dir();
            let removed_id = self.arena.node(idx).node_id;
            if removed_id > 0
                && let Some(index) = self.id_index.write().as_mut()
            {
                index.remove(removed_id);
            }
            if let Some(bursts) = self.bursts.write().as_mut()
                && (idx as usize) < bursts.len()
            {
                bursts[idx as usize] = 0;
            }

            self.path_index.remove(&self.arena, path);

            let last_idx = self.arena.len().saturating_sub(1) as u32;
            if idx < last_idx {
                let last_path = self.arena.path(last_idx);
                self.path_index.remove(&self.arena, &last_path);

                let last_node = self.arena.nodes[last_idx as usize];
                self.arena.nodes[idx as usize] = last_node;

                let swapped_path = self.arena.path(idx);
                self.path_index.insert(&self.arena, &swapped_path, idx);

                let swapped_id = self.arena.node(idx).node_id;
                if swapped_id > 0
                    && let Some(index) = self.id_index.write().as_mut()
                {
                    index.insert(swapped_id, idx);
                }

                if let Some(bursts) = self.bursts.write().as_mut()
                    && (idx as usize) < bursts.len()
                    && (last_idx as usize) < bursts.len()
                {
                    bursts[idx as usize] = bursts[last_idx as usize];
                }
            }
            // Only directories have children. Files are the vast majority of
            // nodes, and scanning the whole arena per removed file made
            // folder deletes quadratic under the write lock.
            if idx < last_idx && self.arena.nodes[idx as usize].is_dir() {
                for node in self.arena.nodes.iter_mut() {
                    if node.parent == last_idx {
                        node.parent = idx;
                    }
                }
            }
            self.arena.nodes.pop();
            if let Some(bursts) = self.bursts.write().as_mut()
                && !bursts.is_empty()
            {
                bursts.pop();
            }

            self.generation.fetch_add(1, Ordering::SeqCst);
            if was_dir {
                self.dir_generation.fetch_add(1, Ordering::SeqCst);
                *self.cached_trie.write() = None;
            }
        }

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.remove(path)
        {
            eprintln!(
                "[Babbage] Failed to delete metadata entry '{}': {}",
                path.display(),
                e
            );
        }
    }

    pub fn rename_prefix(&mut self, old: &Path, new: &Path) -> Vec<PathBuf> {
        if old.as_os_str().is_empty() || old == new {
            return Vec::new();
        }

        // conflicts
        let mut conflicts: Vec<PathBuf> = Vec::new();
        for i in 0..self.arena.len() {
            let idx = i as u32;
            let p = self.arena.path(idx);
            if !p.starts_with(old) && p.starts_with(new) {
                conflicts.push(p);
            }
        }
        for path in conflicts {
            self.remove(&path);
        }

        let mut old_paths = Vec::new();
        // Reparent the renamed root (if present) under the new location's ancestors.
        for i in 0..self.arena.len() {
            let idx = i as u32;
            let p = self.arena.path(idx);
            if p == old {
                let new_parent = self.parent_index_for(new);
                if new_parent == NO_PARENT {
                    let full = new.to_string_lossy().into_owned();
                    let (off, len) = self.arena.names.push(&full);
                    let node = self.arena.node_mut(idx);
                    node.parent = new_parent;
                    node.name_off = off;
                    node.name_len = len;
                } else {
                    let node = self.arena.node_mut(idx);
                    node.parent = new_parent;
                }
            }
        }
        // Snapshot the subtree before changing any name.
        //
        // `arena.path()` is reconstructed from the parent chain, so rewriting
        // an ancestor's name changed every descendant's derived path. The
        // single-pass version below that mutated as it scanned therefore
        // matched the renamed root and then failed `strip_prefix(old)` for
        // every child: only the root reached `old_paths`, it has no document
        // of its own, and every `old/**` document stayed in the content index
        // forever under a path that no longer exists.
        let mut moved: Vec<(u32, PathBuf, PathBuf)> = Vec::new();
        for i in 0..self.arena.len() {
            let idx = i as u32;
            let p = self.arena.path(idx);
            let Ok(suffix) = p.strip_prefix(old) else {
                continue;
            };
            let new_path = if suffix.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(suffix)
            };
            moved.push((idx, p, new_path));
        }

        for (idx, old_path, new_path) in moved {
            old_paths.push(old_path);
            // update name (file_name) and char_bloom
            let new_name = new_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (name_off, name_len) = self.arena.names.push(&new_name);
            let node = self.arena.node_mut(idx);
            node.name_off = name_off;
            node.name_len = name_len;
            node.char_bloom = crate::search::fuzzy::compute_char_mask(&new_name);
            // store new path? We don't store full path; path derived from parent chain.
            // Parent chain unchanged (directory renamed would require updating parents).
            // For correctness, update parent pointers? Complex. Instead, rebuild path_index after.
        }

        if old_paths.is_empty() {
            return old_paths;
        }

        // Rebuild path_index
        self.path_index.clear();
        for i in 0..self.arena.len() {
            let idx = i as u32;
            let p = self.arena.path(idx);
            // Wait: path() derives from parent chain + names. If we only updated leaf names,
            // parent directories names not updated for rename_prefix of a directory.
            // This is a known limitation; rename_prefix in original updated path strings.
            // To handle directory renames properly, we'd need update all descendants' parent names?
            // Our arena stores names per node; renaming a directory means renaming that node's name,
            // and descendants keep parent pointers, so path() will reflect new dir name. Good.
            // But we updated only nodes whose path starts with old? We updated names for those nodes
            // based on new_path.file_name(), which for a child keeps its own name, not parent. Oops.
            // Actually for a directory rename, the directory node's name should change to new's last component;
            // descendants keep their names. Our loop changed EVERY descendant's name to its new_path last component,
            // which is wrong (child name becomes same as? new_path for child is new/child, file_name=child, okay).
            // For the renamed root itself, new_path=new, file_name=new last comp. Good.
            // So names are okay.
            self.insert_path_index(&p, idx);
        }

        self.generation.fetch_add(1, Ordering::SeqCst);
        self.dir_generation.fetch_add(1, Ordering::SeqCst);
        *self.cached_trie.write() = None;

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.rename_prefix(old, new)
        {
            eprintln!(
                "[Babbage] Failed to persist directory rename '{}' -> '{}': {}",
                old.display(),
                new.display(),
                e
            );
        }

        old_paths
    }

    pub fn record_open(&self, path: &Path) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        {
            let mut usage = self.usage.write();
            let entry = usage.entry(path.to_path_buf()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = now;
        }

        if let Some(db) = self.db.as_ref()
            && let Err(e) = db.record_open(path)
        {
            eprintln!("[Babbage] Failed to persist open in database: {}", e);
        }
    }

    pub fn get_usage(&self, path: &Path) -> (u32, u64) {
        self.usage.read().get(path).copied().unwrap_or((0, 0))
    }

    /// The materialized entry list for the current generation.
    ///
    /// Returned behind an `Arc` of immutable rows, so a query can hold the list
    /// while another thread mutates the store: the worst case is one query
    /// seeing a list one generation old, which is what it read before the cache
    /// existed.
    fn mark_slot_dirty(&self, idx: u32) {
        let mut dirty = self.dirty_slots.lock();
        dirty.push(idx);
        // Past this many patches a rebuild is as cheap; stop tracking.
        if dirty.len() > self.arena.len() / 8 + 64 {
            dirty.clear();
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn entries_snapshot(&self) -> Arc<Vec<crate::indexer::walker::FileEntry>> {
        let current_gen = self.generation.load(Ordering::SeqCst);
        {
            let mut dirty = self.dirty_slots.lock();
            if !dirty.is_empty() {
                let mut cache = self.cached_entries.write();
                match cache.as_mut() {
                    Some((cached_gen, entries)) if *cached_gen == current_gen => {
                        // Unique unless a query still holds the old list;
                        // then this clones once, still cheaper than a rebuild.
                        let rows = Arc::make_mut(entries);
                        for &idx in dirty.iter() {
                            if let Some(row) = rows.get_mut(idx as usize) {
                                *row = self.materialize(idx);
                            }
                        }
                    }
                    _ => {}
                }
                dirty.clear();
            }
        }
        // `parking_lot` locks are not poisoned, so there is no Result to unwrap
        // here: the same shape as `cached_trie` above.
        {
            let cache = self.cached_entries.read();
            if let Some((cached_gen, entries)) = cache.as_ref()
                && *cached_gen == current_gen
            {
                return Arc::clone(entries);
            }
        }

        let built = Arc::new(
            (0..self.arena.len())
                .map(|index| self.materialize(index as u32))
                .collect::<Vec<_>>(),
        );
        *self.cached_entries.write() = Some((current_gen, Arc::clone(&built)));
        built
    }

    pub fn get_directory_trie(&self) -> Arc<DirectoryTrie> {
        let current_gen = self.dir_generation.load(Ordering::SeqCst);
        {
            let cache = self.cached_trie.read();
            if let Some((cached_gen, ref trie)) = *cache
                && cached_gen == current_gen
            {
                return Arc::clone(trie);
            }
        }

        // materialize
        let mut entries = Vec::with_capacity(self.arena.len());
        let mut path_index = HashMap::with_capacity(self.arena.len());
        for i in 0..self.arena.len() {
            let idx = i as u32;
            let e = self.materialize(idx);
            path_index.insert(e.path.clone(), i);
            entries.push(e);
        }

        let new_trie = Arc::new(DirectoryTrie::build(&entries, &path_index, current_gen));
        let mut cache = self.cached_trie.write();
        *cache = Some((current_gen, Arc::clone(&new_trie)));
        new_trie
    }

    pub fn get(&self, path: &Path) -> Option<FileEntry> {
        self.find_path_index(path).map(|idx| self.materialize(idx))
    }

    pub fn get_index(&self, path: &Path) -> Option<u32> {
        self.find_path_index(path)
    }

    pub fn materialize(&self, idx: u32) -> FileEntry {
        let node = self.arena.node(idx);
        let extension =
            crate::indexer::walker::intern_extension(self.arena.extension(idx).as_deref());
        let path = self.arena.path(idx);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.arena.name(idx).to_string());
        FileEntry {
            id: node.node_id,
            path,
            name,
            extension,
            size: node.size,
            modified: node.mtime,
            is_dir: node.is_dir(),
            dev: node.dev,
            ino: node.ino,
            char_bloom: node.char_bloom,
            content_pending: node.content_pending(),
        }
    }

    pub fn get_by_node_id(&self, node_id: u64) -> Option<FileEntry> {
        let idx = {
            let mut guard = self.id_index.write();
            if guard.is_none() {
                let mut index = IdIndex::new();
                for i in 0..self.arena.len() {
                    let id = self.arena.node(i as u32).node_id;
                    if id > 0 {
                        index.insert(id, i as u32);
                    }
                }
                *guard = Some(index);
            }
            guard.as_ref()?.get(node_id)?
        };
        Some(self.materialize(idx))
    }

    pub fn get_index_by_node_id(&self, node_id: u64) -> Option<u32> {
        // Fast path: shared lock once the index exists (the common case), so
        // concurrent lookups from ranking never serialize on a write lock.
        if let Some(index) = self.id_index.read().as_ref() {
            return index.get(node_id);
        }
        let mut guard = self.id_index.write();
        if guard.is_none() {
            let mut index = IdIndex::new();
            for i in 0..self.arena.len() {
                let id = self.arena.node(i as u32).node_id;
                if id > 0 {
                    index.insert(id, i as u32);
                }
            }
            *guard = Some(index);
        }
        guard.as_ref()?.get(node_id)
    }

    pub fn path_index_map(&self) -> HashMap<PathBuf, usize> {
        let mut map = HashMap::with_capacity(self.arena.len());
        for i in 0..self.arena.len() {
            let idx = i as u32;
            map.insert(self.arena.path(idx), i);
        }
        map
    }

    /// Assigned burst for a node, if it is an image that has been grouped.
    pub fn burst_for_node_id(&self, node_id: u64) -> Option<u64> {
        let idx = self.get_index_by_node_id(node_id)?;
        let bursts = self.bursts.read();
        let bursts = bursts.as_ref()?;
        if idx as usize >= bursts.len() {
            return None;
        }
        let burst = bursts[idx as usize];
        if burst == 0 { None } else { Some(burst) }
    }

    /// Assigned burst for a path. The hash lookup is the same one search
    /// already uses for metadata, so collapsing results stays O(results).
    pub fn burst_for_path(&self, path: &Path) -> Option<u64> {
        let idx = self.find_path_index(path)?;
        let node_id = self.arena.node(idx).node_id;
        self.burst_for_node_id(node_id)
    }

    /// Drops a stale burst assignment while a changed image waits for a new
    /// perceptual hash/backfill pass.
    pub fn clear_burst_for_path(&mut self, path: &Path) {
        let Some(idx) = self.find_path_index(path) else {
            return;
        };
        if let Some(bursts) = self.bursts.write().as_mut()
            && (idx as usize) < bursts.len()
        {
            bursts[idx as usize] = 0;
        }
    }

    /// Applies burst assignments returned by the database after a merge.
    pub fn apply_bursts(&mut self, updates: &[(PathBuf, u64)]) {
        let mut bursts = self.bursts.write();
        if bursts.is_none() {
            *bursts = Some(vec![0; self.arena.len()]);
        }
        let bursts = bursts.as_mut().expect("bursts allocated");
        for (path, burst) in updates {
            let Some(idx) = self.find_path_index(path) else {
                continue;
            };
            if idx as usize >= bursts.len() {
                bursts.resize(self.arena.len(), 0);
            }
            let node_id = self.arena.node(idx).node_id;
            if node_id > 0 {
                bursts[idx as usize] = *burst;
            }
        }
    }

    pub fn db(&self) -> Option<Arc<MetadataDb>> {
        self.db.as_ref().map(Arc::clone)
    }

    pub fn path_index_len(&self) -> usize {
        self.path_index.len()
    }

    pub fn path_index_capacity(&self) -> usize {
        self.path_index.capacity()
    }

    pub fn id_index_len(&self) -> usize {
        self.id_index.read().as_ref().map(IdIndex::len).unwrap_or(0)
    }

    pub fn bursts_allocated(&self) -> usize {
        self.bursts
            .read()
            .as_ref()
            .map(|bursts| bursts.len())
            .unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.arena.len()
    }

    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    // Accessors for search hot path
    pub fn node(&self, idx: u32) -> &Node {
        self.arena.node(idx)
    }
    pub fn name(&self, idx: u32) -> &str {
        self.arena.name(idx)
    }
    pub fn path(&self, idx: u32) -> PathBuf {
        self.arena.path(idx)
    }
    pub fn extension(&self, idx: u32) -> Option<String> {
        self.arena.extension(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempStore {
        store: Option<MetadataStore>,
        dir: PathBuf,
    }

    impl TempStore {
        fn new(tag: &str) -> Self {
            let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "babbage-metadata-store-test-{}-{}-{}",
                tag,
                std::process::id(),
                seq
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let db = MetadataDb::open_in_dir(&dir).expect("open temp db");
            Self {
                store: Some(MetadataStore::with_db(db)),
                dir,
            }
        }

        fn store(&mut self) -> &mut MetadataStore {
            self.store.as_mut().expect("store open")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            self.store.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn entry(path: &Path, is_dir: bool) -> FileEntry {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        FileEntry {
            id: 0,
            path: path.to_path_buf(),
            name,
            extension: if is_dir {
                None
            } else {
                crate::indexer::walker::intern_extension(
                    path.extension().and_then(|ext| ext.to_str()),
                )
            },
            size: 1,
            modified: 1,
            is_dir,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending: false,
        }
    }

    fn tree(store: &mut MetadataStore, root: &Path, dirs: usize, files: usize) -> Vec<PathBuf> {
        store.add(entry(root, true));
        let mut all = Vec::new();
        for d in 0..dirs {
            let dir = root.join(format!("d{d}"));
            store.add(entry(&dir, true));
            for f in 0..files {
                let file = dir.join(format!("f{f}.txt"));
                store.add(entry(&file, false));
                all.push(file);
            }
            all.push(dir);
        }
        all
    }

    #[test]
    fn remove_many_matches_one_by_one_removal() {
        let mut a = TempStore::new("remove-many-a");
        let mut b = TempStore::new("remove-many-b");
        let root_a = a.dir.join("root");
        let root_b = b.dir.join("root");
        tree(a.store(), &root_a, 6, 10);
        tree(b.store(), &root_b, 6, 10);

        // Directories listed before their children, as a caller might.
        let doomed = |root: &Path, big: bool| -> Vec<PathBuf> {
            let mut paths = vec![root.join("d2")];
            paths.extend((0..10).map(|f| root.join("d2").join(format!("f{f}.txt"))));
            paths.extend((0..10).step_by(3).map(|f| root.join("d4").join(format!("f{f}.txt"))));
            if big {
                paths.push(root.join("d3"));
                paths.extend((0..10).map(|f| root.join("d3").join(format!("f{f}.txt"))));
            }
            paths
        };
        // Small batch (fallback path), then a large one (compaction path).
        a.store().remove_many(&doomed(&root_a, false));
        a.store().remove_many(&doomed(&root_a, true));
        // Reference: one at a time, children before their directory.
        let mut reference = doomed(&root_b, true);
        reference.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for path in reference {
            b.store().remove(&path);
        }

        let names = |store: &MetadataStore, root: &Path| -> Vec<String> {
            let mut v: Vec<String> = store
                .entries_snapshot()
                .iter()
                .map(|e| e.path.strip_prefix(root).unwrap().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let (sa, sb) = (a.store.as_ref().unwrap(), b.store.as_ref().unwrap());
        assert_eq!(names(sa, &root_a), names(sb, &root_b));
        // Every surviving path still resolves to itself.
        for e in sa.entries_snapshot().iter() {
            let idx = sa.get_index(&e.path).unwrap_or_else(|| panic!("not indexed: {:?} (len {})", e.path, sa.len()));
            assert_eq!(sa.materialize(idx).path, e.path);
        }
        assert!(sa.get(&root_a.join("d2").join("f0.txt")).is_none());
    }

    #[test]
    fn descendant_paths_lists_only_the_subtree() {
        let mut temp = TempStore::new("descendants");
        let root = temp.dir.join("root");
        tree(temp.store(), &root, 3, 4);
        let mut got = temp.store().descendant_paths(&root.join("d1"));
        got.sort();
        let mut want: Vec<PathBuf> = (0..4).map(|f| root.join("d1").join(format!("f{f}.txt"))).collect();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn in_place_updates_patch_the_snapshot_without_a_rebuild() {
        let mut temp = TempStore::new("snapshot-patch");
        let root = temp.dir.join("root");
        tree(temp.store(), &root, 2, 3);
        let file = root.join("d0").join("f1.txt");
        let store = temp.store();
        let before = store.entries_snapshot();
        let generation = store.generation();
        drop(before);

        store.set_content_pending(&file);
        let mut changed = entry(&file, false);
        changed.size = 99;
        store.add(changed);

        assert_eq!(store.generation(), generation, "in-place edits must not bump generation");
        let snapshot = store.entries_snapshot();
        let row = snapshot.iter().find(|e| e.path == file).expect("row");
        assert_eq!(row.size, 99);
        assert!(row.content_pending);
    }

    #[test]
    fn mark_all_content_pending_skips_directories() {
        let mut temp = TempStore::new("mark-all");
        let image = temp.dir.join("photo.jpg");
        let doc = temp.dir.join("notes.txt");
        let dir_path = temp.dir.join("folder");

        let store = temp.store();
        store.add(entry(&image, false));
        store.add(entry(&doc, false));
        store.add(entry(&dir_path, true));

        assert_eq!(store.mark_all_content_pending(), 2);
        assert!(store.get(&image).expect("image").content_pending);
        assert!(store.get(&doc).expect("doc").content_pending);
        assert!(!store.get(&dir_path).expect("dir").content_pending);
    }

    #[test]
    fn mark_images_pending_updates_memory_and_only_images() {
        let mut temp = TempStore::new("mark");
        let image = temp.dir.join("photo.jpg");
        let doc = temp.dir.join("notes.txt");
        let dir_path = temp.dir.join("folder.png");

        let store = temp.store();
        store.add(entry(&image, false));
        store.add(entry(&doc, false));
        store.add(entry(&dir_path, true));

        assert_eq!(store.mark_images_pending(), 1);
        assert!(store.get(&image).expect("image").content_pending);
        assert!(!store.get(&doc).expect("doc").content_pending);
        assert!(!store.get(&dir_path).expect("dir").content_pending);

        store.clear_content_pending(&image);
        assert!(!store.get(&image).expect("image").content_pending);

        let db = store.db().expect("database");
        let loaded = db.load_all().expect("load nodes");
        assert!(
            !loaded
                .iter()
                .find(|entry| entry.path == image)
                .expect("image row")
                .content_pending,
            "clear must be persisted"
        );
    }

    #[test]
    fn add_preserves_pending_when_replacing_an_entry() {
        let mut temp = TempStore::new("preserve");
        let image = temp.dir.join("photo.png");

        let mut first = entry(&image, false);
        first.content_pending = true;
        first.size = 10;
        temp.store().add(first);

        let mut replacement = entry(&image, false);
        replacement.size = 20;
        temp.store().add(replacement);

        let stored = temp.store().get(&image).expect("image entry");
        assert!(
            stored.content_pending,
            "replacing an entry must preserve its pending flag"
        );
        assert_eq!(stored.size, 20, "the new metadata must still be applied");
    }
}
