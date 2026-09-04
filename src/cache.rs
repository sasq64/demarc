//! A keyed, size-bounded store for files demarc downloads or derives and would
//! rather not produce twice.
//!
//! Everything here is content-addressed by a caller-supplied key: the key is
//! hashed, the hash names a directory under the cache root, and whatever the
//! caller produces lives inside it. That shape means eviction has exactly one
//! kind of thing to evict, and that a cache hit is a single `is_file` test.
//!
//! Two entry shapes, because the callers come in two kinds:
//!
//! * [`FileCache::get_file`] for a single produced file — a download, a built
//!   disc image.
//! * [`FileCache::get_dir`] for a set of files that only make sense together —
//!   a cue sheet and the tracks it names.
//!
//! Both publish atomically, by building under a dotted `.part` name and
//! renaming into place. That is not paranoia about crashes so much as about
//! *other demarc processes*: two copies opening the same release at once will
//! race, and a half-written file under a name that says it is finished is a
//! cache hit that returns garbage forever.
//!
//! A cache is bounded two ways, both optional to the caller in the sense that
//! neither ever fails a lookup: a size budget it is pruned back to (see
//! [`FileCache::prune`]), and an age limit past which an entry stops counting
//! as a hit (see [`FileCache::with_max_age`]).
//!
//! The size budget can be split by entry size (see [`FileCache::with_level`]),
//! so that a cache holding both a thousand 40 KB tunes and a handful of 600 MB
//! discs doesn't let either kind evict the other.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

/// Records inside an entry when that entry was *produced*.
///
/// The age limit needs a timestamp a cache hit does not move, and the entry's
/// own mtimes are the opposite of that: [`touch`] pushes them forward on every
/// hit so eviction can tell unused entries from merely old ones. An entry that
/// stayed popular would then never be seen as stale.
const STAMP: &str = ".stamp";

/// Holds the cache's size budget, in the cache root.
///
/// Written the first time a cache is pruned and read on every prune after, so
/// a user who wants a bigger — or smaller — cache has a file to edit rather
/// than a constant to rebuild demarc over.
///
/// One line per size band (see [`parse_levels`]): a bare budget for the band
/// holding everything left over, `<entry size>=<budget>` for the ones below
/// it.
const LIMIT_FILE: &str = ".limit";

/// How long a staging file must have gone untouched before
/// [`FileCache::prune`] reads it as abandoned rather than in flight.
///
/// Pruning happens at startup, when this demarc is holding nothing — but
/// *another* demarc may be halfway through a download at that moment, and
/// deleting the file it is writing turns its publish into an error. A live
/// transfer touches its `.part` continuously; an hour of silence means the run
/// that owned it is gone.
const PARTIAL_GRACE: Duration = Duration::from_secs(3600);

/// One size band of a cache's budget: the entries no larger than `max_entry`
/// — and larger than the band below, if there is one — share `budget` bytes
/// between them, and are evicted only against each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Level {
    max_entry: u64,
    budget: u64,
}

/// A cache rooted at `<user cache dir>/demarc/<name>`.
///
/// Holds nothing but the path and its two bounds, so it is cheap to construct
/// and `Send + Sync` — which matters because downloads run both on the main
/// thread and on Bevy's `IoTaskPool` (see [`crate::jobs`]).
pub struct FileCache {
    root: PathBuf,
    /// Size budgets by band, smallest entries first, used when [`LIMIT_FILE`]
    /// doesn't exist yet. Never empty: the last band catches everything the
    /// ones before it didn't.
    levels: Vec<Level>,
    /// How old an entry may be and still count as a hit. `None` — the default
    /// — means an entry stays valid until it is evicted for space.
    max_age: Option<Duration>,
}

