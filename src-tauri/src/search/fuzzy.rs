use crate::indexer::walker::FileEntry;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;

/// Computes a 64-bit character presence bitmask for fast prefiltering.
pub fn compute_char_mask(text: &str) -> u64 {
    let mut mask = 0u64;
    for ch in text.chars() {
        let ch_lower = ch.to_ascii_lowercase();
        let bit = match ch_lower {
            'a'..='z' => ch_lower as u8 - b'a',
            '0'..='9' => 26 + (ch_lower as u8 - b'0'),
            '_' => 36,
            '-' => 37,
            '.' => 38,
            '/' | '\\' => 39,
            ' ' => 40,
            _ => 41 + ((ch as u32 % 23) as u8),
        };
        mask |= 1u64 << (bit & 63);
    }
    mask
}

/// Lowercased whitespace-separated query terms for the literal check, or
/// `None` when the query uses nucleo atom syntax (`!x`, `^x`, `'x`, `x$`)
/// whose meaning is not "contains this text".
fn literal_terms(query: &str) -> Option<Vec<String>> {
    let mut terms = Vec::new();
    for atom in query.split_whitespace() {
        if atom.starts_with(['!', '^', '\'']) || atom.ends_with('$') {
            return None;
        }
        // Either separator matches either: `src/app` finds `src\app`.
        terms.push(atom.to_lowercase().replace('\\', "/"));
    }
    (!terms.is_empty()).then_some(terms)
}

/// Scores one entry for a parsed pattern. Callers provide the matcher and
/// scratch buffer so parallel workers can keep their own state.
///
/// The flag is true when every query term appears verbatim (case-insensitive)
/// in the path, as opposed to a scattered subsequence match.
fn score_entry(
    pattern: &Pattern,
    matcher: &mut Matcher,
    char_buf: &mut Vec<char>,
    entry: &FileEntry,
    query_mask: u64,
    match_path: bool,
    literal_terms: Option<&[String]>,
) -> Option<(u32, bool)> {
    // Char-bloom prefilter: 64-bit letter mask per name.
    if !match_path && (entry.char_bloom & query_mask) != query_mask {
        return None;
    }
    let path_str = entry.path.to_string_lossy();
    let utf32 = Utf32Str::new(&path_str, char_buf);
    let mut final_score = pattern.score(utf32, matcher)?;
    let name_utf32 = Utf32Str::new(&entry.name, char_buf);
    if let Some(name_score) = pattern.score(name_utf32, matcher) {
        final_score += name_score * 2;
    }
    // Only entries nucleo already accepted pay for the lowercase copy.
    let literal = literal_terms.is_some_and(|terms| {
        let haystack = path_str.to_lowercase().replace('\\', "/");
        terms.iter().all(|term| haystack.contains(term.as_str()))
    });
    Some((final_score, literal))
}

pub struct FuzzySearcher {
    matcher: Matcher,
}

