//! Project discovery: recursive scans for git repositories.
//!
//! Used by the startup flow (cwd is not a repo → prompt to scan subfolders)
//! and the `I` key (rescan `[discovery].scan_root`, add new repos).
//!
//! Scan semantics:
//! - A directory whose `.git` entry is a DIRECTORY is a repository and is
//!   reported; the walk does not descend into it (repos nested inside a
//!   repo — vendored or submodule trees — are that repo's own business).
//! - A directory whose `.git` entry is a FILE is a linked worktree or
//!   submodule: skipped, covered by its main repo.
//! - Hidden directories (names starting with `.`) and the configured
//!   `skip_dirs` are never entered.
//! - Depth is bounded by `[discovery].max_depth` (0 = scan root only).
//! - Symlinks are not followed (avoids cycles; the walk only descends into
//!   real directories).
//!
//! Results are cached in `$XDG_CACHE_HOME/lazywingman/discovered.json`
//! (root + timestamp + paths) — regenerable data, per XDG cache semantics.
//! The cache is written after every scan and read at startup to tell the
//! prompt how many repos a previous scan of this root found.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// One scan's persisted result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryCache {
    /// The root that was scanned (absolute).
    pub root: PathBuf,
    /// Unix seconds at scan time (informational / audit).
    pub scanned_at: u64,
    /// Repositories found, absolute, sorted, deduplicated.
    pub paths: Vec<PathBuf>,
}

/// Path of the discovery cache file.
pub fn cache_path() -> PathBuf {
    crate::util::discovery_cache_path()
}

/// Load the cache; `None` when missing or unparsable (it is regenerable).
pub fn load_cache() -> Option<DiscoveryCache> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist a scan result.
pub fn save_cache(cache: &DiscoveryCache) -> Result<()> {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(cache)
        .map_err(|e| crate::error::Error::State(format!("discovery cache: {e}")))?;
    std::fs::write(&path, text)?;
    log::info!(
        "discovery cache written: {} repo(s) under {}",
        cache.paths.len(),
        cache.root.display()
    );
    Ok(())
}

/// Recursively scan `root` for git repositories (see module docs).
///
/// `max_depth` bounds directory depth (0 = only `root` itself is checked);
/// `skip_dirs` names are never entered (hidden dirs are always skipped).
/// Returns absolute, sorted, deduplicated paths.
pub fn scan(root: &Path, max_depth: usize, skip_dirs: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk(root, 0, max_depth, skip_dirs, &mut out, &mut seen);
    out.sort();
    out.dedup();
    out
}

fn walk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    skip_dirs: &[String],
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    // Symlink-cycle guard: only descend into each (lexically normalized)
    // directory once. Symlinked directories are not followed at all — the
    // entry below is only descended into when `file_type` says real dir.
    if !seen.insert(crate::util::normalize_path(dir)) {
        return;
    }
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            // A `.git` entry decides what THIS directory is.
            let git = dir.join(".git");
            if git.exists() {
                if git.is_dir() {
                    // A repository: report it and do not descend — repos
                    // nested inside a repo are that repo's business.
                    out.push(dir.to_path_buf());
                }
                // `.git` FILE → linked worktree / submodule: covered by
                // its main repo (which the startup auto-import handles).
                return;
            }
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || skip_dirs.contains(&name) {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    walk(&entry.path(), depth + 1, max_depth, skip_dirs, out, seen);
                }
            }
        }
        Err(e) => {
            log::debug!("scan: cannot read {}: {e}", dir.display());
        }
    }
}

