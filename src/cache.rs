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
mod tests {
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
}
