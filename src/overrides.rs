//! Per-release fixups, read from `overrides.toml` at startup.
//!
//! A db line says where a release can be downloaded and little else, and for a
//! handful of releases that isn't enough to run them: the listing holds three
//! files where only one is the demo, the archive holds two programs where only
//! one is the one to start, or the release needs a config file it was never
//! packed with. An override is where that knowledge is written down, keyed on
//! the demozoo id of the release (the `id` field of a demozoo db line).
//!
//! The file looks like this:
//!
//! ```toml
//! [zoo.102]
//! file = "rgba_tbc_elevated.zip"      # which download to fetch
//! boot = "elevated_1280x720.exe"      # which file inside it to start
//!
//! [zoo.68604]
//! libretro = { dosbox_pure_cycles = "max" }   # core options, as meta
//!
//! [zoo.18030]
//! file = "inside.zip"
//! patch = { target = "SOUND.CFG", contents = "U0RJR1VT…", info = "GUS 0x240" }
//!
//! [zoo.119665]
//! assign = { Love = "SYS:" }         # AmigaDOS assigns to make before booting
//!
//! [zoo.7236]
//! fast = true                        # accelerated A1200 with FPU, Z3 mem and JIT
//! ```
//!
//! Every key is optional, and an entry may carry several patches by writing
//! `patch` as an array (`[[zoo.18030.patch]]`). What each one does, and when,
//! is described on [`Override`]; the three are applied at the three stages of a
//! load — `file` when it is downloaded
//! ([`FileSource::pick_download`](crate::emu_file::FileSource::pick_download)),
//! `patch` once it is unpacked and `boot`/`libretro`/`fast` as it is handed to
//! a system (both in [`NewSys::load_file`](crate::newsys::NewSys::load_file)).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::{info, warn};

use crate::emu_file::{Override, Patch};
use crate::files::leak;
use crate::system_dir;

/// What the file is called wherever it is looked for.
pub const FILE_NAME: &str = "overrides.toml";

/// Where an overrides file is looked for, in order: the directory demarc was
/// started in, the user's config directory, and finally the system dir the
/// bundled assets are extracted to. The first one that exists wins, so a file
/// in the working directory is the way to try a new override out without
/// touching the installed one.
fn search_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(FILE_NAME)];
    if let Some(config) = dirs::config_dir() {
        paths.push(config.join("demarc").join(FILE_NAME));
    }
    paths.push(system_dir().join(FILE_NAME));
    paths
}

/// The overrides to run with, from the first file [`search_paths`] finds.
///
/// Overrides are a convenience, not a requirement: no file at all is the normal
/// case and gives an empty map, and a file that doesn't parse is reported and
/// then likewise ignored rather than taking the run down with it.
pub fn load_default() -> HashMap<usize, Override> {
    let Some(path) = search_paths().into_iter().find(|p| p.is_file()) else {
        return HashMap::new();
    };
    match load(&path) {
        Ok(overrides) => {
            info!("Read {} overrides from {path:?}", overrides.len());
            overrides
        }
        Err(err) => {
            crate::println(format!("** Error: Can't read {}: {err:#}", path.display()));
            HashMap::new()
        }
    }
}

/// Read and parse one overrides file.
pub fn load(path: &Path) -> Result<HashMap<usize, Override>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Could not read {}", path.display()))?;
    parse(&text)
}

/// The whole file: the `zoo` table of overrides, plus whatever else was written
/// at the top level, which is kept only so it can be warned about — a
/// mistyped `[zoo_57849]` is a table of its own as far as toml is concerned,
/// and silently doing nothing is the least helpful thing to do about it.
#[derive(Deserialize)]
struct OverrideFile {
    #[serde(default)]
    zoo: HashMap<String, RawOverride>,
    #[serde(flatten)]
    rest: toml::Table,
}

/// One `[zoo.<id>]` table, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOverride {
    /// File name of the download to fetch, out of the several a release lists.
    file: Option<String>,
    /// File name inside the release of the program to start.
    boot: Option<String>,
    /// Core options, which is what most meta on an entry is.
    #[serde(default)]
    libretro: toml::Table,
    /// Anything else that belongs on the entry, under its own name.
    #[serde(default)]
    meta: toml::Table,
    /// `fast = true` to run the release on the fast Amiga configuration —
    /// an accelerated A1200 with fast/Z3 memory, an FPU and the JIT. Shorthand
    /// for the half-dozen core options that spells out, see
    /// [`apply_fast`](crate::newsys::amiga::apply_fast).
    #[serde(default)]
    fast: bool,
    /// AmigaDOS assigns the release needs, as `assign = { Love = "SYS:" }`.
    /// They are folded into the single `assign` meta value the Amiga system
    /// reads when it writes the startup-sequence — see
    /// [`handle_exe`](crate::newsys::amiga).
    #[serde(default)]
    assign: toml::Table,
    /// One patch, or an array of them.
    patch: Option<Patches>,
}

