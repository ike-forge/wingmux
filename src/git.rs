//! Git operations: project validation, worktree management, branch naming.
//!
//! All operations shell out to the `git` binary with `-C <dir>` so paths with
//! special characters are never interpreted by a shell. Errors carry git's
//! stderr so the user sees the real reason (e.g. "branch already exists").

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// Result of a git invocation.
#[derive(Debug)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

/// A worktree as reported by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Full ref, e.g. `refs/heads/feature-x`. `None` when detached.
    pub branch: Option<String>,
}

impl WorktreeInfo {
    pub fn branch_short(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .and_then(|b| b.strip_prefix("refs/heads/"))
    }
}

/// Max length for generated branch names (git limits ~245; stay well under).
const MAX_BRANCH_LEN: usize = 120;

/// Turn a feature name into a git-safe branch name / directory component:
/// lowercase, non `[a-z0-9._-]` characters become `-`, runs collapsed and
/// trimmed, truncated to [`MAX_BRANCH_LEN`]. Empty input → `"feature"`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.to_lowercase().chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
        if ok {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return "feature".to_string();
    }
    let mut out: String = out.chars().take(MAX_BRANCH_LEN).collect();
    out.truncate(MAX_BRANCH_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "feature".to_string()
    } else {
        out
    }
}

/// The git client. Stateless; every call resolves the binary fresh.
pub struct Git;

impl Git {
    fn command(dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        c.args(args);
        c
    }

    fn run(dir: &Path, args: &[&str]) -> Result<GitOutput> {
        let out = Self::command(dir, args).output()?;
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Run git once, returning Ok(()) only on exit code 0.
    fn run_ok(dir: &Path, args: &[&str]) -> Result<GitOutput> {
        let out = Self::command(dir, args).output()?;
        if !out.status.success() {
            return Err(Error::Git(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Is `dir` inside a git work tree?
    pub fn is_repo(&self, dir: &Path) -> bool {
        Self::run(dir, &["rev-parse", "--is-inside-work-tree"])
            .map(|o| o.stdout.trim() == "true")
            .unwrap_or(false)
    }

    /// Repository name: the basename of the path.
    pub fn repo_name(&self, dir: &Path) -> String {
        dir.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string())
    }

    /// The current branch of the repo's HEAD.
    pub fn current_branch(&self, dir: &Path) -> Result<String> {
        let out = Self::run_ok(dir, &["branch", "--show-current"])?;
        let b = out.stdout.trim().to_string();
        if b.is_empty() {
            // detached HEAD: use the symbolic ref if resolvable
            let out = Self::run_ok(dir, &["symbolic-ref", "--short", "HEAD"])?;
            Ok(out.stdout.trim().to_string())
        } else {
            Ok(b)
        }
    }

    /// Does the branch exist locally?
    pub fn branch_exists(&self, dir: &Path, branch: &str) -> bool {
        Self::run(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .map(|o| o.stdout.trim() != "")
        .unwrap_or(false)
    }

    /// The branch new feature branches are cut from: the configured base if
    /// it exists in this repo, otherwise the repo's current branch.
    pub fn resolve_base_branch(&self, dir: &Path, configured: &str) -> Result<String> {
        if !configured.is_empty() && self.branch_exists(dir, configured) {
            return Ok(configured.to_string());
        }
        if !configured.is_empty() {
            log::warn!(
                "base branch '{}' not found in {}, falling back to current branch",
                configured,
                dir.display()
            );
        }
        self.current_branch(dir)
    }

    /// List the repo's worktrees (main + linked), parsed from porcelain.
    pub fn worktree_list(&self, dir: &Path) -> Result<Vec<WorktreeInfo>> {
        let out = Self::run_ok(dir, &["worktree", "list", "--porcelain"])?;
        Ok(parse_worktree_porcelain(&out.stdout))
    }

    /// Create a linked worktree with a new branch cut from `base`.
    pub fn worktree_add(&self, dir: &Path, wt_path: &Path, branch: &str, base: &str) -> Result<()> {
        log::info!(
            "worktree add: -b {branch} {} (base {base})",
            wt_path.display()
        );
        Self::run_ok(
            dir,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                wt_path.to_str().unwrap_or_default(),
                base,
            ],
        )?;
        Ok(())
    }

    /// Remove a linked worktree. `force` is required when it has changes.
    /// Removing something that isn't a worktree anymore is a no-op.
    pub fn worktree_remove(&self, wt_path: &Path, force: bool) -> Result<()> {
        // git refuses to remove a worktree from inside itself, and running
        // from an unrelated repo misidentifies it — so run from the worktree's
        // main repo, discovered via its `.git` file. No `.git` file means the
        // worktree is already gone.
        let Some(main) = main_repo_of_worktree(wt_path) else {
            log::warn!("worktree {} already gone, ignoring", wt_path.display());
            return Ok(());
        };
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(wt_path.to_str().unwrap_or_default());
        let out = Self::command(&main, &args).output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("not a working tree") {
                log::warn!("worktree {} already gone, ignoring", wt_path.display());
                return Ok(());
            }
            return Err(Error::Git(format!(
                "git worktree remove {} failed: {}",
                wt_path.display(),
                stderr.trim()
            )));
        }
        log::info!("worktree removed: {}", wt_path.display());
        Ok(())
    }

    /// Does the worktree have uncommitted changes?
    pub fn worktree_dirty(&self, wt_path: &Path) -> Result<bool> {
        let out = Self::run_ok(wt_path, &["status", "--porcelain"])?;
        Ok(!out.stdout.trim().is_empty())
    }

    /// Delete a local branch (`-D`, force — the branch is orphaned once its
    /// worktree is gone and the feature is done).
    pub fn branch_delete(&self, dir: &Path, branch: &str) -> Result<()> {
        log::info!("deleting branch {branch} in {}", dir.display());
        Self::run_ok(dir, &["branch", "-D", branch])?;
        Ok(())
    }
}

/// Parse `git worktree list --porcelain` output.
pub fn parse_worktree_porcelain(text: &str) -> Vec<WorktreeInfo> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut wt = WorktreeInfo {
            path: PathBuf::new(),
            head: None,
            branch: None,
        };
        let mut has_path = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                wt.path = PathBuf::from(p);
                has_path = true;
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                wt.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                wt.branch = Some(b.to_string());
            }
            // "detached" lines are ignored: branch stays None.
        }
        if has_path {
            out.push(wt);
        }
    }
    out
}