impl Default for FuzzySearcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzySearcher {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        }
    }

    pub fn search(
        &mut self,
        query: &str,
        all_entries: &[FileEntry],
        candidate_indices: &[usize],
        limit: usize,
    ) -> Vec<(usize, u32)> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let pattern = Pattern::parse(trimmed, CaseMatching::Ignore, Normalization::Smart);
        // Whitespace separates terms and is never matched, so it must not be
        // required of the name: `vite config` has to reach `vite.config.ts`.
        let mask_text: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        let query_mask = compute_char_mask(&mask_text);
        let match_path = trimmed.contains('/') || trimmed.contains('\\');
        let literal_terms = literal_terms(trimmed);
        let literal_terms = literal_terms.as_deref();

        // Large candidate sets (1M-file corpora) are scored across the rayon
        // pool; small ones stay sequential so a keystroke never pays
        // scheduling overhead.
        const PARALLEL_THRESHOLD: usize = 20_000;
        let scored: Vec<(usize, u32, bool)> = if candidate_indices.len() >= PARALLEL_THRESHOLD {
            candidate_indices
                .par_iter()
                .map_init(
                    || (Matcher::new(Config::DEFAULT.match_paths()), Vec::new()),
                    |(matcher, char_buf), &orig_idx| {
                        all_entries.get(orig_idx).and_then(|entry| {
                            score_entry(
                                &pattern,
                                matcher,
                                char_buf,
                                entry,
                                query_mask,
                                match_path,
                                literal_terms,
                            )
                            .map(|(score, literal)| (orig_idx, score, literal))
                        })
                    },
                )
                .flatten()
                .collect()
        } else {
            let mut char_buf = Vec::new();
            let mut scored = Vec::new();
            for &orig_idx in candidate_indices {
                let Some(entry) = all_entries.get(orig_idx) else {
                    continue;
                };
                if let Some((score, literal)) = score_entry(
                    &pattern,
                    &mut self.matcher,
                    &mut char_buf,
                    entry,
                    query_mask,
                    match_path,
                    literal_terms,
                ) {
                    scored.push((orig_idx, score, literal));
                }
            }
            scored
        };

        // Someone typing `nihil.cfg` wants that file, not every path whose
        // letters happen to contain n-i-h-i-l-.-c-f-g in order. When anything
        // matches the query verbatim, the scattered subsequence matches are
        // dropped; they stay as the typo/abbreviation fallback for queries
        // with no verbatim match at all.
        let any_literal = scored.iter().any(|&(_, _, literal)| literal);
        let mut scored_indices: Vec<(usize, u32)> = scored
            .into_iter()
            .filter(|&(_, _, literal)| literal || !any_literal)
            .map(|(idx, score, _)| (idx, score))
            .collect();

        // Top-k heap/selection using select_nth_unstable: O(N) instead of O(N log N)
        if scored_indices.len() > limit {
            scored_indices.select_nth_unstable_by(limit, |a, b| b.1.cmp(&a.1));
            scored_indices.truncate(limit);
        }
        scored_indices.sort_by_key(|a| std::cmp::Reverse(a.1));

        scored_indices
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// (query, expected file) pairs used by the ranking tests. The expected
    /// value is matched against the entry path suffix.
    const CASES: &[(&str, &str)] = &[
        ("cargo", "Cargo.toml"),
        ("app", "App.tsx"),
        ("search", "search/mod.rs"),
        ("readme", "README.md"),
        ("fuzzy", "fuzzy.rs"),
        ("settings", "settings.json"),
    ];

    /// Builds an entry from a repo-relative path. The display name mirrors the
    /// relative path, so the name-based char-bloom prefilter sees the same
    /// characters as the path that nucleo scores.
    fn entry(relative: &str) -> FileEntry {
        let path = PathBuf::from(relative);
        let extension =
            crate::indexer::walker::intern_extension(path.extension().and_then(|e| e.to_str()));
        FileEntry {
            id: 0,
            char_bloom: compute_char_mask(relative),
            path,
            name: relative.to_string(),
            extension,
            size: 0,
            modified: 0,
            is_dir: false,
            dev: 0,
            ino: 0,
            content_pending: false,
        }
    }

    fn fixture() -> Vec<FileEntry> {
        [
            "repo/Cargo.toml",
            "repo/app/App.tsx",
            "repo/src/search/mod.rs",
            "repo/README.md",
            "repo/src/main.rs",
            "repo/src/search/fuzzy.rs",
            "repo/config/settings.json",
            "repo/notes.md",
            "repo/package.json",
            "repo/tsconfig.json",
            "repo/vite.config.ts",
            "repo/src/lib.rs",
            "repo/index.html",
            "repo/styles.css",
            "repo/src/embed/model.rs",
        ]
        .into_iter()
        .map(entry)
        .collect()
    }

    fn index_of(entries: &[FileEntry], expected: &str) -> usize {
        entries
            .iter()
            .position(|e| Path::new(expected) == e.path || e.path.ends_with(expected))
            .unwrap_or_else(|| panic!("fixture has no entry matching {expected}"))
    }

    #[test]
    fn expected_files_rank_in_top_three() {
        let entries = fixture();
        let candidates: Vec<usize> = (0..entries.len()).collect();
        let mut searcher = FuzzySearcher::new();

        for &(query, expected) in CASES {
            let results = searcher.search(query, &entries, &candidates, 50);
            assert!(!results.is_empty(), "query {query:?} returned nothing");

            let top_three: Vec<&Path> = results
                .iter()
                .take(3)
                .map(|&(idx, _)| entries[idx].path.as_path())
                .collect();
            assert!(
                top_three.iter().any(|path| path.ends_with(expected)),
                "query {query:?}: expected {expected} in top 3, got {top_three:?}"
            );
        }
    }

    fn names(entries: &[FileEntry], results: &[(usize, u32)]) -> Vec<String> {
        results
            .iter()
            .map(|&(idx, _)| entries[idx].name.clone())
            .collect()
    }

    #[test]
    fn verbatim_match_drops_scattered_subsequence_matches() {
        // Every path contains n-i-h-i-l-.-c-f-g in order; only one contains
        // the text itself.
        let entries: Vec<FileEntry> = [
            "C:/Users/Nihil/csgo/cfg/Nihil.cfg",
            "C:/Users/Nihil/Desktop/Vestigium/playwright.config.ts",
            "C:/Users/Nihil/Music/You Shall Never Have To Forgive Me Again.flac",
            "C:/Platform/Microsoft.DiagnosticsHub.VisualStudio.Package.vsdconfig",
        ]
        .into_iter()
        .map(entry)
        .collect();
        let candidates: Vec<usize> = (0..entries.len()).collect();
        let mut searcher = FuzzySearcher::new();

        let results = searcher.search("nihil.cfg", &entries, &candidates, 50);
        assert_eq!(
            names(&entries, &results),
            ["C:/Users/Nihil/csgo/cfg/Nihil.cfg"]
        );
    }

    #[test]
    fn scattered_matches_remain_the_fallback_without_a_verbatim_match() {
        let entries = fixture();
        let candidates: Vec<usize> = (0..entries.len()).collect();
        let mut searcher = FuzzySearcher::new();

        // No path contains "crgtml"; the abbreviation still finds Cargo.toml.
        let results = searcher.search("crgtml", &entries, &candidates, 50);
        assert!(
            names(&entries, &results).contains(&"repo/Cargo.toml".to_string()),
            "abbreviation lost its fuzzy fallback: {:?}",
            names(&entries, &results)
        );
    }

    #[test]
    fn multi_word_query_matches_a_name_without_spaces() {
        // Real entries carry the bare file name, and the prefilter runs on it.
        let mut vite = entry("repo/vite.config.ts");
        vite.name = "vite.config.ts".to_string();
        vite.char_bloom = compute_char_mask(&vite.name);
        let mut ts = entry("repo/tsconfig.json");
        ts.name = "tsconfig.json".to_string();
        ts.char_bloom = compute_char_mask(&ts.name);
        let entries = vec![vite, ts];
        let mut searcher = FuzzySearcher::new();

        let results = searcher.search("vite config", &entries, &[0, 1], 50);
        assert_eq!(names(&entries, &results), ["vite.config.ts"]);
    }

    #[test]
    fn char_bloom_prefilter_keeps_valid_matches() {
        let entries = fixture();
        let candidates: Vec<usize> = (0..entries.len()).collect();
        let mut searcher = FuzzySearcher::new();

        for &(query, expected) in CASES {
            let expected_idx = index_of(&entries, expected);
            let query_mask = compute_char_mask(query);

            // The expected file must pass the char-bloom prefilter ...
            assert_eq!(
                entries[expected_idx].char_bloom & query_mask,
                query_mask,
                "query {query:?}: {expected} fails the char-bloom prefilter"
            );

            // ... and therefore survive into the search results.
            let results = searcher.search(query, &entries, &candidates, 50);
            assert!(
                results.iter().any(|&(idx, _)| idx == expected_idx),
                "query {query:?}: {expected} was dropped before scoring"
            );
        }
    }

    #[test]
    fn prefilter_drops_only_files_without_query_chars() {
        let entries = fixture();
        let candidates: Vec<usize> = (0..entries.len()).collect();
        let mut searcher = FuzzySearcher::new();

        // Reference scan without the prefilter: every entry the matcher would
        // accept is expected to survive the char-bloom check and reach scoring.
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut buf = Vec::new();

        for &(query, _) in CASES {
            let query_mask = compute_char_mask(query);
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

            let mut expected_matches = Vec::new();
            for (idx, entry) in entries.iter().enumerate() {
                let path = entry.path.to_string_lossy();
                let accepted = pattern
                    .score(Utf32Str::new(&path, &mut buf), &mut matcher)
                    .is_some()
                    || pattern
                        .score(Utf32Str::new(&entry.name, &mut buf), &mut matcher)
                        .is_some();
                if accepted && entry.char_bloom & query_mask == query_mask {
                    expected_matches.push(idx);
                }
            }

            let results = searcher.search(query, &entries, &candidates, 50);
            for idx in expected_matches {
                assert!(
                    results.iter().any(|&(result_idx, _)| result_idx == idx),
                    "query {query:?}: entry {} was dropped by the prefilter",
                    entries[idx].name
                );
            }
        }
    }
}