/// `patch = { … }` for the common single patch, `[[zoo.<id>.patch]]` (or an
/// array of inline tables) when a release needs more than one.
#[derive(Deserialize)]
#[serde(untagged)]
enum Patches {
    One(RawPatch),
    Many(Vec<RawPatch>),
}

impl Patches {
    fn into_vec(self) -> Vec<RawPatch> {
        match self {
            Patches::One(patch) => vec![patch],
            Patches::Many(patches) => patches,
        }
    }
}

/// One `patch` table, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPatch {
    /// Name of the file to write, found anywhere inside the release.
    target: String,
    /// What to write, base64 encoded — these are binary config files.
    contents: String,
    /// Where in the file it goes. Left out to replace the file entirely,
    /// which is what a config file small enough to write out in full wants.
    offset: Option<usize>,
    /// What the patch is for, in a few words, for the log.
    #[serde(default)]
    info: String,
}

/// Parse the contents of an overrides file — see the module docs for the shape.
///
/// An entry that doesn't make sense (an id that isn't a number, base64 that
/// doesn't decode) is reported and dropped on its own; the rest of the file
/// still applies, since one bad entry says nothing about the others.
pub fn parse(text: &str) -> Result<HashMap<usize, Override>> {
    let file: OverrideFile = toml::from_str(text).context("Not a valid overrides file")?;
    for key in file.rest.keys() {
        warn!("Ignoring unknown section [{key}] — overrides go under [zoo.<id>]");
    }

    let mut overrides = HashMap::with_capacity(file.zoo.len());
    for (id, raw) in file.zoo {
        let Ok(id) = id.parse::<usize>() else {
            warn!("Ignoring [zoo.{id}]: not a demozoo id");
            continue;
        };
        match raw.build() {
            Ok(over) => {
                overrides.insert(id, over);
            }
            Err(err) => warn!("Ignoring [zoo.{id}]: {err:#}"),
        }
    }
    Ok(overrides)
}

impl RawOverride {
    /// Turn the parsed toml into the [`Override`] the loader uses.
    ///
    /// Every string is leaked on the way, because that is what an
    /// [`Override`] holds and what the entries it is applied to hold — see
    /// [`crate::files::leak`]. The file is read once at startup and lives for
    /// the run, so nothing here would ever be freed anyway.
    fn build(self) -> Result<Override> {
        let mut meta = HashMap::new();
        for table in [self.libretro, self.meta] {
            for (key, value) in table {
                let Some(value) = meta_value(&value) else {
                    bail!("meta {key} is a {}, not a value", value.type_str());
                };
                meta.insert(leak(key), leak(value));
            }
        }

        // `Name=Target;Name2=Target2`, which is the shape
        // `newsys::amiga::handle_exe` splits back apart. Written out here
        // rather than kept as a table because meta is strings all the way down.
        let mut assigns = Vec::new();
        for (key, value) in self.assign {
            let Some(value) = meta_value(&value) else {
                bail!("assign {key} is a {}, not a value", value.type_str());
            };
            assigns.push(format!("{key}={value}"));
        }
        if !assigns.is_empty() {
            meta.insert("assign", leak(assigns.join(";")));
        }

        let patches = self
            .patch
            .map(Patches::into_vec)
            .unwrap_or_default()
            .into_iter()
            .map(RawPatch::build)
            .collect::<Result<Vec<Patch>>>()?;

        Ok(Override {
            download: self.file.map(leak),
            boot_file: self.boot.map(leak),
            meta,
            patches,
            fast: self.fast,
        })
    }
}

impl RawPatch {
    fn build(self) -> Result<Patch> {
        let patch = Patch {
            target: leak(self.target),
            offset: self.offset,
            data: leak(self.contents),
            info: leak(self.info),
        };
        // Decoded here and thrown away, so that a mistyped `contents` is
        // reported at startup rather than by the one load that needs it.
        patch.bytes()?;
        Ok(patch)
    }
}

