//! The configuration assembled from directories of TOML fragments.
//!
//! Instead of one file, the configuration can come from two directories:
//! one with the fragments that packages ship (`--conf-dir`) and one with the
//! user's own (`--user-conf-dir`).
//!
//! - A fragment is a regular file, or a link to one, whose name ends in
//!   `.toml` and does not start with `.`. A link to `/dev/null` counts too,
//!   and reads as empty. Everything else in the directories is ignored, and
//!   a directory that does not exist counts as empty.
//! - The fragments of both directories are combined in the byte order of
//!   their names. A user fragment hides the package fragment of the same
//!   name, which is never read: an empty user fragment (or a link to
//!   `/dev/null`) masks it.
//! - Settings other than `block` merge in fragment order: two tables merge
//!   key by key, recursively, and any other later value replaces the
//!   earlier one.
//! - A `[[block]]` entry may have an `id`, a non-empty string that is never
//!   passed to the block. An entry without `id`, or with an `id` not seen
//!   before, adds a block where it stands (a new `id` needs `block`). An
//!   entry with a known `id` changes that block, which moves to the entry's
//!   place: it replaces the block when it names a different `block`, and
//!   merges into it otherwise. An entry with only `id` and `remove = true`
//!   removes the block.
//! - A fragment that cannot be read, is not valid TOML, breaks the rules
//!   above or makes the configuration fail to load is skipped as a whole,
//!   as if it were not there.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::stream::{self, Stream, StreamExt as _};
use inotify::{EventMask, EventOwned, EventStream, Inotify, WatchDescriptor, WatchMask, Watches};
use itertools::Itertools as _;
use serde::Deserialize as _;
use toml::{Table, Value};

use super::Config;
use crate::errors::*;
use crate::signals::hangups_stream;
use crate::{BoxedStream, util};

/// How long the fragment directories must stay unchanged before the bar
/// reads them again, so that a burst of changes causes a single reload.
const SETTLE_TIME: Duration = Duration::from_millis(200);

/// The directories that hold the configuration fragments.
#[derive(Debug, Clone)]
pub struct FragmentDirs {
    package: Option<PathBuf>,
    user: Option<PathBuf>,
}

/// The configuration built from the fragment directories.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// The fragments that were left out, in merge order.
    pub skipped: Vec<SkippedFragment>,
    /// What the configuration was built from.
    pub snapshot: Snapshot,
}

/// A fragment left out of the configuration, and why.
#[derive(Debug)]
pub struct SkippedFragment {
    pub path: PathBuf,
    pub reason: String,
}

/// The fragments in merge order, with their contents: two equal snapshots
/// build the same configuration.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot(Vec<(PathBuf, Result<Vec<u8>, io::ErrorKind>)>);

impl FragmentDirs {
    pub fn new(package: Option<PathBuf>, user: Option<PathBuf>) -> Self {
        Self { package, user }
    }

    /// The configured directories, the package one first.
    fn dirs(&self) -> impl Iterator<Item = &Path> {
        [&self.package, &self.user]
            .into_iter()
            .flatten()
            .map(PathBuf::as_path)
    }

    /// The fragments in merge order.
    fn fragments(&self) -> Vec<PathBuf> {
        let mut by_name = BTreeMap::new();
        // User fragments come last, replacing package fragments of the
        // same name.
        for dir in self.dirs() {
            let entries = match fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(err) => {
                    if err.kind() != io::ErrorKind::NotFound {
                        log::warn!("cannot read fragment directory {}: {err}", dir.display());
                    }
                    continue;
                }
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let path = entry.path();
                if is_fragment_name(&name) && is_fragment_file(&path) {
                    by_name.insert(name, path);
                }
            }
        }
        by_name.into_values().collect()
    }

    /// Reads every fragment.
    fn snapshot(&self) -> Snapshot {
        Snapshot(
            self.fragments()
                .into_iter()
                .map(|path| {
                    let contents = fs::read(&path).map_err(|err| err.kind());
                    (path, contents)
                })
                .collect(),
        )
    }

    /// Builds the configuration from the fragments, leaving out the ones
    /// that cannot be used.
    pub fn load(&self) -> Result<Loaded> {
        let snapshot = self.snapshot();
        let mut merged = Merged::default();
        let mut config = None;
        let mut skipped = Vec::new();
        for (path, contents) in &snapshot.0 {
            match merged.with_fragment(contents) {
                Ok((next, next_config)) => {
                    merged = next;
                    config = Some(next_config);
                }
                Err(reason) => skipped.push(SkippedFragment {
                    path: path.clone(),
                    reason,
                }),
            }
        }
        let config = match config {
            Some(config) => config,
            None => merged.to_config().map_err(|err| {
                Error::new(format!("The default configuration does not load: {err}"))
            })?,
        };
        Ok(Loaded {
            config,
            skipped,
            snapshot,
        })
    }

    /// Starts watching for reasons to read the fragments again.
    ///
    /// Start watching before calling [`Self::load`]: then no change made
    /// while loading goes unnoticed.
    pub fn watch(&self) -> Watcher {
        let files = DirWatches::new(self)
            .inspect_err(|err| {
                eprintln!("Not watching the configuration fragment directories: {err}");
            })
            .ok();
        Watcher {
            dirs: self.clone(),
            hangups: hangups_stream(),
            files,
        }
    }
}

