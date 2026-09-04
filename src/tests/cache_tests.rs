use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An hour, as an age limit for the expiry tests.
const HOUR: Duration = Duration::from_secs(3600);

/// A cache rooted in a temp dir rather than under the user's real one, with
/// a budget far past anything these tests write.
fn temp_cache() -> (tempfile::TempDir, FileCache) {
    let dir = tempfile::tempdir().unwrap();
    let cache = FileCache::at(dir.path().join("cache"), 1024 * 1024);
    (dir, cache)
}

/// Backdate `path`'s mtime by `hours`, so [`FileCache::prune`] sees it as
/// the least recently used entry.
fn age(path: &Path, hours: u64) {
    let when = SystemTime::now() - Duration::from_secs(hours * 3600);
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}

/// Backdate a whole entry — its payload *and* its stamp, since an entry's
/// last-use time is the newest mtime anywhere inside it.
fn age_all(payload: &Path, hours: u64) {
    age(payload, hours);
    age(&payload.with_file_name(STAMP), hours);
}

/// Backdate the *entry* in `dir` by `hours` — its production stamp rather
/// than its last-use mtimes, which is what the age limit reads.
fn age_entry(dir: &Path, hours: u64) {
    let when = SystemTime::now() - Duration::from_secs(hours * 3600);
    let secs = when.duration_since(UNIX_EPOCH).unwrap().as_secs();
    std::fs::write(dir.join(STAMP), format!("{secs}\n")).unwrap();
}

#[test]
fn produces_once_and_then_hits() {
    let (_tmp, cache) = temp_cache();
    let calls = AtomicUsize::new(0);
    let produce = |dest: &Path| {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(std::fs::write(dest, b"payload")?)
    };

    let first = cache
        .get_file("http://x/game.zip", "game.zip", produce)
        .unwrap();
    assert_eq!(std::fs::read(&first).unwrap(), b"payload");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let second = cache
        .get_file("http://x/game.zip", "game.zip", produce)
        .unwrap();
    assert_eq!(second, first);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "a hit must not produce");
}

/// Two urls sharing a last segment must not share an entry — the whole
/// point of hashing the key rather than naming the entry after the file.
#[test]
fn distinct_keys_get_distinct_entries() {
    let (_tmp, cache) = temp_cache();
    let write = |text: &'static str| {
        move |dest: &Path| -> Result<()> { Ok(std::fs::write(dest, text)?) }
    };
    let a = cache
        .get_file("http://x/v1/game.zip", "game.zip", write("one"))
        .unwrap();
    let b = cache
        .get_file("http://x/v2/game.zip", "game.zip", write("two"))
        .unwrap();
    assert_ne!(a, b);
    assert_eq!(std::fs::read(&a).unwrap(), b"one");
    assert_eq!(std::fs::read(&b).unwrap(), b"two");
}

/// A failed producer must leave nothing that a later lookup would mistake
/// for a finished entry.
#[test]
fn a_failed_produce_leaves_no_entry() {
    let (_tmp, cache) = temp_cache();
    let err = cache.get_file("k", "out.bin", |dest| {
        // Half a file, then failure — exactly the shape a cut-off download
        // leaves behind.
        std::fs::write(dest, b"trunc")?;
        anyhow::bail!("mirror died")
    });
    assert!(err.is_err());

    let calls = AtomicUsize::new(0);
    let path = cache
        .get_file("k", "out.bin", |dest| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(std::fs::write(dest, b"complete")?)
        })
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the retry must produce");
    assert_eq!(std::fs::read(&path).unwrap(), b"complete");
}

#[test]
fn get_dir_publishes_a_whole_directory() {
    let (_tmp, cache) = temp_cache();
    let calls = AtomicUsize::new(0);
    let produce = |dir: &Path| -> Result<()> {
        calls.fetch_add(1, Ordering::Relaxed);
        std::fs::write(dir.join("track01.wav"), b"pcm")?;
        std::fs::write(dir.join("disc.cue"), b"FILE \"track01.wav\" WAVE")?;
        Ok(())
    };

    let entry = cache.get_dir("disc", "disc.cue", produce).unwrap();
    assert!(entry.join("track01.wav").is_file());
    assert!(entry.join("disc.cue").is_file());

    assert_eq!(cache.get_dir("disc", "disc.cue", produce).unwrap(), entry);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "a hit must not produce");

    // Nothing dotted should survive a successful publish.
    let leftovers: Vec<_> = std::fs::read_dir(&cache.root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging dirs left behind: {leftovers:?}"
    );
}