impl FileCache {
    /// A cache named `name`, living under the user's cache directory, pruned
    /// back to `size_limit` bytes.
    ///
    /// With no user cache directory to put it in the cache still works, it just
    /// lands somewhere the OS may clear between runs — a slow demarc beats one
    /// that refuses to fetch anything.
    pub fn new(name: &str, size_limit: u64) -> Self {
        Self {
            root: dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("demarc")
                .join(name),
            levels: vec![Level {
                max_entry: u64::MAX,
                budget: size_limit,
            }],
            max_age: None,
        }
    }

    /// Give entries of up to `max_entry` bytes a budget of their own, taking
    /// them out of the one the cache was constructed with.
    ///
    /// Without this a cache is a single pool, and whichever kind of entry
    /// arrives in bulk evicts the other: a browse through a few hundred tiny
    /// downloads pushes out the big disc images that cost minutes to rebuild,
    /// and one big image pushes out hundreds of small ones. A band per size
    /// class means each is only ever evicted by its own kind.
    ///
    /// Bands are half-open — each covers the entries above the next band down
    /// and up to its own `max_entry` — so they can be added in any order, and
    /// the budget passed to [`FileCache::new`] is the one for everything above
    /// the largest band named here.
    pub fn with_level(mut self, max_entry: u64, budget: u64) -> Self {
        self.levels.push(Level { max_entry, budget });
        self.levels.sort_by_key(|level| level.max_entry);
        self
    }

    /// Stop treating entries older than `max_age` as hits, so they are produced
    /// again — for a cache whose upstream keeps changing under a stable key, a
    /// nightly build being the obvious one.
    ///
    /// Expiry is a preference, not a requirement: if producing the fresh copy
    /// fails, the getters warn and return the old entry rather than failing.
    /// Being a fortnight behind beats not running because the network is down.
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// A cache rooted at an explicit path, for tests that must not touch — or
    /// depend on the state of — the user's real cache directory.
    #[cfg(test)]
    pub(crate) fn at(root: PathBuf, size_limit: u64) -> Self {
        Self {
            root,
            levels: vec![Level {
                max_entry: u64::MAX,
                budget: size_limit,
            }],
            max_age: None,
        }
    }

    /// The cached file `filename` for `key`, producing it if it isn't there.
    ///
    /// On a miss `produce` is called with the path to write and is expected to
    /// leave a complete file there; it is not called at all on a hit, so a
    /// caller reporting progress should not assume it will ever run.
    ///
    /// `filename` is not part of the key — it is the readable, correctly
    /// suffixed name the entry carries, and downstream code dispatches on the
    /// extension, so the extension has to survive.
    pub fn get_file<F>(&self, key: &str, filename: &str, produce: F) -> Result<PathBuf>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let dir = self.root.join(hash_key(key));
        let path = dir.join(filename);
        // A stale entry is not a hit, but it is still a fallback: it stays on
        // disk while the replacement is produced, and is returned unchanged if
        // producing one fails.
        let stale = path.is_file() && {
            if !self.expired(&dir) {
                // Mark the hit as recent so [`FileCache::prune`] evicts
                // genuinely unused entries rather than merely old ones.
                touch(&path);
                return Ok(path);
            }
            debug!("Cache entry {path:?} is past its age limit; refreshing");
            true
        };