impl SkippedFragment {
    /// The file name of the fragment.
    pub fn file_name(&self) -> Cow<'_, str> {
        self.path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy()
    }
}

impl fmt::Display for SkippedFragment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason)
    }
}

/// Whether `name` is the name of a fragment.
fn is_fragment_name(name: &OsStr) -> bool {
    let name = name.as_bytes();
    name.ends_with(b".toml") && !name.starts_with(b".")
}

/// Whether `path` is a regular file, a link to one, or a link to
/// `/dev/null`.
fn is_fragment_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => fs::canonicalize(path).is_ok_and(|target| target == Path::new("/dev/null")),
        Err(_) => false,
    }
}

/// Joins the lines of `message` into one.
fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .join("; ")
}

/// The configuration merged from the fragments so far.
#[derive(Debug, Clone, Default)]
struct Merged {
    /// Every setting except `block`.
    settings: Table,
    blocks: Vec<MergedBlock>,
}

#[derive(Debug, Clone)]
struct MergedBlock {
    id: Option<String>,
    settings: Table,
}

impl Merged {
    /// `self` with one more fragment merged in, and the configuration that
    /// results, or why the fragment cannot be used.
    fn with_fragment(
        &self,
        contents: &Result<Vec<u8>, io::ErrorKind>,
    ) -> Result<(Self, Config), String> {
        let contents = contents
            .as_ref()
            .map_err(|kind| format!("cannot be read: {kind}"))?;
        let text =
            std::str::from_utf8(contents).map_err(|err| format!("is not valid UTF-8: {err}"))?;
        let fragment: Table = toml::from_str(text).map_err(|err| {
            let message = one_line(err.message());
            match err.span() {
                Some(span) => format!(
                    "is not valid TOML: line {}: {message}",
                    util::line_number(text, span.start)
                ),
                None => format!("is not valid TOML: {message}"),
            }
        })?;
        let mut merged = self.clone();
        merged.apply(fragment)?;
        let config = merged
            .to_config()
            .map_err(|err| format!("makes the configuration invalid: {err}"))?;
        Ok((merged, config))
    }

    /// Merges `fragment` in. On error, `self` may be left partly changed.
    fn apply(&mut self, mut fragment: Table) -> Result<(), String> {
        let entries = match fragment.remove("block") {
            None => Vec::new(),
            Some(Value::Array(entries)) => entries,
            Some(_) => return Err("`block` is not an array of tables".into()),
        };
        merge_tables(&mut self.settings, fragment);
        for (index, entry) in entries.into_iter().enumerate() {
            let Value::Table(entry) = entry else {
                return Err("`block` is not an array of tables".into());
            };
            self.apply_block(entry)
                .map_err(|err| format!("block entry {}: {err}", index + 1))?;
        }
        Ok(())
    }

