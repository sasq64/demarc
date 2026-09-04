//! The filtering behind the searchable list: given a query, produce the
//! entries that match it, best match first. Pure logic — no UI, no engine; the
//! list itself is drawn elsewhere and only calls in here.
//!
//! Matching is delegated to a [`FuzzySource`] so the strategy is pluggable. The
//! bundled [`SubstringSource`] / [`AllWordsSource`] do a simple
//! case-insensitive substring scan over an in-memory `Vec<String>` — fine for
//! small/medium lists. For large lists (tens of thousands of entries) use
//! [`IndexedSource`], which prunes with a precomputed trigram index and ranks
//! the survivors with `nucleo` — an order of magnitude faster than a linear
//! scan. To go further still, implement [`FuzzySource`] over your own index or
//! an external database and hand that to the picker; nothing else changes.

use std::collections::HashMap;
use std::sync::Arc;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// The number of results fetched from the source per query. A source may hold
/// far more items than can be shown; this caps how many we pull and render.
pub const DEFAULT_MAX_RESULTS: usize = 500_000;

/// Backs the searchable list with items. Implement this to plug in smarter
/// matching without touching the UI: prefix trees, fuzzy scoring, or an
/// external index/database.
///
/// `T` is what one item *is* to the caller — the record the list is a view of
/// ([`get_data`](FuzzySource::get_data) hands it back, so a caller holding an
/// id needn't keep its own copy of the list to resolve it). A source with
/// nothing behind its rows but their text leaves `T` at `()` and inherits the
/// default `get_data`; the bundled sources go further and implement the trait
/// for *every* `T`, so a plain list of strings drops into a picker whose other
/// sources do carry records.
pub trait FuzzySource<T = ()>: Send + Sync + 'static {
    /// Return the ids of the items matching `query`, best match first, capped
    /// at `limit`. An empty/whitespace query should return the head of the full
    /// list (the unfiltered view).
    ///
    /// An id is a stable handle on an item in the underlying source (an index
    /// into a `Vec`, a database row id, …) — it is what a selection reports
    /// back, so callers get something that holds regardless of the current
    /// filter/order.
    fn search(&self, query: &str, limit: usize) -> Vec<usize>;

    /// The line to display for the item with this id. Asked for the rows on
    /// screen only, so a source is free to build it on the spot.
    fn get_text(&self, id: usize) -> String;

    /// Free-form detail about the item with this id, shown in the
    /// multi-line field below the list as the selection moves. Newlines are
    /// honoured and long lines wrap. The default returns nothing, which hides
    /// the field entirely — implement it to describe the highlighted item.
    fn get_info(&self, _id: usize) -> String {
        String::new()
    }

    /// The record behind the item with this id, borrowed from the source, or
    /// `None` when there is nothing behind it (the default) or the id is not
    /// one of ours. Borrowed rather than cloned: the picker asks for it as the
    /// selection moves, and a record can be a good deal bigger than a row.
    fn get_data(&self, _id: usize) -> Option<&T> {
        None
    }
}

/// Simple in-memory source: case-insensitive substring match over a
/// `Vec<String>`. Lowercased copies are precomputed once so each keystroke is a
/// linear scan of cheap `contains` checks. Good enough up to a few thousand
/// entries; swap in an indexed source beyond that.
pub struct SubstringSource {
    items: Vec<String>,
    lowercased: Vec<String>,
}

impl SubstringSource {
    #[allow(dead_code)]
    pub fn new(items: Vec<String>) -> Self {
        let lowercased = items.iter().map(|s| s.to_lowercase()).collect();
        Self { items, lowercased }
    }

    /// The matching ids. Inherent as well as trait method so a caller holding
    /// the source itself needn't say which `T` it means -- the source carries
    /// no records, so it implements [`FuzzySource`] for all of them.
    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let q = query.trim().to_lowercase();
        self.lowercased
            .iter()
            .enumerate()
            .filter(|(_, s)| q.is_empty() || s.contains(&q))
            .take(limit)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn get_text(&self, id: usize) -> String {
        self.items[id].clone()
    }
}