        std::fs::create_dir_all(&dir)?;
        let partial = dir.join(format!(".{filename}.part"));
        // A `.part` left by an interrupted run is not resumable — whatever is in
        // it came from a transfer or a build that never finished — so it goes
        // before the producer starts rather than being appended to.
        let _ = std::fs::remove_file(&partial);
        match produce(&partial) {
            Ok(()) => {
                stamp(&dir);
                std::fs::rename(&partial, &path)?;
                Ok(path)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                if stale {
                    warn!(
                        "Failed to refresh {}: {e} — using the cached copy",
                        path.display()
                    );
                    touch(&path);
                    return Ok(path);
                }
                Err(e)
            }
        }
    }

    /// The cached directory for `key`, producing it if it isn't there.
    ///
    /// An entry counts as present once `marker` exists inside it, so the
    /// producer's last write should be the one file that proves the rest
    /// arrived. On a miss `produce` is handed a staging directory to fill,
    /// which is renamed into place as a unit once it returns.
    ///
    /// Returns the entry directory; the caller joins `marker` (or anything else
    /// it put there) itself.
    pub fn get_dir<F>(&self, key: &str, marker: &str, produce: F) -> Result<PathBuf>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let hash = hash_key(key);
        let dir = self.root.join(&hash);
        let mut stale = false;
        if dir.join(marker).is_file() {
            if !self.expired(&dir) {
                touch(&dir.join(marker));
                return Ok(dir);
            }
            debug!("Cache entry {dir:?} is past its age limit; rebuilding");
            stale = true;
        } else if dir.exists() {
            // An entry without its marker never finished. Nothing may reference
            // it, so clear it rather than renaming a second copy alongside.
            debug!("Rebuilding incomplete cache entry {dir:?}");
            std::fs::remove_dir_all(&dir)?;
        }

        std::fs::create_dir_all(&self.root)?;
        let staging = self.root.join(format!(".{hash}.part"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        if let Err(e) = produce(&staging) {
            let _ = std::fs::remove_dir_all(&staging);
            if stale {
                warn!(
                    "Failed to rebuild {}: {e} — using the cached copy",
                    dir.display()
                );
                touch(&dir.join(marker));
                return Ok(dir);
            }
            return Err(e);
        }
        // Stamped inside the staging directory, so the entry and the record of
        // when it was built are published by the same rename.
        stamp(&staging);

        // A stale entry is still sitting on the name the rename needs. Move it
        // aside rather than deleting it, so there is something to put back if
        // publishing the rebuild fails.
        let previous = self.root.join(format!(".{hash}.old"));
        if stale {
            let _ = std::fs::remove_dir_all(&previous);
            if let Err(e) = std::fs::rename(&dir, &previous) {
                warn!(
                    "Failed to replace {}: {e} — using the cached copy",
                    dir.display()
                );
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(dir);
            }
        }

        if let Err(e) = std::fs::rename(&staging, &dir) {
            let _ = std::fs::remove_dir_all(&staging);
            // Another demarc built the same entry while we were working. Its
            // copy is as good as ours by construction — same key, same
            // contents — so drop ours and use what is already there.
            if dir.join(marker).is_file() {
                debug!("Lost the race to build {dir:?}; using the existing entry");
                let _ = std::fs::remove_dir_all(&previous);
                return Ok(dir);
            }
            if stale {
                // Nothing took the name, so the entry we moved aside is still
                // the best copy there is. Put it back.
                let _ = std::fs::rename(&previous, &dir);
                if dir.join(marker).is_file() {
                    warn!(
                        "Failed to publish the rebuilt {}: {e} — using the cached copy",
                        dir.display()
                    );
                    return Ok(dir);
                }
            }
            return Err(e.into());
        }
        let _ = std::fs::remove_dir_all(&previous);
        Ok(dir)
    }

    /// Delete the wreckage of interrupted runs, then least-recently-used
    /// entries until the cache is back under its budget. Intended to run once
    /// at startup, when nothing is holding a path into the cache yet.
    ///
    /// The budget is whatever [`LIMIT_FILE`] holds, falling back to — and
    /// recording — the one this cache was constructed with; see
    /// [`FileCache::limits`]. With more than one band (see
    /// [`FileCache::with_level`]) there is one pass per band, each totalling up
    /// only the entries whose size falls in it and evicting only those, so a
    /// flood of small entries cannot push out the large ones or the reverse.
    ///
    /// Eviction is per entry — the unit a cache hit is keyed on — using the
    /// newest mtime inside it as its last-use time, which the getters refresh
    /// on every hit. Errors are logged and skipped rather than propagated: a
    /// cache that can't be pruned is a disk-space problem, not a reason to
    /// refuse to start.
    pub fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            // No cache directory yet — nothing to prune.
            return;
        };

        let mut freed = 0u64;
        let mut removed = 0usize;
        let mut items: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            // Not an entry: evicting it would silently reset a limit the user
            // set by hand.
            if name == LIMIT_FILE {
                continue;
            }
            let path = entry.path();
            let partial = is_partial(&name);
            if !partial {
                // The staging file [`FileCache::get_file`] writes lives *inside*
                // the entry, so an interrupted download leaves one there next to
                // no payload at all.
                let (size, count) = sweep_partials(&path);
                freed += size;
                removed += count;
            }
            let (size, used) = entry_stats(&path);
            // Wreckage, not an entry: a `.part` is a download or a build that
            // was interrupted, and an empty directory is an entry nothing ever
            // wrote into. Neither can ever be a hit, so neither is worth
            // carrying around until it happens to be the oldest thing here —
            // once [`PARTIAL_GRACE`] says nobody is still writing it.
            if partial || size == 0 {
                if abandoned(used) && remove_entry(&path) {
                    freed += size;
                    removed += 1;
                }
                continue;
            }
            items.push((used, size, path));
        }

        // Oldest first, so the entries nobody has opened in the longest go first.
        items.sort_by_key(|(used, ..)| *used);
        let mut floor = 0u64;
        for level in self.limits() {
            // Each band is measured and evicted on its own; entries outside it
            // are neither counted against its budget nor touched by it.
            let in_band = |size: u64| size > floor && size <= level.max_entry;
            let mut total: u64 = items
                .iter()
                .filter(|(_, size, _)| in_band(*size))
                .map(|(_, size, _)| *size)
                .sum();
            for (_, size, path) in &items {
                if total <= level.budget {
                    break;
                }
                if !in_band(*size) {
                    continue;
                }
                if remove_entry(path) {
                    total -= *size;
                    freed += *size;
                    removed += 1;
                }
            }
            floor = level.max_entry;
        }

        if removed > 0 {
            info!(
                "Pruned {removed} entr{} from {}, freeing {} MB",
                if removed == 1 { "y" } else { "ies" },
                self.root.display(),
                freed / (1024 * 1024)
            );
        }
    }

    /// The size budgets to prune against, smallest band first.
    ///
    /// [`LIMIT_FILE`] wins if it is there and readable, which is the point of
    /// writing it: the constants demarc ships are a starting guess, and someone
    /// with a small disk or a big collection knows better. An unparseable one
    /// is reported and ignored rather than treated as zero, which would empty
    /// the cache over a typo.
    fn limits(&self) -> Vec<Level> {
        let path = self.root.join(LIMIT_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if let Some(levels) = parse_levels(&text) {
                    return levels;
                }
                warn!(
                    "Ignoring unreadable size limit {:?} in {}",
                    text.trim(),
                    path.display()
                );
            }
            Err(_) => {
                if let Err(e) = std::fs::write(&path, format_levels(&self.levels)) {
                    debug!("Failed to write {}: {e}", path.display());
                }
            }
        }
        self.levels.clone()
    }

    /// Whether the entry in `dir` is past this cache's age limit.
    ///
    /// With no age limit nothing expires. With one, an entry carrying no
    /// [`STAMP`] — written before the cache had an age limit, or by a run that
    /// died between publishing and stamping — has an age nobody can establish,
    /// and counts as too old rather than as fresh: the cost of being wrong is
    /// one produce that would otherwise have been skipped, and the fallback
    /// keeps even that from being fatal.
    fn expired(&self, dir: &Path) -> bool {
        let Some(max_age) = self.max_age else {
            return false;
        };
        entry_age(&dir.join(STAMP)).is_none_or(|age| age > max_age)
    }
}

