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

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

/// A cache rooted at `<user cache dir>/demarc/<name>`.
///
/// Holds nothing but the path, so it is cheap to construct and `Send + Sync` —
/// which matters because downloads run both on the main thread and on Bevy's
/// `IoTaskPool` (see [`crate::jobs`]).
pub struct FileCache {
    root: PathBuf,
}

impl FileCache {
    /// A cache named `name`, living under the user's cache directory.
    ///
    /// With no user cache directory to put it in the cache still works, it just
    /// lands somewhere the OS may clear between runs — a slow demarc beats one
    /// that refuses to fetch anything.
    pub fn new(name: &str) -> Self {
        Self {
            root: dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("demarc")
                .join(name),
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
        if path.is_file() {
            // Mark the hit as recent so [`FileCache::prune`] evicts genuinely
            // unused entries rather than merely old ones.
            touch(&path);
            return Ok(path);
        }

        std::fs::create_dir_all(&dir)?;
        let partial = dir.join(format!(".{filename}.part"));
        // A `.part` left by an interrupted run is not resumable — whatever is in
        // it came from a transfer or a build that never finished — so it goes
        // before the producer starts rather than being appended to.
        let _ = std::fs::remove_file(&partial);
        match produce(&partial) {
            Ok(()) => {
                std::fs::rename(&partial, &path)?;
                Ok(path)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
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
        if dir.join(marker).is_file() {
            touch(&dir.join(marker));
            return Ok(dir);
        }

        // An entry without its marker never finished. Nothing may reference it,
        // so clear it rather than renaming a second copy alongside.
        if dir.exists() {
            debug!("Rebuilding incomplete cache entry {dir:?}");
            std::fs::remove_dir_all(&dir)?;
        }

        std::fs::create_dir_all(&self.root)?;
        let staging = self.root.join(format!(".{hash}.part"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        if let Err(e) = produce(&staging) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }

        if let Err(e) = std::fs::rename(&staging, &dir) {
            // Another demarc built the same entry while we were working. Its
            // copy is as good as ours by construction — same key, same
            // contents — so drop ours and use what is already there.
            if dir.join(marker).is_file() {
                debug!("Lost the race to build {dir:?}; using the existing entry");
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(dir);
            }
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e.into());
        }
        Ok(dir)
    }

    /// Delete least-recently-used entries until the cache's total size is back
    /// under `max_size`. Intended to run once at startup, when nothing is
    /// holding a path into the cache yet.
    ///
    /// Eviction is per entry — the unit a cache hit is keyed on — using the
    /// newest mtime inside it as its last-use time, which the getters refresh
    /// on every hit. Errors are logged and skipped rather than propagated: a
    /// cache that can't be pruned is a disk-space problem, not a reason to
    /// refuse to start.
    pub fn prune(&self, max_size: u64) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            // No cache directory yet — nothing to prune.
            return;
        };

        let mut total = 0u64;
        let mut items: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let (size, used) = entry_stats(&path);
            total += size;
            items.push((used, size, path));
        }
        if total <= max_size {
            return;
        }

        // Oldest first, so the entries nobody has opened in the longest go first.
        items.sort_by_key(|(used, ..)| *used);
        let mut freed = 0u64;
        let mut removed = 0usize;
        for (_, size, path) in items {
            if total <= max_size {
                break;
            }
            let result = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match result {
                Ok(()) => {
                    total -= size;
                    freed += size;
                    removed += 1;
                }
                Err(e) => warn!("Failed to prune {}: {e}", path.display()),
            }
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
    use std::time::Duration;

    /// A cache rooted in a temp dir rather than under the user's real one.
    fn temp_cache() -> (tempfile::TempDir, FileCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = FileCache {
            root: dir.path().join("cache"),
        };
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

    #[test]
    fn prunes_least_recently_used_until_under_limit() {
        let (_tmp, cache) = temp_cache();
        // Three 100-byte entries, aged 3h / 2h / 1h ago.
        let mut entries = Vec::new();
        for (key, hours) in [("old", 3), ("mid", 2), ("new", 1)] {
            let path = cache
                .get_file(key, "a.zip", |dest| {
                    Ok(std::fs::write(dest, vec![0u8; 100])?)
                })
                .unwrap();
            age(&path, hours);
            entries.push(path);
        }
        let [old, mid, new] = <[PathBuf; 3]>::try_from(entries).unwrap();

        // Under the limit: nothing is touched.
        cache.prune(300);
        assert!(old.exists());

        // Over it: evict oldest first, and stop as soon as we're back under.
        cache.prune(150);
        assert!(!old.exists());
        assert!(!mid.exists());
        assert!(new.exists());
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
        let (size, old) = entry_stats(&entry);
        assert_eq!(size, 300);

        touch(&entry.join("disc.cue"));
        let (_, used) = entry_stats(&entry);
        assert!(used > old, "the newest file inside sets the entry's age");
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