impl From<Vec<String>> for SubstringSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

impl<T> FuzzySource<T> for SubstringSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.get_text(id)
    }
}

/// In-memory source that matches on all query words independently: the query is
/// split on whitespace and an item matches only if it contains every word as a
/// (case-insensitive) substring, in any order. So `"na an"` matches `"banana"`.
/// Like [`SubstringSource`] it precomputes lowercased copies and scans linearly;
/// good up to a few thousand entries.
pub struct AllWordsSource {
    items: Vec<String>,
    lowercased: Vec<String>,
}

impl AllWordsSource {
    #[allow(dead_code)]
    pub fn new(items: Vec<String>) -> Self {
        let lowercased = items.iter().map(|s| s.to_lowercase()).collect();
        Self { items, lowercased }
    }

    /// See [`SubstringSource::search`] for why this is inherent too.
    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.lowercased
            .iter()
            .enumerate()
            .filter(|(_, s)| words.iter().all(|w| s.contains(w)))
            .take(limit)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn get_text(&self, id: usize) -> String {
        self.items[id].clone()
    }
}

impl From<Vec<String>> for AllWordsSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

impl<T> FuzzySource<T> for AllWordsSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.get_text(id)
    }
}

/// Number of pre-filtered candidates ranked per query. The trigram index
/// normally prunes far below this; the cap only bites on very broad queries
/// (e.g. a single common short word), bounding the work so a keystroke always
/// stays well under a frame. Matches past the cap are ignored — a picker gets
/// narrowed by typing more, not by scrolling thousands of rows.
const RANK_CAP: usize = 4096;

/// High-performance source for large lists (tens of thousands of entries).
///
/// Filtering is split into a fast **prune** and a cheap **rank**:
///
/// 1. A precomputed inverted **trigram index** maps every 3-byte substring to
///    the sorted ids of items containing it. A query word jumps straight to the
///    handful of items that could contain it (intersecting its trigrams'
///    posting lists) instead of scanning all items. This is what makes it an
///    order of magnitude faster than the linear `contains` scan of
///    [`AllWordsSource`] — measured ~14x overall and 13–27x on selective
///    multi-word queries over 40k entries, since those are exactly the case
///    where a linear scan must touch *every* item because few match.
/// 2. The small surviving candidate set is verified against the real substring
///    predicate (trigram membership is necessary but not sufficient) and then
///    ranked by [`nucleo_matcher`] (the matcher behind Helix) so results come
///    back best-match-first rather than in arbitrary id order.
///
/// Matching semantics match [`AllWordsSource`]: the query splits on whitespace
/// and an item must contain every word (case-insensitively), in any order.
/// Words shorter than a trigram (1–2 chars) can't be indexed, so a query made
/// only of such words falls back to a linear scan — fine, because those queries
/// match many items and hit [`RANK_CAP`] almost immediately.
/// Cheaply clonable (`Arc`): building the trigram index is the expensive part,
/// so a caller that reopens the same picker can build one `IndexedSource` and
/// clone it per open instead of re-indexing.
#[derive(Clone)]
pub struct IndexedSource {
    inner: Arc<IndexedData>,
}

struct IndexedData {
    items: Vec<String>,
    lowercased: Vec<String>,
    /// Inverted index: byte-trigram → ascending ids of items whose lowercased
    /// text contains it. Built once in [`IndexedSource::new`].
    postings: HashMap<[u8; 3], Vec<u32>>,
}