/// Builds a cache key out of content rather than out of a short string.
///
/// Callers that key on what a file *contains* — because the path it arrived at
/// is a fresh temp directory every launch — would otherwise have to concatenate
/// megabytes into a key string. Feed the material through here instead and pass
/// [`KeyHasher::finish`]'s output as the key.
///
/// SHA-256 rather than [`std::collections::hash_map::DefaultHasher`], whose
/// output is explicitly not stable across Rust releases: keyed on that, a cache
/// silently invalidates in its entirety on every toolchain upgrade.
#[derive(Default)]
pub struct KeyHasher(Sha256);

impl KeyHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one field to the key.
    ///
    /// Each field is length-prefixed, so a caller feeding a sequence of
    /// variable-length pieces cannot accidentally hash two different sequences
    /// the same by shifting a boundary — `["ab", "c"]` and `["a", "bc"]` are
    /// distinct keys.
    pub fn add(&mut self, bytes: impl AsRef<[u8]>) {
        let bytes = bytes.as_ref();
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    /// The finished key, as hex.
    pub fn finish(self) -> String {
        hex(self.0.finalize().iter().take(8))
    }
}

/// Hash a key into the hex string naming its entry directory.
///
/// A key's own text is not a safe directory name: it may be a URL, it may be
/// arbitrarily long, and `.../v1/game.zip` and `.../v2/game.zip` must not
/// collapse together. 16 hex chars (64 bits) is far past any plausible
/// collision here, and SHA-256 keeps the mapping stable across toolchain
/// upgrades so an existing cache stays valid.
fn hash_key(key: &str) -> String {
    hex(Sha256::digest(key.as_bytes()).iter().take(8))
}