/// Is the path a linked worktree of `dir`? (Used by state reconciliation.)
pub fn is_linked_worktree(dir: &Path, candidate: &Path) -> Result<bool> {
    for wt in Git.worktree_list(dir)? {
        if wt.path == candidate {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Main repo of a linked worktree, discovered via the worktree's `.git` file
/// (`gitdir: <main>/.git/worktrees/<id>`).
pub(crate) fn main_repo_of_worktree(wt_path: &Path) -> Option<PathBuf> {
    let gitfile = wt_path.join(".git");
    if !gitfile.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(gitfile).ok()?;
    let admin = content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir: "))
        .map(PathBuf::from)?;
    // <admin>/worktrees/<id> → walk up to the repo root
    let parent = admin.parent()?.parent()?.parent()?;
    Some(parent.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a throwaway git repo with one commit and a `main` branch.
    fn temp_repo(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(name);
        fs::create_dir_all(&path).unwrap();
        run(&path, &["init", "-b", "main"]);
        run(&path, &["config", "user.name", "lw-test"]);
        run(&path, &["config", "user.email", "lw-test@example.com"]);
        fs::write(path.join("readme.md"), "# test\n").unwrap();
        run(&path, &["add", "."]);
        run(&path, &["commit", "-m", "initial"]);
        (dir, path)
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("My Feature!"), "my-feature");
        assert_eq!(slugify("  x  "), "x");
        assert_eq!(slugify(""), "feature");
        assert_eq!(slugify("a_b.c-d"), "a_b.c-d");
        assert_eq!(slugify("branch/name"), "branch-name");
        assert_eq!(slugify("café"), "caf");
        assert_eq!(slugify("---"), "feature");
    }

    #[test]
    fn slugify_truncates_and_trim() {
        let long = "x".repeat(300);
        let s = slugify(&long);
        assert!(s.len() <= MAX_BRANCH_LEN);
        let mixed = format!("{}!!!", "y".repeat(200));
        let s = slugify(&mixed);
        assert!(!s.ends_with('-'));
        assert!(s.len() <= MAX_BRANCH_LEN);
    }

    #[test]
    fn repo_detection_and_name() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        assert!(git.is_repo(&repo));
        assert_eq!(git.repo_name(&repo), "proj");
        assert!(!git.is_repo(repo.parent().unwrap()));
    }

    #[test]
    fn current_branch_and_exists() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        assert_eq!(git.current_branch(&repo).unwrap(), "main");
        assert!(git.branch_exists(&repo, "main"));
        assert!(!git.branch_exists(&repo, "nope"));
        assert_eq!(git.resolve_base_branch(&repo, "main").unwrap(), "main");
        // configured branch missing → falls back to current branch
        assert_eq!(git.resolve_base_branch(&repo, "master").unwrap(), "main");
    }

    #[test]
    fn worktree_add_list_remove_roundtrip() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt_path = repo.parent().unwrap().join("wt-feature-x");

        // initial list has just the main worktree
        let list = git.worktree_list(&repo).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, repo);

        git.worktree_add(&repo, &wt_path, "feature-x", "main")
            .unwrap();
        assert!(wt_path.exists());
        assert!(git.branch_exists(&repo, "feature-x"));

        let list = git.worktree_list(&repo).unwrap();
        assert_eq!(list.len(), 2);
        let wt = list.iter().find(|w| w.path == wt_path).unwrap();
        assert_eq!(wt.branch_short(), Some("feature-x"));
        assert!(wt.head.is_some());

        // dirty check: clean worktree
        assert!(!git.worktree_dirty(&wt_path).unwrap());
        fs::write(wt_path.join("change.txt"), "dirty").unwrap();
        assert!(git.worktree_dirty(&wt_path).unwrap());

        // remove without force on a dirty worktree fails
        assert!(git.worktree_remove(&wt_path, false).is_err());
        git.worktree_remove(&wt_path, true).unwrap();
        assert!(!wt_path.exists());
        assert_eq!(git.worktree_list(&repo).unwrap().len(), 1);

        // removing again is a no-op success
        git.worktree_remove(&wt_path, false).unwrap();
    }

    #[test]
    fn worktree_add_duplicate_branch_fails() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt_path = repo.parent().unwrap().join("wt-1");
        git.worktree_add(&repo, &wt_path, "feat", "main").unwrap();
        let wt2 = repo.parent().unwrap().join("wt-2");
        let err = git.worktree_add(&repo, &wt2, "feat", "main").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn porcelain_parser() {
        let text = "worktree /main\nHEAD abc123\n\nworktree /wt/feat\nHEAD def456\nbranch refs/heads/feat\n\ndetached\n";
        let list = parse_worktree_porcelain(text);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, PathBuf::from("/main"));
        assert_eq!(list[0].branch, None);
        assert_eq!(list[1].branch_short(), Some("feat"));
    }

    #[test]
    fn is_repo_rejects_plain_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!Git.is_repo(dir.path()));
    }
}