impl IndexedSource {
    pub fn new(items: Vec<String>) -> Self {
        let lowercased: Vec<String> = items.iter().map(|s| s.to_lowercase()).collect();
        let mut postings: HashMap<[u8; 3], Vec<u32>> = HashMap::new();
        for (i, s) in lowercased.iter().enumerate() {
            // Sliding window over bytes: UTF-8 is self-synchronizing, so byte
            // trigrams are a sound necessary condition for a str substring
            // match, and the verify step re-checks with real `str::contains`.
            for w in s.as_bytes().windows(3) {
                let v = postings.entry([w[0], w[1], w[2]]).or_default();
                // Each item id is pushed at most once per trigram; ids are
                // inserted in increasing order, so posting lists stay sorted.
                if v.last() != Some(&(i as u32)) {
                    v.push(i as u32);
                }
            }
        }
        Self {
            inner: Arc::new(IndexedData {
                items,
                lowercased,
                postings,
            }),
        }
    }

    /// Ids that could contain the word `wb` (>= 3 bytes): the intersection of
    /// its trigrams' posting lists. Empty if any trigram is absent everywhere.
    fn word_candidates(&self, wb: &[u8]) -> Vec<u32> {
        let mut lists: Vec<&Vec<u32>> = Vec::new();
        for w in wb.windows(3) {
            match self.inner.postings.get(&[w[0], w[1], w[2]]) {
                Some(l) => lists.push(l),
                None => return Vec::new(),
            }
        }
        // Intersect shortest-first so the working set only shrinks.
        lists.sort_by_key(|l| l.len());
        let mut acc: Vec<u32> = lists[0].clone();
        for l in &lists[1..] {
            acc = intersect(&acc, l);
            if acc.is_empty() {
                break;
            }
        }
        acc
    }

    /// See [`SubstringSource::search`] for why this is inherent too.
    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        // Empty/whitespace query: unfiltered head of the list, in original order.
        if words.is_empty() {
            return (0..self.inner.items.len().min(limit)).collect();
        }

        // Prune to candidate ids using every word long enough to be indexed
        // (>= 3 bytes). Shorter words are still enforced in the verify step.
        let mut candidates: Option<Vec<u32>> = None;
        for w in &words {
            if w.len() < 3 {
                continue;
            }
            let wc = self.word_candidates(w.as_bytes());
            candidates = Some(match candidates {
                None => wc,
                Some(c) => intersect(&c, &wc),
            });
        }

        let verify = |i: u32| {
            let s = &self.inner.lowercased[i as usize];
            words.iter().all(|w| s.contains(w))
        };

        // No indexable word (query is all 1–2 char words): there's nothing to
        // prune on and fuzzy-ranking a 1-char query is meaningless, so just take
        // the first `limit` matches in id order — exactly what `AllWordsSource`
        // does, and just as fast (such queries match plenty, so we stop early).
        let Some(ids) = candidates else {
            let mut out = Vec::new();
            for i in 0..self.inner.items.len() as u32 {
                if verify(i) {
                    out.push(i as usize);
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            return out;
        };

        // Indexed path: verify the real predicate on the pruned candidate set
        // (trigram membership is necessary but not sufficient), keeping up to
        // RANK_CAP matches in id order.
        let mut matched: Vec<u32> = Vec::new();
        for i in ids {
            if verify(i) {
                matched.push(i);
                if matched.len() >= RANK_CAP {
                    break;
                }
            }
        }

        // Rank the survivors by fuzzy match quality, best first, tie-broken on
        // id for a stable order. The set is tiny after pruning, so this is cheap.
        let pattern = Pattern::parse(&q, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, u32)> = matched
            .into_iter()
            .map(|i| {
                let score = pattern
                    .score(
                        Utf32Str::new(&self.inner.lowercased[i as usize], &mut buf),
                        &mut matcher,
                    )
                    .unwrap_or(0);
                (score, i)
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.truncate(limit);
        scored.into_iter().map(|(_, i)| i as usize).collect()
    }

    pub fn get_text(&self, id: usize) -> String {
        self.inner.items[id].clone()
    }
}

impl From<Vec<String>> for IndexedSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

/// Intersect two ascending, de-duplicated id lists into a new ascending list.
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

impl<T> FuzzySource<T> for IndexedSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.get_text(id)
    }
}

#[cfg(test)]
#[path = "tests/fuzzy_list_tests.rs"]
mod tests;
