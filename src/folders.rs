//! The folder view and the hide model behind both views.
//!
//! Only explicit choices are stored: repository paths hidden one by one and
//! folder paths hidden as a whole. Everything else is derived from them:
//!
//! - a repository is hidden when it is in the repository set, or when a
//!   folder above it (below the scope root) is in the folder set — so a
//!   repository that appears later in a hidden folder starts hidden;
//! - a folder shows as hidden when every repository below it is hidden. That
//!   is never stored: a folder whose repositories were hidden one by one
//!   comes back when a new repository appears in it.
//!
//! Folders are derived from the repository paths, so a folder holding no
//! repository has no row. A folder whose only child is another folder is
//! merged with it (`a/b/c` is one row), as in VS Code's compact folders.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A folder row of the folder view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// The deepest directory of a merged chain: what hiding it stores.
    pub path: PathBuf,
    /// The chain relative to the parent row, `/`-joined (`old_/archive`).
    pub label: String,
    /// The folder row above it; `None` at the scope root.
    pub parent: Option<usize>,
    /// Every repository below it, by repository index.
    pub repositories: Vec<usize>,
}

/// The folder rows over a set of repositories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folders {
    /// The directory the tree starts from. Folders at or above it are never
    /// rows and never hide anything.
    pub root: PathBuf,
    /// In tree order: a folder comes before its children, siblings by path.
    pub folders: Vec<Folder>,
    /// The folder row each repository sits in, by repository index; `None`
    /// at the root and for repositories outside it.
    pub parent_of: Vec<Option<usize>>,
}

impl Folders {
    /// Group `repos` (index, path) under `root`. `n` is the number of
    /// repository indices, so `parent_of` covers every one.
    pub fn build(root: &Path, repos: &[(usize, &Path)], n: usize) -> Folders {
        let paths: Vec<&Path> = repos.iter().map(|(_, p)| *p).collect();
        let mut dirs: BTreeSet<&Path> = BTreeSet::new();
        for p in &paths {
            dirs.extend(folders_above(p, root));
        }
        // A directory whose only child is another directory is merged into
        // that child's row.
        let merged = |d: &Path| {
            let kids = children(d, &paths);
            kids.len() == 1 && dirs.contains(kids[0].as_path())
        };
        let shown: BTreeSet<&Path> = dirs.iter().copied().filter(|d| !merged(d)).collect();
        fn up<'a>(p: &'a Path, root: &'a Path, shown: &BTreeSet<&Path>) -> Option<&'a Path> {
            folders_above(p, root).find(|a| shown.contains(a))
        }
        let row_above = |p| up(p, root, &shown);

        let mut folders: Vec<Folder> = Vec::new();
        let mut index = std::collections::HashMap::new();
        // BTreeSet order is a pre-order walk: a parent sorts before its
        // children, and siblings by path.
        for d in &shown {
            let parent = row_above(d).map(|a| index[a]);
            let base = row_above(d).unwrap_or(root);
            let label = d
                .strip_prefix(base)
                .unwrap_or(d)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            index.insert(*d, folders.len());
            folders.push(Folder {
                path: d.to_path_buf(),
                label,
                parent,
                repositories: repos
                    .iter()
                    .filter(|(_, p)| p.starts_with(d))
                    .map(|(ri, _)| *ri)
                    .collect(),
            });
        }
        let mut parent_of = vec![None; n];
        for (ri, p) in repos {
            if let Some(slot) = parent_of.get_mut(*ri) {
                *slot = row_above(p).map(|a| index[a]);
            }
        }
        Folders {
            root: root.to_path_buf(),
            folders,
            parent_of,
        }
    }

    /// The folder rows directly under `parent` (`None`: the root).
    pub fn children_of(&self, parent: Option<usize>) -> impl Iterator<Item = usize> + '_ {
        (0..self.folders.len()).filter(move |&fi| self.folders[fi].parent == parent)
    }
}

/// The deepest directory holding every one of `paths` as a descendant.
pub fn common_root<'a>(paths: impl IntoIterator<Item = &'a Path>) -> PathBuf {
    let mut root: Option<PathBuf> = None;
    for p in paths {
        let parent = p.parent().unwrap_or(p);
        root = Some(match root {
            None => parent.to_path_buf(),
            Some(r) => r
                .ancestors()
                .find(|a| parent.starts_with(a))
                .unwrap_or(Path::new(""))
                .to_path_buf(),
        });
    }
    root.unwrap_or_default()
}

/// The directories strictly between `root` and `path`, nearest first.
fn folders_above<'a>(path: &'a Path, root: &'a Path) -> impl Iterator<Item = &'a Path> {
    path.ancestors()
        .skip(1)
        .take_while(move |a| a.starts_with(root) && *a != root)
}

/// The entries directly under `dir` that lead to one of `repos`: a
/// repository, or a directory holding one.
fn children(dir: &Path, repos: &[&Path]) -> Vec<PathBuf> {
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for r in repos {
        if let Ok(rest) = r.strip_prefix(dir)
            && let Some(first) = rest.components().next()
        {
            out.insert(dir.join(first));
        }
    }
    out.into_iter().collect()
}

/// Is the repository at `path` hidden, on its own or by a folder above it?
pub fn is_hidden(
    path: &Path,
    root: &Path,
    hidden: &BTreeSet<PathBuf>,
    hidden_folders: &BTreeSet<PathBuf>,
) -> bool {
    hidden.contains(path) || folders_above(path, root).any(|a| hidden_folders.contains(a))
}