fn hex<'a>(bytes: impl Iterator<Item = &'a u8>) -> String {
    bytes.map(|b| format!("{b:02x}")).collect()
}

/// Record the entry in `dir` as produced now.
///
/// Failures are ignored: an unstamped entry reads as expired and is produced
/// again next time it comes up, which is a wasted download rather than a
/// reason to fail the lookup that just succeeded.
fn stamp(dir: &Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let _ = std::fs::write(dir.join(STAMP), format!("{}\n", now.as_secs()));
}

/// How long ago the entry `stamp` belongs to was produced, or `None` if there
/// is no readable stamp to say.
///
/// A stamp dated in the future means the clock moved, not that the entry is
/// from tomorrow; it counts as brand new, which is the reading that doesn't
/// throw away a perfectly good entry.
fn entry_age(stamp: &Path) -> Option<Duration> {
    let text = std::fs::read_to_string(stamp).ok()?;
    let written = UNIX_EPOCH + Duration::from_secs(text.trim().parse().ok()?);
    Some(
        SystemTime::now()
            .duration_since(written)
            .unwrap_or_default(),
    )
}

/// Whether `name` is one of the dotted staging names the getters build under —
/// `.<file>.part` for [`FileCache::get_file`], `.<hash>.part` for
/// [`FileCache::get_dir`] — rather than a published entry.
fn is_partial(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().ends_with(".part")
}

/// Whether something last written at `used` has been left alone long enough
/// that the run producing it must be gone; see [`PARTIAL_GRACE`].
///
/// A timestamp in the future — a clock that moved, a file copied from a
/// machine ahead of this one — reads as in flight, which is the answer that
/// doesn't delete somebody's download.
fn abandoned(used: SystemTime) -> bool {
    SystemTime::now()
        .duration_since(used)
        .is_ok_and(|idle| idle > PARTIAL_GRACE)
}

/// Delete `path`, whichever shape the entry is, reporting whether it went.
fn remove_entry(path: &Path) -> bool {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            warn!("Failed to prune {}: {e}", path.display());
            false
        }
    }
}

/// Delete the staging files left inside the entry `dir` by a run that died
/// mid-produce, returning how many bytes and how many files went.
///
/// [`FileCache::get_file`] clears its own `.part` before producing and again if
/// producing fails, so what this finds is what a process that was killed —
/// or that ran out of disk — left behind, on a key nothing has asked for since.
fn sweep_partials(dir: &Path) -> (u64, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Not a directory, or not readable: nothing to sweep either way.
        return (0, 0);
    };
    let mut freed = 0;
    let mut removed = 0;
    for entry in entries.flatten() {
        if !is_partial(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let (size, used) = entry_stats(&path);
        if abandoned(used) && remove_entry(&path) {
            freed += size;
            removed += 1;
        }
    }
    (freed, removed)
}

