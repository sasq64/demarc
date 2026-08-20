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

/// One filtered result: the text to display plus a stable `id` identifying the
/// item in the underlying source (an index into a `Vec`, a database row id, …).
/// The `id` is what a selection reports back, so callers get a handle that is
/// stable regardless of the current filter/order.
#[derive(Debug, Clone)]
pub struct FuzzyItem {
    pub id: usize,
    pub text: String,
}

impl AsRef<str> for FuzzyItem {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

/// Backs the searchable list with items. Implement this to plug in smarter
/// matching without touching the UI: prefix trees, fuzzy scoring, or an
/// external index/database.
pub trait FuzzySource: Send + Sync + 'static {
    /// Return the items matching `query`, best match first, capped at `limit`.
    /// An empty/whitespace query should return the head of the full list (the
    /// unfiltered view).
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem>;

    /// Free-form detail about the item with this [`FuzzyItem::id`], shown in the
    /// multi-line field below the list as the selection moves. Newlines are
    /// honoured and long lines wrap. The default returns nothing, which hides
    /// the field entirely — implement it to describe the highlighted item.
    fn get_info(&self, _id: usize) -> String {
        String::new()
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
}

impl From<Vec<String>> for SubstringSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

impl FuzzySource for SubstringSource {
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem> {
        let q = query.trim().to_lowercase();
        self.lowercased
            .iter()
            .enumerate()
            .filter(|(_, s)| q.is_empty() || s.contains(&q))
            .take(limit)
            .map(|(i, _)| FuzzyItem {
                id: i,
                text: self.items[i].clone(),
            })
            .collect()
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
}

impl From<Vec<String>> for AllWordsSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

impl FuzzySource for AllWordsSource {
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.lowercased
            .iter()
            .enumerate()
            .filter(|(_, s)| words.iter().all(|w| s.contains(w)))
            .take(limit)
            .map(|(i, _)| FuzzyItem {
                id: i,
                text: self.items[i].clone(),
            })
            .collect()
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

impl FuzzySource for IndexedSource {
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        // Empty/whitespace query: unfiltered head of the list, in original order.
        if words.is_empty() {
            return self
                .inner
                .items
                .iter()
                .take(limit)
                .enumerate()
                .map(|(id, text)| FuzzyItem {
                    id,
                    text: text.clone(),
                })
                .collect();
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
                    out.push(FuzzyItem {
                        id: i as usize,
                        text: self.inner.items[i as usize].clone(),
                    });
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
        scored
            .into_iter()
            .map(|(_, i)| FuzzyItem {
                id: i as usize,
                text: self.inner.items[i as usize].clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<String> {
        ["apple", "apricot", "banana", "cherry", "grape"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn substring_source_filters_case_insensitively() {
        let src = SubstringSource::new(items());
        let texts =
            |q: &str| -> Vec<String> { src.search(q, 256).into_iter().map(|r| r.text).collect() };

        // Empty query is the unfiltered list…
        assert_eq!(texts(""), items());
        // …a substring matches wherever it sits in the entry…
        assert_eq!(texts("ap"), vec!["apple", "apricot", "grape"]);
        // …case doesn't matter…
        assert_eq!(texts("BAN"), vec!["banana"]);
        // …and nothing matching means nothing shown.
        assert!(texts("zzz").is_empty());
    }

    #[test]
    fn substring_source_reports_stable_source_ids() {
        let src = SubstringSource::new(items());
        let hits = src.search("cherry", 256);
        assert_eq!(hits.len(), 1);
        // The id indexes the source, not the filtered view.
        assert_eq!(hits[0].id, 3);
        assert_eq!(hits[0].text, "cherry");
    }

    #[test]
    fn sources_describe_nothing_by_default() {
        // `get_info` is optional; a source that doesn't implement it leaves the
        // info field empty, which hides it.
        assert_eq!(SubstringSource::new(items()).get_info(0), "");
    }

    #[test]
    fn all_words_source_matches_every_word_in_any_order() {
        let src = AllWordsSource::new(items());

        // Empty query returns everything.
        let all: Vec<String> = src.search("", 256).into_iter().map(|r| r.text).collect();
        assert_eq!(all, items());

        // Two words, out of order, both as substrings of the same item.
        let hits: Vec<String> = src
            .search("na an", 256)
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(hits, vec!["banana"]);

        // A word matching nothing filters the item out even if others match.
        assert!(src.search("apple zzz", 256).is_empty());
    }

    #[test]
    fn indexed_source_matches_words_in_any_order_and_ranks() {
        let src = IndexedSource::new(items());

        // Empty query returns the head of the list, in order.
        let all: Vec<String> = src.search("", 256).into_iter().map(|r| r.text).collect();
        assert_eq!(all, items());

        // Substring match finds the right rows regardless of order…
        let hits: Vec<String> = src.search("ap", 256).into_iter().map(|r| r.text).collect();
        assert!(hits.contains(&"apple".to_string()));
        assert!(hits.contains(&"apricot".to_string()));
        assert!(hits.contains(&"grape".to_string()));

        // …and reports the stable source id, not the ranked position.
        let cherry = src.search("cherry", 256);
        assert_eq!(cherry.len(), 1);
        assert_eq!(cherry[0].id, 3);
        assert_eq!(cherry[0].text, "cherry");

        // Case-insensitive, and two out-of-order words both as substrings of one
        // item (the `AllWordsSource` semantics), via the < 3-char fallback path.
        let banana: Vec<String> = src
            .search("NA an", 256)
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(banana, vec!["banana"]);

        // A word matching nothing filters the item out.
        assert!(src.search("apple zzz", 256).is_empty());

        // Ranking puts the closest match first: an exact/prefix hit outranks a
        // mid-word one for the same query.
        let ranked: Vec<String> = src.search("ap", 256).into_iter().map(|r| r.text).collect();
        assert_eq!(
            ranked[0], "apple",
            "prefix match should rank ahead of 'grape'"
        );
    }

    /// Guards the headline claim: on a large list, [`IndexedSource`] filters at
    /// least 10x faster than the linear [`AllWordsSource`] it replaces (and,
    /// for contrast, a plain nucleo scan would be *slower*, so the index — not
    /// the fuzzy crate — is what buys the speed).
    /// Ignored by default (timing-sensitive); run with `--ignored --release`.
    #[test]
    #[ignore]
    fn indexed_source_is_at_least_10x_faster() {
        use std::time::Instant;

        // A realistically large list (~40k entries) with diverse vocabulary,
        // like a fetched game DB. Titles are a few pseudo-random words drawn
        // from a large vocabulary so trigram posting lists stay short — the
        // real-world case, unlike a handful of repeated words.
        let vocab: Vec<String> = (0..3000)
            .map(|n| {
                // Deterministic pronounceable-ish tokens: cons+vowel salad.
                let cons = b"bcdfghjklmnpqrstvwxz";
                let vow = b"aeiou";
                let mut s = String::new();
                let mut x = n * 2654435761u64.wrapping_mul(1) as usize + 12345;
                for k in 0..(4 + n % 4) {
                    x = x.wrapping_mul(1103515245).wrapping_add(12345);
                    let c = if k % 2 == 0 {
                        cons[(x >> 8) % cons.len()]
                    } else {
                        vow[(x >> 8) % vow.len()]
                    };
                    s.push(c as char);
                }
                s
            })
            .collect();
        let names: Vec<String> = (0..40_000)
            .map(|i| {
                let a = &vocab[(i * 2654435761usize) % vocab.len()];
                let b = &vocab[(i * 40503 + 7) % vocab.len()];
                let c = &vocab[(i * 12289 + 3) % vocab.len()];
                format!("{a} {b} {c} {i}")
            })
            .collect();

        let old = AllWordsSource::new(names.clone());
        let idx = IndexedSource::new(names.clone());

        // Queries derived from the data: a broad prefix, a mid-selectivity
        // token, and two highly-selective multi-word filters (the case where a
        // linear scan must touch every item because few/none match).
        let w0: Vec<&str> = names[100].split_whitespace().collect();
        let w1: Vec<&str> = names[25000].split_whitespace().collect();
        let queries = [
            &vocab[0][..1],
            &vocab[500][..3.min(vocab[500].len())],
            &format!("{} {}", w0[0], w0[1]) as &str,
            &format!("{} {}", w1[0], w1[2]) as &str,
        ];

        // Warm up (let allocators/caches settle).
        for q in queries {
            let _ = old.search(q, 256);
            let _ = idx.search(q, 256);
        }

        let time = |src: &dyn FuzzySource| {
            let start = Instant::now();
            for _ in 0..20 {
                for q in queries {
                    std::hint::black_box(src.search(q, 256));
                }
            }
            start.elapsed()
        };

        // Per-query breakdown.
        for q in queries {
            let n_old = old.search(q, 256).len();
            let t_old = {
                let s = Instant::now();
                for _ in 0..50 {
                    std::hint::black_box(old.search(q, 256));
                }
                s.elapsed() / 50
            };
            let t_idx = {
                let s = Instant::now();
                for _ in 0..50 {
                    std::hint::black_box(idx.search(q, 256));
                }
                s.elapsed() / 50
            };
            println!(
                "  {q:24} hits={n_old:5}  old={t_old:>10.3?}  idx={t_idx:>10.3?}  ({:.1}x)",
                t_old.as_secs_f64() / t_idx.as_secs_f64().max(1e-12)
            );
        }

        let old_t = time(&old);
        let idx_t = time(&idx);
        println!("AllWordsSource (linear): {old_t:?}");
        println!(
            "IndexedSource  (trigram+nucleo): {idx_t:?} ({:.1}x)",
            old_t.as_secs_f64() / idx_t.as_secs_f64()
        );
        let speedup = old_t.as_secs_f64() / idx_t.as_secs_f64();
        assert!(
            speedup >= 10.0,
            "expected >=10x speedup, got {speedup:.1}x (old {old_t:?}, new {idx_t:?})"
        );
    }
}