/// An entry whose marker never landed is a build that died partway. It has
/// to be rebuilt, not served.
#[test]
fn get_dir_rebuilds_an_entry_missing_its_marker() {
    let (_tmp, cache) = temp_cache();
    let err = cache.get_dir("disc", "disc.cue", |dir| {
        std::fs::write(dir.join("track01.wav"), b"pcm")?;
        anyhow::bail!("transcode failed")
    });
    assert!(err.is_err());

    let entry = cache
        .get_dir("disc", "disc.cue", |dir| {
            std::fs::write(dir.join("disc.cue"), b"cue")?;
            Ok(())
        })
        .unwrap();
    assert!(entry.join("disc.cue").is_file());
    assert!(
        !entry.join("track01.wav").exists(),
        "the abandoned build's files must not survive into the rebuilt entry"
    );
}

/// With no age limit — the default — an entry stays a hit however old the
/// thing upstream has got.
#[test]
fn without_a_max_age_nothing_expires() {
    let (_tmp, cache) = temp_cache();
    let path = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"one")?))
        .unwrap();
    age_entry(path.parent().unwrap(), 24 * 365);
    cache.get_file("k", "a.zip", |_| unreachable!()).unwrap();
}

#[test]
fn a_file_within_its_max_age_is_still_a_hit() {
    let (_tmp, cache) = temp_cache();
    let cache = cache.with_max_age(HOUR);
    cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"one")?))
        .unwrap();
    let path = cache.get_file("k", "a.zip", |_| unreachable!()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"one");
}

/// Past the age limit the entry stops counting as present, so the producer
/// runs and its result replaces what was there.
#[test]
fn an_expired_file_is_produced_again() {
    let (_tmp, cache) = temp_cache();
    let cache = cache.with_max_age(HOUR);
    let path = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"one")?))
        .unwrap();
    age_entry(path.parent().unwrap(), 2);

    let fresh = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"two")?))
        .unwrap();
    assert_eq!(fresh, path);
    assert_eq!(std::fs::read(&fresh).unwrap(), b"two");

    // And the refreshed entry is fresh again, so it hits without producing.
    cache.get_file("k", "a.zip", |_| unreachable!()).unwrap();
}

/// Expiry is not deletion: if the replacement can't be produced — offline,
/// mirror down — the old copy is still the best there is.
#[test]
fn an_expired_file_survives_a_failed_refresh() {
    let (_tmp, cache) = temp_cache();
    let cache = cache.with_max_age(HOUR);
    let path = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"one")?))
        .unwrap();
    age_entry(path.parent().unwrap(), 2);

    let stale = cache
        .get_file("k", "a.zip", |_| anyhow::bail!("no route to host"))
        .unwrap();
    assert_eq!(stale, path);
    assert_eq!(std::fs::read(&stale).unwrap(), b"one");
    assert!(
        !path.with_file_name(".a.zip.part").exists(),
        "the failed attempt must not leave a staging file"
    );
}