/// The size bands as written in [`LIMIT_FILE`], smallest first: one per line,
/// either `<entry size>=<budget>` for a band covering entries up to that size,
/// or a bare `<budget>` for the band holding everything the others didn't.
///
/// `None` if any line is unreadable, so a typo leaves the shipped defaults in
/// place rather than silently applying half a budget. Blank lines and `#`
/// comments are skipped, so a user can write down what they meant.
fn parse_levels(text: &str) -> Option<Vec<Level>> {
    let mut levels = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        levels.push(match line.split_once('=') {
            Some((max_entry, budget)) => Level {
                max_entry: parse_size(max_entry)?,
                budget: parse_size(budget)?,
            },
            None => Level {
                max_entry: u64::MAX,
                budget: parse_size(line)?,
            },
        });
    }
    if levels.is_empty() {
        return None;
    }
    levels.sort_by_key(|level| level.max_entry);
    Some(levels)
}

/// The size bands as [`parse_levels`] reads them back, for the file demarc
/// writes the first time it prunes.
fn format_levels(levels: &[Level]) -> String {
    levels
        .iter()
        .map(|level| match level.max_entry {
            u64::MAX => format!("{}\n", format_size(level.budget)),
            max_entry => format!("{}={}\n", format_size(max_entry), format_size(level.budget)),
        })
        .collect()
}

/// A size as written in [`LIMIT_FILE`]: a byte count, optionally suffixed `K`,
/// `M`, `G` or `T` — each 1024 of the one before, with a trailing `B` allowed
/// so `500MB` reads the same as `500M`.
fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_uppercase();
    let digits = text.strip_suffix('B').unwrap_or(&text);
    let (digits, unit) = match digits.chars().next_back()? {
        'K' => (&digits[..digits.len() - 1], 1024),
        'M' => (&digits[..digits.len() - 1], 1024 * 1024),
        'G' => (&digits[..digits.len() - 1], 1024 * 1024 * 1024),
        'T' => (&digits[..digits.len() - 1], 1024u64.pow(4)),
        _ => (digits, 1),
    };
    digits.trim_end().parse::<u64>().ok()?.checked_mul(unit)
}

/// The most readable spelling of `size` that [`parse_size`] reads back
/// unchanged — so the file demarc writes is one a user can edit in the units
/// they already think in.
fn format_size(size: u64) -> String {
    for (suffix, unit) in [("G", 1024u64.pow(3)), ("M", 1024 * 1024), ("K", 1024)] {
        if size >= unit && size.is_multiple_of(unit) {
            return format!("{}{suffix}", size / unit);
        }
    }
    size.to_string()
}

/// Set `path`'s access and modification times to now, recording it as recently
/// used for [`FileCache::prune`].
///
/// Failures are ignored: a cache entry we can't touch is not worth failing a
/// lookup over, it just risks being evicted earlier than it should be.
fn touch(path: &Path) {
    let now = SystemTime::now();
    let times = std::fs::FileTimes::new()
        .set_accessed(now)
        .set_modified(now);
    // Opening for write, not read: on Windows `set_times` needs write access,
    // and on Unix futimens wants a handle we're allowed to modify.
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_times(times);
    }
}

/// Total size of `path` and the time it was last used, taken as the newest
/// mtime found within it. A path we can't stat counts as zero-sized and
/// last used at the epoch, so a broken entry is evicted first.
fn entry_stats(path: &Path) -> (u64, SystemTime) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    if !meta.is_dir() {
        return (
            meta.len(),
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        );
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    let mut size = 0;
    let mut used = SystemTime::UNIX_EPOCH;
    for entry in entries.flatten() {
        let (child_size, child_used) = entry_stats(&entry.path());
        size += child_size;
        used = used.max(child_used);
    }
    (size, used)
}

#[cfg(test)]
#[path = "tests/cache_tests.rs"]
mod tests;