    fn apply_block(&mut self, mut entry: Table) -> Result<(), String> {
        let id = match entry.remove("id") {
            None => None,
            Some(Value::String(id)) if !id.is_empty() => Some(id),
            Some(_) => return Err("`id` is not a non-empty string".into()),
        };
        let position = id
            .as_ref()
            .and_then(|id| self.blocks.iter().position(|b| b.id.as_ref() == Some(id)));

        if let Some(remove) = entry.remove("remove") {
            if remove != Value::Boolean(true) {
                return Err("`remove` is not `true`".into());
            }
            if id.is_none() {
                return Err("`remove` needs an `id`".into());
            }
            if !entry.is_empty() {
                return Err("`remove` goes with no keys other than `id`".into());
            }
            if let Some(position) = position {
                self.blocks.remove(position);
            }
            return Ok(());
        }

        let settings = match (&id, position) {
            (None, _) => entry,
            (Some(_), Some(position)) => {
                let mut block = self.blocks.remove(position).settings;
                match entry.get("block") {
                    Some(kind) if Some(kind) != block.get("block") => entry,
                    _ => {
                        merge_tables(&mut block, entry);
                        block
                    }
                }
            }
            (Some(_), None) if entry.contains_key("block") => entry,
            (Some(id), None) => return Err(format!("the new block `{id}` has no `block` key")),
        };
        self.blocks.push(MergedBlock { id, settings });
        Ok(())
    }

    /// The merged configuration as one TOML document, without the ids.
    fn to_document(&self) -> Table {
        let mut document = self.settings.clone();
        document.insert(
            "block".into(),
            Value::Array(
                self.blocks
                    .iter()
                    .map(|block| Value::Table(block.settings.clone()))
                    .collect(),
            ),
        );
        document
    }

    fn to_config(&self) -> Result<Config, String> {
        Config::deserialize(Value::Table(self.to_document())).map_err(|err| one_line(err.message()))
    }
}