/// A meta value as the string every consumer of meta wants. Numbers and bools
/// are written unquoted often enough (`dosbox_pure_cycles = 150000`) that
/// rejecting them would only be pedantic; a table or an array is a mistake.
fn meta_value(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file as it is actually written: which download to take, which
    /// program inside it to start, core options and a patch.
    #[test]
    fn parses_an_override_per_release() {
        let overrides = parse(
            r#"
            [zoo.102]
            file = "rgba_tbc_elevated.zip"
            boot = "elevated_1280x720.exe"

            [zoo.68604]
            libretro = { dosbox_pure_cycles = "max" }

            [zoo.57849]
            libretro = { dosbox_pure_cycles = 150000 }
            meta = { dos4gw = true }

            [zoo.18030]
            file = "inside.zip"
            patch = { info = "GUS 0x240", target = "SOUND.CFG", contents = "AAEC" }
            "#,
        )
        .unwrap();
        assert_eq!(overrides.len(), 4);

        let elevated = &overrides[&102];
        assert_eq!(elevated.download, Some("rgba_tbc_elevated.zip"));
        assert_eq!(elevated.boot_file, Some("elevated_1280x720.exe"));
        assert!(elevated.patches.is_empty());

        assert_eq!(overrides[&68604].meta["dosbox_pure_cycles"], "max");
        // A number written unquoted is still a meta value, as is a bool.
        assert_eq!(overrides[&57849].meta["dosbox_pure_cycles"], "150000");
        assert_eq!(overrides[&57849].meta["dos4gw"], "true");

        let inside = &overrides[&18030];
        assert_eq!(inside.patches.len(), 1);
        assert_eq!(inside.patches[0].target, "SOUND.CFG");
        assert_eq!(inside.patches[0].info, "GUS 0x240");
        assert_eq!(inside.patches[0].offset, None);
        assert_eq!(inside.patches[0].bytes().unwrap(), [0, 1, 2]);
    }

    /// `assign` is written as a table of AmigaDOS names, and arrives as the one
    /// `assign` meta string `newsys::amiga` splits back apart.
    #[test]
    fn folds_assigns_into_one_meta_value() {
        let overrides = parse(
            r#"
            [zoo.119665]
            assign = { Love = "SYS:" }

            [zoo.2]
            assign = { Data = "DH0:data", Music = "DH0:mod" }
            "#,
        )
        .unwrap();
        assert_eq!(overrides[&119665].meta["assign"], "Love=SYS:");
        assert_eq!(overrides[&2].meta["assign"], "Data=DH0:data;Music=DH0:mod");
        // Nothing written, nothing set — the Amiga side never sees the key.
        assert!(
            !parse("[zoo.3]\nfile = \"a.zip\"\n").unwrap()[&3]
                .meta
                .contains_key("assign")
        );
    }

    /// `fast = true` is one word standing in for a whole Amiga configuration,
    /// and is applied before the entry's own options so those still win.
    #[test]
    fn takes_the_fast_amiga_configuration() {
        let overrides = parse(
            r#"
            [zoo.7236]
            fast = true

            [zoo.108]
            file = "2nd_real.zip"
            "#,
        )
        .unwrap();
        assert!(overrides[&7236].fast);
        assert!(!overrides[&108].fast);
    }

    /// A release needing more than one file written gets an array of patches,
    /// and a patch may write into a file rather than replace it.
    #[test]
    fn parses_several_patches() {
        let overrides = parse(
            r#"
            [[zoo.1.patch]]
            target = "SOUND.CFG"
            contents = "AAEC"

            [[zoo.1.patch]]
            target = "DEMO.EXE"
            offset = 1024
            contents = "AAEC"
            "#,
        )
        .unwrap();
        let patches = &overrides[&1].patches;
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].offset, None);
        assert_eq!(patches[1].offset, Some(1024));
    }

    /// One unusable entry is dropped on its own — the rest of the file is
    /// still worth having.
    #[test]
    fn drops_only_the_bad_entry() {
        let overrides = parse(
            r#"
            [zoo.not-an-id]
            file = "a.zip"

            [zoo.2]
            patch = { target = "A.CFG", contents = "not base64!" }

            [zoo.3]
            file = "c.zip"
            "#,
        )
        .unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[&3].download, Some("c.zip"));
    }

    /// A section outside `zoo` is a typo rather than a feature, so it is
    /// ignored — while a misspelled *key* inside an entry is an error, since
    /// there is nowhere else it could have been meant to go.
    #[test]
    fn rejects_what_it_cannot_apply() {
        assert!(parse("[zoo_57849]\nfile = \"a.zip\"\n").unwrap().is_empty());
        assert!(parse("[zoo.1]\nfil = \"a.zip\"\n").is_err());
        assert!(parse("[zoo.1] file =").is_err());
    }
}