#[test]
fn an_expired_dir_is_rebuilt_and_survives_a_failed_rebuild() {
    let (_tmp, cache) = temp_cache();
    let cache = cache.with_max_age(HOUR);
    let entry = cache
        .get_dir("disc", "disc.cue", |dir| {
            std::fs::write(dir.join("old.bin"), b"pcm")?;
            std::fs::write(dir.join("disc.cue"), b"one")?;
            Ok(())
        })
        .unwrap();
    age_entry(&entry, 2);

    // A rebuild that fails leaves the old entry exactly as it was.
    let kept = cache
        .get_dir("disc", "disc.cue", |_| anyhow::bail!("no route to host"))
        .unwrap();
    assert_eq!(kept, entry);
    assert_eq!(std::fs::read(entry.join("disc.cue")).unwrap(), b"one");

    // One that succeeds replaces it wholesale, old files included.
    age_entry(&entry, 2);
    let rebuilt = cache
        .get_dir("disc", "disc.cue", |dir| {
            std::fs::write(dir.join("disc.cue"), b"two")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(rebuilt, entry);
    assert_eq!(std::fs::read(entry.join("disc.cue")).unwrap(), b"two");
    assert!(
        !entry.join("old.bin").exists(),
        "the replaced entry's files must not survive the rebuild"
    );
    // The moved-aside copy goes once the rebuild is published.
    let leftovers: Vec<_> = std::fs::read_dir(&cache.root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.') && n != LIMIT_FILE)
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

/// The age limit measures from when an entry was produced, not from when it
/// was last used — otherwise a popular entry would never refresh.
#[test]
fn a_hit_does_not_extend_an_entrys_age() {
    let (_tmp, cache) = temp_cache();
    let cache = cache.with_max_age(HOUR);
    let path = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"one")?))
        .unwrap();
    age_entry(path.parent().unwrap(), 2);
    // A hit that touches the payload's mtime...
    cache
        .get_file("k", "a.zip", |_| anyhow::bail!("offline"))
        .unwrap();
    // ...must still leave the entry expired.
    let calls = AtomicUsize::new(0);
    cache
        .get_file("k", "a.zip", |dest| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(std::fs::write(dest, b"two")?)
        })
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn prunes_least_recently_used_until_under_limit() {
    let tmp = tempfile::tempdir().unwrap();
    // Three 100-byte entries, aged 3h / 2h / 1h ago — plus each entry's
    // stamp, so the budget has a little slack over the round 300.
    let cache = FileCache::at(tmp.path().join("cache"), 400);
    let mut entries = Vec::new();
    for (key, hours) in [("old", 3), ("mid", 2), ("new", 1)] {
        let path = cache
            .get_file(key, "a.zip", |dest| {
                Ok(std::fs::write(dest, vec![0u8; 100])?)
            })
            .unwrap();
        // The stamp too: an entry's last-use time is the newest mtime
        // anywhere inside it, so an entry stamped a moment ago is a recent
        // one however far back its payload is dated.
        age_all(&path, hours);
        entries.push(path);
    }
    let [old, mid, new] = <[PathBuf; 3]>::try_from(entries).unwrap();

    // Under the limit: nothing is touched.
    cache.prune();
    assert!(old.exists());

    // The budget is now on disk, where a user can change it...
    let limit = cache.root.join(LIMIT_FILE);
    assert_eq!(std::fs::read_to_string(&limit).unwrap().trim(), "400");
    std::fs::write(&limit, "150\n").unwrap();

    // ...and the edited one wins: evict oldest first, stopping as soon as
    // we're back under it.
    cache.prune();
    assert!(!old.exists());
    assert!(!mid.exists());
    assert!(new.exists());
    assert!(limit.is_file(), "the limit itself must not be evicted");
}

/// The budget file is not an entry, and evicting it would quietly throw
/// away a limit the user set by hand.
#[test]
fn a_limit_file_is_not_counted_as_an_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::at(tmp.path().join("cache"), 0);
    cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"x")?))
        .unwrap();
    cache.prune();
    assert!(cache.root.join(LIMIT_FILE).is_file());
}

#[test]
fn size_limits_are_written_and_read_in_human_units() {
    assert_eq!(parse_size("500M\n"), Some(500 * 1024 * 1024));
    assert_eq!(parse_size(" 2g "), Some(2 * 1024 * 1024 * 1024));
    assert_eq!(parse_size("500MB"), Some(500 * 1024 * 1024));
    assert_eq!(parse_size("1024"), Some(1024));
    assert_eq!(parse_size("lots"), None);
    assert_eq!(parse_size(""), None);
    assert_eq!(parse_size("99999999999999999999G"), None, "overflow");

    // Whatever demarc writes has to read back as the same number.
    for size in [500 * 1024 * 1024, 2 * 1024 * 1024 * 1024, 1500, 0] {
        assert_eq!(parse_size(&format_size(size)), Some(size));
    }
    assert_eq!(format_size(500 * 1024 * 1024), "500M");
}