/// Merges `incoming` into `base`: tables merge key by key, recursively, and
/// any other value replaces the one in `base`.
fn merge_tables(base: &mut Table, incoming: Table) {
    for (key, value) in incoming {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(base)), Value::Table(incoming)) => merge_tables(base, incoming),
            (Some(existing), value) => *existing = value,
            (None, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Watches for reasons to read the fragments again: SIGHUP, and changes to
/// the fragments.
pub struct Watcher {
    dirs: FragmentDirs,
    hangups: BoxedStream<()>,
    files: Option<DirWatches>,
}

impl Watcher {
    /// A stream that yields whenever the configuration built from
    /// `snapshot` should be built again: when the process receives SIGHUP,
    /// and when the fragments no longer match `snapshot`.
    pub fn reloads(self, snapshot: Snapshot) -> impl Stream<Item = ()> {
        stream::unfold((self, snapshot), |(mut watcher, mut snapshot)| async move {
            watcher.next_reload(&mut snapshot).await;
            Some(((), (watcher, snapshot)))
        })
    }

    async fn next_reload(&mut self, snapshot: &mut Snapshot) {
        match &mut self.files {
            Some(files) => tokio::select! {
                _ = self.hangups.next() => (),
                () = files.changed(&self.dirs, snapshot) => (),
            },
            None => {
                self.hangups.next().await;
            }
        }
    }
}

/// inotify watches on the fragment directories. A directory that does not
/// exist is watched through its nearest existing ancestor, until it
/// appears.
struct DirWatches {
    events: EventStream<[u8; 4096]>,
    watches: Watches,
    /// Every watch in use.
    active: HashSet<WatchDescriptor>,
    /// The watches on fragment directories that are not also watched as the
    /// ancestor of a missing one: only fragment names matter in them.
    fragments_only: HashSet<WatchDescriptor>,
}

impl DirWatches {
    const FRAGMENT_DIR: WatchMask = WatchMask::CREATE
        .union(WatchMask::DELETE)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::MODIFY)
        .union(WatchMask::CLOSE_WRITE)
        .union(WatchMask::ATTRIB)
        .union(WatchMask::DELETE_SELF)
        .union(WatchMask::MOVE_SELF)
        .union(WatchMask::ONLYDIR);
    const ANCESTOR: WatchMask = WatchMask::CREATE
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::DELETE_SELF)
        .union(WatchMask::MOVE_SELF)
        .union(WatchMask::ONLYDIR);

    fn new(dirs: &FragmentDirs) -> io::Result<Self> {
        let inotify = Inotify::init()?;
        let watches = inotify.watches();
        let events = inotify.into_event_stream([0; 4096])?;
        let mut this = Self {
            events,
            watches,
            active: HashSet::new(),
            fragments_only: HashSet::new(),
        };
        this.rearm(dirs);
        Ok(this)
    }

    /// Points the watches at the directories as they are now.
    fn rearm(&mut self, dirs: &FragmentDirs) {
        // path => (is a fragment directory, is the ancestor of a missing one)
        let mut targets = HashMap::<PathBuf, (bool, bool)>::new();
        for dir in dirs.dirs() {
            if let Some(target) = watch_target(dir) {
                let roles = targets.entry(target.clone()).or_default();
                if target == dir {
                    roles.0 = true;
                } else {
                    roles.1 = true;
                }
            }
        }

        let mut active = HashSet::new();
        let mut fragments_only = HashSet::new();
        for (path, (fragment_dir, ancestor)) in targets {
            let mask = if fragment_dir {
                Self::FRAGMENT_DIR
            } else {
                Self::ANCESTOR
            };
            match self.watches.add(&path, mask) {
                Ok(wd) => {
                    if fragment_dir && !ancestor {
                        fragments_only.insert(wd.clone());
                    }
                    active.insert(wd);
                }
                Err(err) => log::warn!("cannot watch {}: {err}", path.display()),
            }
        }
        for stale in self.active.difference(&active) {
            // The kernel drops the watch of a deleted directory by itself.
            let _ = self.watches.remove(stale.clone());
        }
        self.active = active;
        self.fragments_only = fragments_only;
    }

    /// Waits until the fragments no longer match `snapshot`, and updates it.
    async fn changed(&mut self, dirs: &FragmentDirs, snapshot: &mut Snapshot) {
        loop {
            self.next_relevant_event().await;
            while tokio::time::timeout(SETTLE_TIME, self.next_relevant_event())
                .await
                .is_ok()
            {}
            self.rearm(dirs);
            let current = dirs.snapshot();
            if current != *snapshot {
                *snapshot = current;
                return;
            }
        }
    }

    /// Waits for an event that may concern a fragment.
    async fn next_relevant_event(&mut self) {
        while let Some(event) = self.events.next().await {
            match event {
                Ok(event) if !self.is_relevant(&event) => (),
                Ok(_) => return,
                Err(err) => {
                    log::warn!("cannot read inotify events: {err}");
                    return;
                }
            }
        }
        std::future::pending::<()>().await;
    }

    fn is_relevant(&self, event: &EventOwned) -> bool {
        if event.mask.contains(EventMask::Q_OVERFLOW) || !self.fragments_only.contains(&event.wd) {
            return true;
        }
        // Events without a name concern the directory itself.
        event.name.as_deref().is_none_or(is_fragment_name)
    }
}