/// Show `target` — a repository, or a folder and every repository below it.
///
/// A folder hide above the target is replaced by hides of everything beside
/// the way down to it, folders as folders: only the target comes back, and
/// a sibling folder still takes the repositories that appear in it later.
/// A folder whose repositories were all hidden one by one brings them back
/// too, since it would not show otherwise.
pub fn unhide(
    target: &Path,
    root: &Path,
    repos: &[&Path],
    hidden: &mut BTreeSet<PathBuf>,
    hidden_folders: &mut BTreeSet<PathBuf>,
) {
    let above: Vec<PathBuf> = folders_above(target, root)
        .filter(|a| hidden_folders.contains(*a))
        .map(Path::to_path_buf)
        .collect();
    for folder in above.iter().rev() {
        hidden_folders.remove(folder);
        for dir in target.ancestors().skip(1) {
            if !dir.starts_with(folder) {
                break;
            }
            for child in children(dir, repos) {
                if target.starts_with(&child) {
                    continue;
                }
                if repos.contains(&child.as_path()) {
                    hidden.insert(child);
                } else {
                    hidden_folders.insert(child);
                }
            }
        }
    }
    hidden.remove(target);
    hidden_folders.retain(|f| !f.starts_with(target));
    let below: Vec<&Path> = repos
        .iter()
        .copied()
        .filter(|r| r.starts_with(target) && *r != target)
        .collect();
    if !below.is_empty() && below.iter().all(|r| hidden.contains(*r)) {
        for r in below {
            hidden.remove(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn build(paths: &[&str]) -> Folders {
        let repos: Vec<(usize, &Path)> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i, Path::new(*p)))
            .collect();
        Folders::build(Path::new("/s"), &repos, paths.len())
    }

    #[test]
    fn folders_come_from_repository_paths_and_single_child_chains_merge() {
        let f = build(&[
            "/s/repoA",
            "/s/old_/repoA",
            "/s/old_/repoB",
            "/s/deep/a/b/repoC",
            "/s/deep/x/repoD",
        ]);
        let rows: Vec<(&str, Option<usize>)> = f
            .folders
            .iter()
            .map(|d| (d.label.as_str(), d.parent))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("deep", None),
                ("a/b", Some(0)),
                ("x", Some(0)),
                ("old_", None)
            ]
        );
        assert_eq!(f.folders[1].path, Path::new("/s/deep/a/b"));
        assert_eq!(f.parent_of, vec![None, Some(3), Some(3), Some(1), Some(2)]);
        assert_eq!(f.folders[0].repositories, vec![3, 4]);
    }

    #[test]
    fn a_folder_with_one_folder_inside_is_one_row_up_to_the_repositories() {
        let f = build(&["/s/a/b/c/r1", "/s/a/b/c/r2"]);
        assert_eq!(f.folders.len(), 1);
        assert_eq!(f.folders[0].label, "a/b/c");
        assert_eq!(f.parent_of, vec![Some(0), Some(0)]);
    }

    #[test]
    fn a_hidden_folder_hides_every_repository_below_it_including_new_ones() {
        let folders = set(&["/s/old_"]);
        let none = BTreeSet::new();
        let root = Path::new("/s");
        assert!(is_hidden(Path::new("/s/old_/repoC"), root, &none, &folders));
        assert!(is_hidden(Path::new("/s/old_/x/y"), root, &none, &folders));
        assert!(!is_hidden(Path::new("/s/repoA"), root, &none, &folders));
    }

    #[test]
    fn the_root_and_the_folders_above_it_never_hide() {
        let folders = set(&["/s", "/"]);
        let none = BTreeSet::new();
        assert!(!is_hidden(
            Path::new("/s/r"),
            Path::new("/s"),
            &none,
            &folders
        ));
    }

    #[test]
    fn unhiding_a_repository_under_a_hidden_folder_hides_its_siblings_one_level_at_a_time() {
        let repos = [
            Path::new("/s/old_/a"),
            Path::new("/s/old_/sub/b"),
            Path::new("/s/old_/sub/c"),
            Path::new("/s/old_/other/d"),
        ];
        let mut hidden = BTreeSet::new();
        let mut folders = set(&["/s/old_"]);
        unhide(
            Path::new("/s/old_/sub/b"),
            Path::new("/s"),
            &repos,
            &mut hidden,
            &mut folders,
        );
        assert_eq!(hidden, set(&["/s/old_/a", "/s/old_/sub/c"]));
        assert_eq!(folders, set(&["/s/old_/other"]));
    }

    #[test]
    fn unhiding_a_folder_keeps_its_own_hides_unless_they_are_all_of_it() {
        let repos = [Path::new("/s/f/a"), Path::new("/s/f/b")];
        let root = Path::new("/s");
        let mut hidden = set(&["/s/f/a"]);
        let mut folders = set(&["/s/f"]);
        unhide(Path::new("/s/f"), root, &repos, &mut hidden, &mut folders);
        assert!(folders.is_empty());
        assert_eq!(hidden, set(&["/s/f/a"]), "a hide of its own survives");

        let mut hidden = set(&["/s/f/a", "/s/f/b"]);
        let mut folders = BTreeSet::new();
        unhide(Path::new("/s/f"), root, &repos, &mut hidden, &mut folders);
        assert!(
            hidden.is_empty(),
            "a folder hidden only through its repositories comes back"
        );
    }

    #[test]
    fn unhiding_a_folder_drops_the_folder_hides_below_it() {
        let repos = [Path::new("/s/f/g/a"), Path::new("/s/f/b")];
        let mut hidden = BTreeSet::new();
        let mut folders = set(&["/s/f", "/s/f/g"]);
        unhide(
            Path::new("/s/f"),
            Path::new("/s"),
            &repos,
            &mut hidden,
            &mut folders,
        );
        assert!(folders.is_empty());
        assert!(hidden.is_empty());
    }
}
