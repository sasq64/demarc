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
    let texts = |q: &str| -> Vec<String> {
        src.search(q, 256)
            .into_iter()
            .map(|id| src.get_text(id))
            .collect()
    };

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
    assert_eq!(hits[0], 3);
    assert_eq!(src.get_text(hits[0]), "cherry");
}

#[test]
fn sources_describe_nothing_by_default() {
    // `get_info` is optional; a source that doesn't implement it leaves the
    // info field empty, which hides it. Same for `get_data`: a plain list
    // of strings has no record behind a row.
    let src = SubstringSource::new(items());
    let src: &dyn FuzzySource = &src;
    assert_eq!(src.get_info(0), "");
    assert_eq!(src.get_data(0), None);
}

#[test]
fn all_words_source_matches_every_word_in_any_order() {
    let src = AllWordsSource::new(items());
    let texts = |q: &str| -> Vec<String> {
        src.search(q, 256)
            .into_iter()
            .map(|id| src.get_text(id))
            .collect()
    };

    // Empty query returns everything.
    assert_eq!(texts(""), items());

    // Two words, out of order, both as substrings of the same item.
    assert_eq!(texts("na an"), vec!["banana"]);

    // A word matching nothing filters the item out even if others match.
    assert!(src.search("apple zzz", 256).is_empty());
}

#[test]
fn indexed_source_matches_words_in_any_order_and_ranks() {
    let src = IndexedSource::new(items());
    let texts = |q: &str| -> Vec<String> {
        src.search(q, 256)
            .into_iter()
            .map(|id| src.get_text(id))
            .collect()
    };

    // Empty query returns the head of the list, in order.
    assert_eq!(texts(""), items());

    // Substring match finds the right rows regardless of order…
    let hits = texts("ap");
    assert!(hits.contains(&"apple".to_string()));
    assert!(hits.contains(&"apricot".to_string()));
    assert!(hits.contains(&"grape".to_string()));

    // …and reports the stable source id, not the ranked position.
    let cherry = src.search("cherry", 256);
    assert_eq!(cherry.len(), 1);
    assert_eq!(cherry[0], 3);
    assert_eq!(src.get_text(cherry[0]), "cherry");

    // Case-insensitive, and two out-of-order words both as substrings of one
    // item (the `AllWordsSource` semantics), via the < 3-char fallback path.
    assert_eq!(texts("NA an"), vec!["banana"]);

    // A word matching nothing filters the item out.
    assert!(src.search("apple zzz", 256).is_empty());

    // Ranking puts the closest match first: an exact/prefix hit outranks a
    // mid-word one for the same query.
    let ranked = texts("ap");
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