/// Unix seconds, for the cache timestamp.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testutil::with_fresh_xdg;
    use std::fs;

    /// Create `<root>/<rel>` as a real git repo (`.git` directory).
    fn make_repo(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(&p).unwrap();
        fs::create_dir_all(p.join(".git")).unwrap();
        p
    }

    /// Create `<root>/<rel>` with a `.git` FILE — a linked worktree /
    /// submodule, which must be skipped.
    fn make_worktree(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        p
    }

    fn default_skip() -> Vec<String> {
        vec![
            ".git".into(),
            "node_modules".into(),
            "target".into(),
            ".lazywingman-worktrees".into(),
        ]
    }

    #[test]
    fn scan_finds_nested_repos_and_skips_worktrees_deps_and_hidden() {
        let root = tempfile::TempDir::new().unwrap();
        let r = root.path();
        make_repo(r, "repo-a");
        make_repo(r, "repo-b");
        // dep trees are skipped
        make_repo(r, "repo-a/node_modules/pkg");
        make_repo(r, "repo-a/target/debug");
        // worktrees (`.git` file) are skipped — their main repo covers them
        make_worktree(r, "wt-of-a");
        // hidden dirs are never entered
        make_repo(r, ".hidden/repo-c");
        // deep repos: within max_depth found, beyond it not
        // (root=0, so root/1/2/3/repo-c sits at depth 4)
        make_repo(r, "1/2/3/repo-c"); // depth 4 → found
        make_repo(r, "1/2/3/4/repo-d"); // depth 5 > 4 → not found

        let found = scan(r, 4, &default_skip());

        let mut expected = vec![r.join("1/2/3/repo-c"), r.join("repo-a"), r.join("repo-b")];
        expected.sort();
        assert_eq!(found, expected, "found: {found:?}");
    }

    #[test]
    fn scan_reports_root_itself_when_it_is_a_repo() {
        let root = tempfile::TempDir::new().unwrap();
        let r = root.path();
        fs::create_dir_all(r.join(".git")).unwrap();
        assert_eq!(scan(r, 4, &default_skip()), vec![r.to_path_buf()]);
    }

    #[test]
    fn scan_respects_custom_skip_dirs_and_depth() {
        let root = tempfile::TempDir::new().unwrap();
        let r = root.path();
        make_repo(r, "vendor/lib");
        make_repo(r, "src/app");
        // skip "vendor" → only src/app
        let found = scan(r, 4, &["vendor".to_string()]);
        assert_eq!(found, vec![r.join("src/app")]);
        // max_depth 0 → only the root itself is checked (not a repo here)
        assert!(scan(r, 0, &default_skip()).is_empty());
        // max_depth 1 → only direct children
        let found = scan(r, 1, &default_skip());
        assert!(found.is_empty(), "src/app is at depth 2: {found:?}");
    }

    #[test]
    fn scan_is_sorted_and_deduped() {
        let root = tempfile::TempDir::new().unwrap();
        let r = root.path();
        make_repo(r, "b-repo");
        make_repo(r, "a-repo");
        let found = scan(r, 4, &default_skip());
        assert_eq!(found, vec![r.join("a-repo"), r.join("b-repo")], "sorted");
    }

    #[test]
    fn scan_missing_root_is_empty() {
        let root = tempfile::TempDir::new().unwrap();
        let gone = root.path().join("does-not-exist");
        assert!(scan(&gone, 4, &default_skip()).is_empty());
    }

    #[test]
    fn cache_roundtrip() {
        with_fresh_xdg(|| {
            let cache = DiscoveryCache {
                root: PathBuf::from("/work"),
                scanned_at: 1_234_567_890,
                paths: vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")],
            };
            save_cache(&cache).unwrap();
            assert!(cache_path().exists());
            let loaded = load_cache().unwrap();
            assert_eq!(loaded, cache);
            assert_eq!(loaded.root, PathBuf::from("/work"));
            assert_eq!(loaded.paths.len(), 2);
        });
    }

    #[test]
    fn cache_missing_or_corrupt_loads_as_none() {
        with_fresh_xdg(|| {
            assert!(load_cache().is_none());
            fs::create_dir_all(cache_path().parent().unwrap()).unwrap();
            fs::write(cache_path(), "{not json").unwrap();
            assert!(load_cache().is_none());
        });
    }

    #[test]
    fn cache_is_keyed_by_root() {
        with_fresh_xdg(|| {
            let cache = DiscoveryCache {
                root: PathBuf::from("/one"),
                scanned_at: 1,
                paths: vec![],
            };
            save_cache(&cache).unwrap();
            // a different root must not match the cached one
            let loaded = load_cache().unwrap();
            assert_eq!(loaded.root, PathBuf::from("/one"));
            assert_ne!(loaded.root, PathBuf::from("/two"));
        });
    }
}