/// The directory to watch for `dir`: `dir` itself when it exists, or else
/// its nearest existing ancestor.
fn watch_target(dir: &Path) -> Option<PathBuf> {
    let mut candidate = dir;
    loop {
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        candidate = match candidate.parent()? {
            parent if parent.as_os_str().is_empty() => Path::new("."),
            parent => parent,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A directory that is removed when dropped.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "i3status-rs-fragments-{}-{}",
                std::process::id(),
                COUNT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("package")).unwrap();
            fs::create_dir_all(path.join("user")).unwrap();
            Self(path)
        }

        fn package(&self) -> PathBuf {
            self.0.join("package")
        }

        fn user(&self) -> PathBuf {
            self.0.join("user")
        }

        fn dirs(&self) -> FragmentDirs {
            FragmentDirs::new(Some(self.package()), Some(self.user()))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn custom(text: &str) -> String {
        format!("[[block]]\nblock = \"custom\"\ncommand = \"echo {text}\"\n")
    }

    fn with_id(id: &str, text: &str) -> String {
        format!("[[block]]\nid = \"{id}\"\nblock = \"custom\"\ncommand = \"echo {text}\"\n")
    }

    fn merge(fragments: &[&str]) -> Result<Table, String> {
        let mut merged = Merged::default();
        for fragment in fragments {
            merged.apply(toml::from_str(fragment).unwrap())?;
        }
        Ok(merged.to_document())
    }

    fn blocks(document: &Table) -> Vec<&Table> {
        document["block"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block.as_table().unwrap())
            .collect()
    }

    fn commands(document: &Table) -> Vec<&str> {
        blocks(document)
            .iter()
            .map(|block| block["command"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn settings_merge_recursively() {
        let document = merge(&[
            "[theme]\ntheme = \"solarized-dark\"\n[theme.overrides]\nidle_bg = \"#111111\"\nidle_fg = \"#222222\"\n",
            "[theme]\ntheme = \"plain\"\n[theme.overrides]\nidle_bg = \"#333333\"\n",
        ])
        .unwrap();
        let expected: Table = toml::from_str(
            "block = []\n[theme]\ntheme = \"plain\"\n[theme.overrides]\nidle_bg = \"#333333\"\nidle_fg = \"#222222\"\n",
        )
        .unwrap();
        assert_eq!(document, expected);
    }

    #[test]
    fn later_value_replaces_table_or_scalar() {
        let document =
            merge(&["[error_format]\nfull = \"A\"\n", "error_format = \"B\"\n"]).unwrap();
        assert_eq!(document["error_format"].as_str(), Some("B"));
        let document =
            merge(&["error_format = \"A\"\n", "[error_format]\nfull = \"B\"\n"]).unwrap();
        assert_eq!(document["error_format"]["full"].as_str(), Some("B"));
    }

    #[test]
    fn blocks_without_id_are_added() {
        let document = merge(&[&custom("A"), &custom("A")]).unwrap();
        assert_eq!(commands(&document), ["echo A", "echo A"]);
    }

    #[test]
    fn known_id_merges_and_moves() {
        let document = merge(&[
            &(with_id("a", "A")
                + "[block.theme_overrides]\nidle_bg = \"#111111\"\n[[block.click]]\nbutton = \"left\"\ncmd = \"x\"\n"
                + &with_id("b", "B")),
            "[[block]]\nid = \"a\"\n[block.theme_overrides]\nidle_fg = \"#222222\"\n[[block.click]]\nbutton = \"right\"\ncmd = \"y\"\n",
        ])
        .unwrap();
        assert_eq!(commands(&document), ["echo B", "echo A"]);
        let a = blocks(&document)[1];
        assert!(!a.contains_key("id"));
        assert_eq!(a["theme_overrides"]["idle_bg"].as_str(), Some("#111111"));
        assert_eq!(a["theme_overrides"]["idle_fg"].as_str(), Some("#222222"));
        let clicks = a["click"].as_array().unwrap();
        assert_eq!(clicks.len(), 1);
        assert_eq!(clicks[0]["button"].as_str(), Some("right"));
    }

    #[test]
    fn id_only_entry_moves_the_block() {
        let document = merge(&[
            &(with_id("a", "A") + &custom("ANON1") + &with_id("b", "B")),
            &("[[block]]\nid = \"a\"\n".to_owned() + &custom("ANON2") + "[[block]]\nid = \"b\"\n"),
        ])
        .unwrap();
        assert_eq!(
            commands(&document),
            ["echo ANON1", "echo A", "echo ANON2", "echo B"]
        );
    }

    #[test]
    fn other_block_kind_replaces_the_block() {
        let document = merge(&[
            &(with_id("a", "A") + "interval = 5\n"),
            "[[block]]\nid = \"a\"\nblock = \"time\"\n",
        ])
        .unwrap();
        let expected: Table = toml::from_str("block = \"time\"").unwrap();
        assert_eq!(blocks(&document), [&expected]);
    }

    #[test]
    fn remove_takes_the_block_away() {
        let document = merge(&[
            &(with_id("a", "A") + &with_id("b", "B")),
            "[[block]]\nid = \"b\"\nremove = true\n[[block]]\nid = \"zzz\"\nremove = true\n",
        ])
        .unwrap();
        assert_eq!(commands(&document), ["echo A"]);
    }

    #[test]
    fn removed_id_starts_a_new_block() {
        let removed = "[[block]]\nid = \"a\"\nremove = true\n";
        let document = merge(&[&with_id("a", "A"), removed, &with_id("a", "NEW")]).unwrap();
        assert_eq!(commands(&document), ["echo NEW"]);
        assert!(merge(&[&with_id("a", "A"), removed, "[[block]]\nid = \"a\"\n"]).is_err());
    }

    #[test]
    fn invalid_entries_are_rejected() {
        for entry in [
            "id = \"\"\nblock = \"custom\"",
            "id = 42\nblock = \"custom\"",
            "id = [\"a\"]\nblock = \"custom\"",
            "id = \"new\"\ncommand = \"echo\"",
            "id = \"a\"\nremove = false",
            "id = \"a\"\nremove = \"true\"",
            "remove = true",
            "id = \"a\"\nremove = true\nblock = \"custom\"",
        ] {
            assert!(
                merge(&[&format!("[[block]]\n{entry}\n")]).is_err(),
                "accepted {entry:?}"
            );
        }
        assert!(merge(&["block = 1\n"]).is_err());
        assert!(merge(&["block = [1]\n"]).is_err());
    }

    #[test]
    fn fragment_names() {
        for name in ["a.toml", "10_a.toml", "é.toml"] {
            assert!(is_fragment_name(OsStr::new(name)), "{name}");
        }
        for name in [
            ".a.toml",
            ".toml",
            "a.toml~",
            ".a.toml.swp",
            "a.toml.dpkg-dist",
            "a.txt",
        ] {
            assert!(!is_fragment_name(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn fragments_are_ordered_and_shadowed() {
        let dir = TestDir::new();
        write(&dir.package(), "2.toml", "");
        write(&dir.package(), "a.toml", "");
        write(&dir.package(), "shadowed.toml", "");
        write(&dir.user(), "10.toml", "");
        write(&dir.user(), "Z.toml", "");
        write(&dir.user(), "shadowed.toml", "");
        write(&dir.user(), "ignored.toml~", "");
        write(&dir.user(), ".ignored.toml", "");
        fs::create_dir(dir.user().join("ignored.toml")).unwrap();
        std::os::unix::fs::symlink(dir.package(), dir.user().join("dir-link.toml")).unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.user().join("null.toml")).unwrap();

        let fragments = dir.dirs().fragments();
        assert_eq!(
            fragments,
            [
                dir.user().join("10.toml"),
                dir.package().join("2.toml"),
                dir.user().join("Z.toml"),
                dir.package().join("a.toml"),
                dir.user().join("null.toml"),
                dir.user().join("shadowed.toml"),
            ]
        );
    }

    #[test]
    fn missing_directories_are_empty() {
        let dir = TestDir::new();
        let dirs = FragmentDirs::new(Some(dir.0.join("nope")), None);
        assert!(dirs.fragments().is_empty());
        let loaded = dirs.load().unwrap();
        assert!(loaded.config.blocks.is_empty());
        assert!(loaded.skipped.is_empty());
    }

    #[test]
    fn user_fragment_masks_package_fragment() {
        let dir = TestDir::new();
        write(&dir.package(), "a.toml", &custom("PACKAGE"));
        write(&dir.package(), "b.toml", "not toml");
        write(&dir.package(), "c.toml", &custom("PACKAGE"));
        write(&dir.user(), "a.toml", "# nothing\n");
        write(&dir.user(), "b.toml", &custom("USER"));
        std::os::unix::fs::symlink("/dev/null", dir.user().join("c.toml")).unwrap();

        let loaded = dir.dirs().load().unwrap();
        assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
        assert_eq!(loaded.config.blocks.len(), 1);
    }

    #[test]
    fn broken_fragments_are_skipped_whole() {
        let dir = TestDir::new();
        write(
            &dir.package(),
            "10.toml",
            &(with_id("a", "A") + &with_id("b", "B")),
        );
        let syntax = write(&dir.user(), "20.toml", "[theme]\ntheme = \"x\"\nbad [[[\n");
        let utf8 = dir.user().join("30.toml");
        fs::write(&utf8, b"a = \"\xff\"\n").unwrap();
        let config = write(
            &dir.user(),
            "40.toml",
            "[[block]]\nid = \"a\"\nremove = true\n[[block]]\nblock = \"no_such_block\"\n",
        );
        let key = write(&dir.user(), "50.toml", "bogus_key = 1\n");
        let colour = write(
            &dir.user(),
            "60.toml",
            "[theme.overrides]\nidle_fg = \"not-a-colour\"\n",
        );
        write(&dir.user(), "70.toml", "[[block]]\nid = \"b\"\n");

        let loaded = dir.dirs().load().unwrap();
        let skipped: Vec<_> = loaded.skipped.iter().map(|s| &s.path).collect();
        assert_eq!(skipped, [&syntax, &utf8, &config, &key, &colour]);
        assert!(loaded.skipped[0].reason.contains("line 3"));
        assert!(loaded.skipped.iter().all(|s| !s.reason.contains('\n')));
        assert_eq!(loaded.skipped[0].file_name(), "20.toml");
        let names: Vec<_> = loaded
            .config
            .blocks
            .iter()
            .map(|block| block.config.name())
            .collect();
        assert_eq!(names, ["custom", "custom"]);
    }

    #[test]
    fn block_settings_errors_do_not_skip_fragments() {
        let dir = TestDir::new();
        write(
            &dir.package(),
            "10.toml",
            "[[block]]\nblock = \"custom\"\nunknown_setting = true\n",
        );
        let loaded = dir.dirs().load().unwrap();
        assert!(loaded.skipped.is_empty());
        assert_eq!(loaded.config.blocks.len(), 1);
    }

    #[test]
    fn snapshot_follows_fragments_only() {
        let dir = TestDir::new();
        let fragment = write(&dir.user(), "a.toml", &custom("A"));
        let snapshot = dir.dirs().snapshot();
        write(&dir.user(), "a.toml~", "");
        write(&dir.user(), "b.toml.dpkg-dist", "");
        assert_eq!(dir.dirs().snapshot(), snapshot);
        fs::write(&fragment, custom("B")).unwrap();
        assert_ne!(dir.dirs().snapshot(), snapshot);
    }

    #[test]
    fn watch_target_is_nearest_existing_directory() {
        let dir = TestDir::new();
        assert_eq!(watch_target(&dir.user()), Some(dir.user()));
        assert_eq!(watch_target(&dir.0.join("a/b/c")), Some(dir.0.clone()));
        assert_eq!(watch_target(Path::new("no-such-dir")), Some(".".into()));
    }

    async fn reloads_within(reloads: &mut (impl Stream<Item = ()> + Unpin), secs: u64) -> bool {
        tokio::time::timeout(Duration::from_secs(secs), reloads.next())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn fragment_changes_cause_reloads() {
        let dir = TestDir::new();
        let dirs = dir.dirs();
        let watcher = dirs.watch();
        let loaded = dirs.load().unwrap();
        let mut reloads = Box::pin(watcher.reloads(loaded.snapshot));

        write(&dir.user(), "a.toml~", &custom("A"));
        write(&dir.user(), ".a.toml.swp", &custom("A"));
        assert!(!reloads_within(&mut reloads, 1).await);

        write(&dir.user(), "a.toml", &custom("A"));
        assert!(reloads_within(&mut reloads, 5).await);

        let staged = write(&dir.user(), ".staged", &custom("B"));
        fs::rename(staged, dir.user().join("a.toml")).unwrap();
        assert!(reloads_within(&mut reloads, 5).await);

        fs::remove_file(dir.user().join("a.toml")).unwrap();
        assert!(reloads_within(&mut reloads, 5).await);
    }

    #[tokio::test]
    async fn directories_created_later_are_watched() {
        let dir = TestDir::new();
        let user = dir.0.join("later/user");
        let dirs = FragmentDirs::new(Some(dir.package()), Some(user.clone()));
        let watcher = dirs.watch();
        let loaded = dirs.load().unwrap();
        let mut reloads = Box::pin(watcher.reloads(loaded.snapshot));

        fs::create_dir_all(&user).unwrap();
        assert!(!reloads_within(&mut reloads, 1).await);
        write(&user, "a.toml", &custom("LATE"));
        assert!(reloads_within(&mut reloads, 5).await);
    }

    #[test]
    fn shipped_fragments_equal_config_toml() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dirs = FragmentDirs::new(Some(root.join("conf.d")), None);
        let mut merged = Merged::default();
        for path in dirs.fragments() {
            let text = fs::read_to_string(&path).unwrap();
            merged
                .apply(toml::from_str(&text).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        }
        let config: Table =
            toml::from_str(&fs::read_to_string(root.join("config.toml")).unwrap()).unwrap();
        assert_eq!(merged.to_document(), config);
    }
}