/// A hit is what keeps an entry alive, so it has to move the clock.
#[test]
fn a_hit_marks_an_entry_as_recently_used() {
    let (_tmp, cache) = temp_cache();
    let path = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"x")?))
        .unwrap();
    age(&path, 1);

    let (_, before) = entry_stats(&path);
    cache.get_file("k", "a.zip", |_| unreachable!()).unwrap();
    let (_, after) = entry_stats(&path);
    assert!(after > before);
}

/// An entry's size and age come from everything inside it, not just its
/// own directory stat.
#[test]
fn entry_stats_sums_a_directory() {
    let (_tmp, cache) = temp_cache();
    let entry = cache
        .get_dir("k", "disc.cue", |dir| {
            std::fs::write(dir.join("a.bin"), vec![0u8; 100])?;
            std::fs::write(dir.join("disc.cue"), vec![0u8; 200])?;
            Ok(())
        })
        .unwrap();
    age(&entry.join("a.bin"), 1);
    age(&entry.join("disc.cue"), 1);
    age(&entry.join(STAMP), 1);
    let stamp_size = std::fs::metadata(entry.join(STAMP)).unwrap().len();
    let (size, old) = entry_stats(&entry);
    assert_eq!(size, 300 + stamp_size);

    touch(&entry.join("disc.cue"));
    let (_, used) = entry_stats(&entry);
    assert!(used > old, "the newest file inside sets the entry's age");
}

/// Each size band is measured and evicted on its own, so a flood of small
/// entries can't push out the big ones — or the reverse.
#[test]
fn prunes_each_size_band_against_its_own_budget() {
    let tmp = tempfile::tempdir().unwrap();
    // Entries up to 1000 bytes share 250; everything above shares 4500.
    let cache = FileCache::at(tmp.path().join("cache"), 4500).with_level(1000, 250);
    let mut paths = Vec::new();
    for (key, hours, size) in [
        ("s1", 3, 100),
        ("s2", 2, 100),
        ("s3", 1, 100),
        ("b1", 3, 2000),
        ("b2", 2, 2000),
        ("b3", 1, 2000),
    ] {
        let path = cache
            .get_file(key, "a.bin", |dest| {
                Ok(std::fs::write(dest, vec![0u8; size])?)
            })
            .unwrap();
        age_all(&path, hours);
        paths.push(path);
    }
    let [s1, s2, s3, b1, b2, b3] = <[PathBuf; 6]>::try_from(paths).unwrap();

    cache.prune();
    // Three ~111-byte entries over a 250-byte budget: the oldest goes, and
    // only the oldest, because two of them fit.
    assert!(!s1.exists());
    assert!(s2.exists() && s3.exists());
    // Three ~2011-byte entries over 4500 does the same in the other band —
    // which it would not if the two bands shared a budget.
    assert!(!b1.exists());
    assert!(b2.exists() && b3.exists());
}

/// What a run that was killed mid-download leaves behind is not an entry:
/// nothing can ever hit it, so it goes on sight rather than waiting to
/// become the oldest thing in the cache.
#[test]
fn prune_deletes_partial_and_empty_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::at(tmp.path().join("cache"), 1024 * 1024);
    let keep = cache
        .get_file("k", "a.zip", |dest| Ok(std::fs::write(dest, b"payload")?))
        .unwrap();

    // A staging directory from an interrupted `get_dir`...
    let staging = cache.root.join(".deadbeef00000000.part");
    std::fs::create_dir_all(staging.join("sub")).unwrap();
    std::fs::write(staging.join("disc.cue"), b"half").unwrap();
    age(&staging.join("disc.cue"), 2);
    // ...the staging file an interrupted `get_file` leaves inside its own
    // entry, next to no payload at all...
    let orphan = cache.root.join("0123456789abcdef");
    std::fs::create_dir(&orphan).unwrap();
    std::fs::write(orphan.join(".a.zip.part"), vec![0u8; 4096]).unwrap();
    age(&orphan.join(".a.zip.part"), 2);
    // ...the same, in an entry that does have a payload to keep...
    let dead_part = keep.with_file_name(".a.zip.part");
    std::fs::write(&dead_part, vec![0u8; 4096]).unwrap();
    age(&dead_part, 2);
    // ...and an entry nothing ever wrote into.
    let empty = cache.root.join("fedcba9876543210");
    std::fs::create_dir(&empty).unwrap();

    cache.prune();
    assert!(!staging.exists());
    assert!(!dead_part.exists());
    assert!(!orphan.exists(), "an entry that is only a staging file");
    assert!(!empty.exists());
    assert_eq!(std::fs::read(&keep).unwrap(), b"payload");
}

/// A `.part` being written *right now* belongs to another demarc's
/// download, not to a run that died: deleting it would fail that transfer.
#[test]
fn prune_leaves_a_download_still_in_flight_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::at(tmp.path().join("cache"), 1024 * 1024);
    let entry = cache.root.join("0123456789abcdef");
    std::fs::create_dir_all(&entry).unwrap();
    let part = entry.join(".big.iso.part");
    std::fs::write(&part, vec![0u8; 4096]).unwrap();
    let staging = cache.root.join(".fedcba9876543210.part");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("disc.cue"), b"partway").unwrap();

    cache.prune();
    assert!(part.is_file());
    assert!(staging.join("disc.cue").is_file());
}

#[test]
fn size_bands_are_written_and_read_back() {
    let levels = parse_levels("1M=250M\n750M\n").unwrap();
    assert_eq!(
        levels,
        [
            Level {
                max_entry: 1024 * 1024,
                budget: 250 * 1024 * 1024
            },
            Level {
                max_entry: u64::MAX,
                budget: 750 * 1024 * 1024
            },
        ]
    );
    // Order on disk doesn't matter; the passes run smallest band first.
    assert_eq!(
        parse_levels("750M\n# a comment\n\n1M=250M\n").unwrap(),
        levels
    );
    assert_eq!(format_levels(&levels), "1M=250M\n750M\n");
    assert_eq!(parse_levels(&format_levels(&levels)).unwrap(), levels);
    // A single bare budget is what older caches already have on disk.
    assert_eq!(
        parse_levels("500M\n").unwrap(),
        [Level {
            max_entry: u64::MAX,
            budget: 500 * 1024 * 1024
        }]
    );
    // Anything unreadable leaves the shipped defaults in place.
    assert_eq!(parse_levels("1M=lots\n"), None);
    assert_eq!(parse_levels("lots\n"), None);
    assert_eq!(parse_levels("# nothing but a comment\n"), None);
}

/// The multi-band budget is written out in a form the user can edit, and
/// what they edit is what the next prune uses.
#[test]
fn a_banded_limit_file_round_trips_through_a_prune() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::at(tmp.path().join("cache"), 4500).with_level(1000, 250);
    let path = cache
        .get_file("k", "a.bin", |dest| {
            Ok(std::fs::write(dest, vec![0u8; 100])?)
        })
        .unwrap();
    cache.prune();
    let limit = cache.root.join(LIMIT_FILE);
    assert_eq!(std::fs::read_to_string(&limit).unwrap(), "1000=250\n4500\n");

    // Squeeze the small band alone: the entry it holds goes.
    std::fs::write(&limit, "1000=0\n4500\n").unwrap();
    cache.prune();
    assert!(!path.exists());
}

/// The same material must key the same way every run — that is the whole
/// reason this isn't `DefaultHasher`.
#[test]
fn key_hasher_is_stable_and_content_sensitive() {
    let key_of = |parts: &[&str]| {
        let mut h = KeyHasher::new();
        for p in parts {
            h.add(p);
        }
        h.finish()
    };
    assert_eq!(key_of(&["abc", "def"]), key_of(&["abc", "def"]));
    assert_ne!(key_of(&["abc", "def"]), key_of(&["abc", "deg"]));
    // Fields are framed, so moving a boundary is a different key even
    // though the concatenated bytes are identical.
    assert_ne!(key_of(&["ab", "c"]), key_of(&["a", "bc"]));
    assert_eq!(key_of(&["abc"]).len(), 16);
}
