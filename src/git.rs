//! Git operations: repository validation, worktree management, branch naming.
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
}

/// How much work a worktree is carrying, relative to its repository's
/// base branch. Three states, and the question each one answers is "has
/// anything happened here that the base branch does not already have?".
///
/// The working tree's cleanliness is folded in on purpose: to the user a
/// worktree an agent has edited but not yet committed is *not* a fresh
/// cut, and drawing it as one hides live work behind the "nothing here"
/// icon. Uncommitted changes therefore count exactly like commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorktreeState {
    /// Untouched: no commits of its own and a clean working tree —
    /// freshly cut from the base branch, nothing has happened yet.
    #[default]
    New,
    /// Carries work the base branch does not have: commits of its own, an
    /// unclean working tree, or both.
    Ahead,
    /// Every commit of its own is already in the base branch and the
    /// working tree is clean: done, and the worktree is free to delete.
    Merged,
    /// The worktree directory is gone — removed by the agent that was
    /// working in it, or by hand — while the row is still in the state
    /// file. Nothing can be measured any more, and nothing can be done
    /// with the row but delete it.
    Deleted,
}

/// A worktree as reported by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Full ref, e.g. `refs/heads/worktree-x`. `None` when detached.
    pub branch: Option<String>,
}

impl WorktreeInfo {
    /// Short branch name (`refs/heads/x` → `x`).
    pub fn branch_short(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .and_then(|b| b.strip_prefix("refs/heads/"))
    }
}

/// Max length for generated branch names (git limits ~245; stay well under).
const MAX_BRANCH_LEN: usize = 120;

/// Turn a worktree name into a git-safe branch name / directory component:
/// lowercase, non `[a-z0-9._-]` characters become `-`, runs collapsed and
/// trimmed, truncated to [`MAX_BRANCH_LEN`]. Empty input → `"worktree"`.
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
        return "worktree".to_string();
    }
    let mut out: String = out.chars().take(MAX_BRANCH_LEN).collect();
    out.truncate(MAX_BRANCH_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "worktree".to_string()
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
        })
    }

    /// Run git once for its exit code alone (git's own predicates —
    /// `merge-base --is-ancestor` — answer that way, and a non-zero exit
    /// is the answer, not an error). A git that will not run at all reads
    /// as `false`.
    fn run_status(dir: &Path, args: &[&str]) -> bool {
        Self::command(dir, args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
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
        })
    }

    /// Is `dir` inside a git work tree?
    pub fn is_repo(&self, dir: &Path) -> bool {
        Self::run(dir, &["rev-parse", "--is-inside-work-tree"])
            .map(|o| o.stdout.trim() == "true")
            .unwrap_or(false)
    }

    /// The work tree root of `dir` (works from any subdirectory, including
    /// inside a linked worktree). `None` when `dir` is not in a repo.
    pub fn repo_root(&self, dir: &Path) -> Option<PathBuf> {
        let out = Self::run(dir, &["rev-parse", "--show-toplevel"]).ok()?;
        let root = out.stdout.trim();
        if root.is_empty() {
            None
        } else {
            Some(PathBuf::from(root))
        }
    }

    /// The MAIN repository of `dir`'s work tree: when `dir` is inside a
    /// linked worktree this resolves to the main checkout (`git worktree
    /// list` always lists the main worktree first); otherwise it is the
    /// work tree root itself. `None` when `dir` is not in a repo.
    pub fn main_repo(&self, dir: &Path) -> Option<PathBuf> {
        let top = self.repo_root(dir)?;
        self.worktree_list(&top)
            .ok()
            .and_then(|wts| wts.into_iter().next().map(|w| w.path))
            .or(Some(top))
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

    /// The branch new worktree branches are cut from: the repo's current
    /// branch, unless one is configured. A configured branch that does not
    /// exist is an error, not a silent fallback.
    pub fn resolve_base_branch(&self, dir: &Path, configured: &str) -> Result<String> {
        let configured = configured.trim();
        if configured.is_empty() {
            return self.current_branch(dir);
        }
        if self.branch_exists(dir, configured) {
            return Ok(configured.to_string());
        }
        Err(Error::Git(format!(
            "base branch '{configured}' not found in {}",
            dir.display()
        )))
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

    /// Remove a linked worktree of `repo`. `force` is required when it has
    /// changes or is locked. Removing something git no longer knows about is a no-op.
    pub fn worktree_remove(&self, repo: &Path, wt_path: &Path, force: bool) -> Result<()> {
        // git refuses to remove a worktree from inside itself, and running
        // from an unrelated repo misidentifies it — so run from the worktree's
        // main repo, discovered via its `.git` file. A missing `.git` file
        // only means the DIRECTORY is gone (deleted outside wingman): git
        // still lists the worktree as `prunable` and still holds its branch,
        // so fall back to the repository's own main repo and remove it there.
        let main = match main_repo_of_worktree(wt_path) {
            Some(m) => m,
            None => match self.main_repo(repo) {
                Some(m) => m,
                None => {
                    log::warn!(
                        "worktree {} gone and {} is not a repo, ignoring",
                        wt_path.display(),
                        repo.display()
                    );
                    return Ok(());
                }
            },
        };
        let mut args = vec!["worktree", "remove"];
        if force {
            // A locked worktree needs the flag twice.
            args.extend(["--force", "--force"]);
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

    /// The lock reason (possibly empty) when the worktree is locked
    /// (`git worktree lock`, or a tool that crashed mid-creation), else
    /// `None`. `git worktree remove` refuses a locked worktree without
    /// `--force --force`.
    pub fn worktree_lock(&self, wt_path: &Path) -> Option<String> {
        let out = Self::run_ok(wt_path, &["rev-parse", "--absolute-git-dir"]).ok()?;
        let reason = std::fs::read_to_string(Path::new(out.stdout.trim()).join("locked")).ok()?;
        Some(reason.trim().to_string())
    }

    /// Does the worktree have uncommitted changes? Drives the `--force`
    /// option on the delete confirm; the tree draws
    /// [`Git::worktree_state`] instead, which is about commits, not the
    /// working tree.
    pub fn worktree_dirty(&self, wt_path: &Path) -> Result<bool> {
        let out = Self::run_ok(wt_path, &["status", "--porcelain"])?;
        Ok(!out.stdout.trim().is_empty())
    }

    /// Does the worktree carry commits of its own that `base` does not have?
    ///
    /// The commit half of [`Git::worktree_state`], asked on its own: the
    /// delete confirm has to tell "clean tree, nothing committed" apart
    /// from "clean tree, work committed and not merged", and `Ahead`
    /// merges those with the dirty case. `fork` is the commit the worktree
    /// was cut from, as in [`Git::worktree_state`].
    pub fn worktree_committed_ahead(&self, wt_path: &Path, fork: &str, base: &str) -> Result<bool> {
        let range = format!("{fork}..HEAD");
        let out = Self::run_ok(wt_path, &["rev-list", "--count", &range])?;
        if out.stdout.trim().parse::<u64>().unwrap_or(0) == 0 {
            return Ok(false);
        }
        Ok(!Self::run_status(
            wt_path,
            &["merge-base", "--is-ancestor", "HEAD", base],
        ))
    }

    /// How much work the worktree is carrying, relative to `base`.
    ///
    /// `fork` is the commit the worktree was cut from (`Worktree::
    /// base_commit`); it is what separates "never committed anything" from
    /// "committed, and it all landed in the base" — measured against `base`
    /// alone those two are the same empty range. Callers with no recorded
    /// fork point pass `base` and lose the distinction in the merged
    /// direction only.
    ///
    /// An unclean working tree counts as work: it turns both `New` and
    /// `Merged` into [`WorktreeState::Ahead`] (see [`WorktreeState`]). The
    /// extra `git status` only runs when the commit verdict is one of
    /// those two — a worktree already ahead cannot be moved by it, so the
    /// common case pays nothing.
    ///
    /// A worktree whose directory is gone answers
    /// [`WorktreeState::Deleted`] before any git runs: every call in a
    /// path that does not exist fails, and the caller's fallback would
    /// draw the row as a fresh cut — the exact opposite of what happened
    /// to it. This is the normal end of an agent told to merge and clean
    /// up after itself, so it is a state, not an error.
    ///
    /// Known limit: a squash-merged branch keeps commits the base does not
    /// contain by object id, so it reads as [`WorktreeState::Ahead`], not
    /// `Merged`. Detecting that needs patch-id matching (`git cherry`),
    /// which is wrong as often as it is right on a squashed history —
    /// better to under-claim than to call a branch merged when it is not.
    pub fn worktree_state(&self, wt_path: &Path, fork: &str, base: &str) -> Result<WorktreeState> {
        if !wt_path.exists() {
            return Ok(WorktreeState::Deleted);
        }
        let range = format!("{fork}..HEAD");
        let out = Self::run_ok(wt_path, &["rev-list", "--count", &range])?;
        let committed = out.stdout.trim().parse::<u64>().unwrap_or(0) > 0;
        let by_commits = if !committed {
            WorktreeState::New
        } else if Self::run_status(wt_path, &["merge-base", "--is-ancestor", "HEAD", base]) {
            WorktreeState::Merged
        } else {
            WorktreeState::Ahead
        };
        if by_commits == WorktreeState::Ahead {
            return Ok(by_commits);
        }
        // A failed status check keeps the commit verdict: it is still a
        // real answer, and downgrading it to the default would be worse.
        if self.worktree_dirty(wt_path).unwrap_or(false) {
            return Ok(WorktreeState::Ahead);
        }
        Ok(by_commits)
    }

    /// Resolve a revision to its full object id.
    pub fn rev_parse(&self, dir: &Path, rev: &str) -> Result<String> {
        let out = Self::run_ok(dir, &["rev-parse", rev])?;
        Ok(out.stdout.trim().to_string())
    }

    /// Delete a local branch (`-D`, force — the branch is orphaned once its
    /// worktree is gone and the work is done).
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

    /// The three states a worktree row draws, on a real repo: untouched
    /// cut → New, work of its own → Ahead, merged into the base → Merged.
    /// "Work of its own" is commits *or* an unclean working tree — see
    /// [`WorktreeState`].
    #[test]
    fn worktree_state_tells_new_ahead_and_merged_apart() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt = repo.parent().unwrap().join("wt-state");
        // the fork point, exactly as `create_worktree` records it
        let fork = git.rev_parse(&repo, "main").unwrap();
        git.worktree_add(&repo, &wt, "state-branch", "main")
            .unwrap();

        // freshly cut: nothing of its own
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::New
        );

        // uncommitted work is work: an agent has touched it → Ahead
        fs::write(wt.join("scratch.txt"), "wip\n").unwrap();
        assert!(git.worktree_dirty(&wt).unwrap());
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Ahead
        );

        // its own commit, not in main yet
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-m", "work"]);
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Ahead
        );

        // merged into main → Merged, even though the branch still exists
        run(&repo, &["merge", "--no-ff", "-m", "merge", "state-branch"]);
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Merged
        );
        // ...and without the recorded fork point that is indistinguishable
        // from a fresh cut — the documented cost of a reconciled worktree.
        assert_eq!(
            git.worktree_state(&wt, "main", "main").unwrap(),
            WorktreeState::New
        );

        // new uncommitted work on top of a merged branch: there is
        // something here again, so it is no longer free to delete.
        fs::write(wt.join("after-merge.txt"), "wip\n").unwrap();
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Ahead
        );
        fs::remove_file(wt.join("after-merge.txt")).unwrap();

        // a commit after the merge puts it back ahead
        fs::write(wt.join("more.txt"), "more\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-m", "more work"]);
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Ahead
        );
    }

    /// The delete confirm's question — "is there committed work here?" —
    /// answered independently of the working tree: a clean tree on a
    /// branch that is ahead must still say yes, which is exactly the case
    /// [`WorktreeState::Ahead`] could not distinguish.
    #[test]
    fn committed_ahead_ignores_the_working_tree() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt = repo.parent().unwrap().join("wt-committed-ahead");
        let fork = git.rev_parse(&repo, "main").unwrap();
        git.worktree_add(&repo, &wt, "ahead-branch", "main")
            .unwrap();

        // freshly cut
        assert!(!git.worktree_committed_ahead(&wt, &fork, "main").unwrap());

        // uncommitted work only: nothing is committed yet
        fs::write(wt.join("scratch.txt"), "wip\n").unwrap();
        assert!(!git.worktree_committed_ahead(&wt, &fork, "main").unwrap());

        // committed, not merged — and the tree is clean again
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-m", "work"]);
        assert!(!git.worktree_dirty(&wt).unwrap());
        assert!(git.worktree_committed_ahead(&wt, &fork, "main").unwrap());

        // merged into the base: the commits are no longer its own
        run(&repo, &["merge", "--no-ff", "-m", "merge", "ahead-branch"]);
        assert!(!git.worktree_committed_ahead(&wt, &fork, "main").unwrap());
    }

    /// A worktree whose directory is gone — removed from the dashboard
    /// once its work has landed, or by a `git worktree remove` run by
    /// hand — reads as [`WorktreeState::Deleted`]. Every
    /// git call in there fails, and answering "untouched" for a question
    /// that could not be asked draws the row as a fresh cut.
    #[test]
    fn worktree_state_reads_a_removed_directory_as_deleted() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt = repo.parent().unwrap().join("wt-gone");
        let fork = git.rev_parse(&repo, "main").unwrap();
        git.worktree_add(&repo, &wt, "gone-branch", "main").unwrap();
        fs::write(wt.join("f.txt"), "work\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-m", "work"]);
        run(&repo, &["merge", "--no-ff", "-m", "merge", "gone-branch"]);
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Merged
        );

        // the agent cleans up after itself: the directory goes away
        run(&repo, &["worktree", "remove", wt.to_str().unwrap()]);
        assert!(!wt.exists());
        assert_eq!(
            git.worktree_state(&wt, &fork, "main").unwrap(),
            WorktreeState::Deleted
        );
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("My Worktree!"), "my-worktree");
        assert_eq!(slugify("  x  "), "x");
        assert_eq!(slugify(""), "worktree");
        assert_eq!(slugify("a_b.c-d"), "a_b.c-d");
        assert_eq!(slugify("branch/name"), "branch-name");
        assert_eq!(slugify("café"), "caf");
        assert_eq!(slugify("---"), "worktree");
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
    fn repo_root_and_main_repo_resolve_from_subdirs_and_worktrees() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;

        // from a subdirectory of the repo
        fs::create_dir_all(repo.join("sub/dir")).unwrap();
        assert_eq!(git.repo_root(&repo.join("sub/dir")).unwrap(), repo);
        assert_eq!(git.main_repo(&repo.join("sub/dir")).unwrap(), repo);

        // not a repo
        assert!(git.repo_root(repo.parent().unwrap()).is_none());
        assert!(git.main_repo(repo.parent().unwrap()).is_none());

        // from a linked worktree → the MAIN repo is returned
        let wt = repo.parent().unwrap().join("wt-main-repo");
        git.worktree_add(&repo, &wt, "worktree-wt", "main").unwrap();
        let nested = wt.join("src");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(git.repo_root(&nested).unwrap(), wt);
        assert_eq!(git.main_repo(&nested).unwrap(), repo);
        git.worktree_remove(&repo, &wt, false).unwrap();
    }

    #[test]
    fn current_branch_and_exists() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        assert_eq!(git.current_branch(&repo).unwrap(), "main");
        assert!(git.branch_exists(&repo, "main"));
        assert!(!git.branch_exists(&repo, "nope"));
        assert_eq!(git.resolve_base_branch(&repo, "main").unwrap(), "main");
        assert_eq!(git.resolve_base_branch(&repo, "").unwrap(), "main");
        run(&repo, &["checkout", "-q", "-b", "develop"]);
        assert_eq!(git.resolve_base_branch(&repo, "").unwrap(), "develop");
        assert!(git.resolve_base_branch(&repo, "master").is_err());
    }

    #[test]
    fn a_locked_worktree_reports_its_reason_and_needs_force() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt_path = repo.parent().unwrap().join("wt-locked");
        git.worktree_add(&repo, &wt_path, "locked", "main").unwrap();
        assert_eq!(git.worktree_lock(&wt_path), None);

        Git::run_ok(
            &repo,
            &[
                "worktree",
                "lock",
                "--reason",
                "mid-creation",
                wt_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(git.worktree_lock(&wt_path).as_deref(), Some("mid-creation"));

        assert!(git.worktree_remove(&repo, &wt_path, false).is_err());
        git.worktree_remove(&repo, &wt_path, true).unwrap();
        assert!(!wt_path.exists());
    }

    #[test]
    fn worktree_add_list_remove_roundtrip() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt_path = repo.parent().unwrap().join("wt-worktree-x");

        // initial list has just the main worktree
        let list = git.worktree_list(&repo).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, repo);

        git.worktree_add(&repo, &wt_path, "worktree-x", "main")
            .unwrap();
        assert!(wt_path.exists());
        assert!(git.branch_exists(&repo, "worktree-x"));

        let list = git.worktree_list(&repo).unwrap();
        assert_eq!(list.len(), 2);
        let wt = list.iter().find(|w| w.path == wt_path).unwrap();
        assert_eq!(wt.branch_short(), Some("worktree-x"));
        assert!(wt.head.is_some());

        // dirty check: clean worktree
        assert!(!git.worktree_dirty(&wt_path).unwrap());
        fs::write(wt_path.join("change.txt"), "dirty").unwrap();
        assert!(git.worktree_dirty(&wt_path).unwrap());

        // remove without force on a dirty worktree fails
        assert!(git.worktree_remove(&repo, &wt_path, false).is_err());
        git.worktree_remove(&repo, &wt_path, true).unwrap();
        assert!(!wt_path.exists());
        assert_eq!(git.worktree_list(&repo).unwrap().len(), 1);

        // removing again is a no-op success
        git.worktree_remove(&repo, &wt_path, false).unwrap();
    }

    /// A worktree whose DIRECTORY was deleted outside wingman stays
    /// registered in git (`prunable`) and keeps holding its branch.
    /// Removing the worktree has to clear that registration too — otherwise
    /// `git worktree list` keeps reporting it and the worktree reappears on
    /// the next start.
    #[test]
    fn worktree_remove_clears_a_prunable_registration() {
        let (_guard, repo) = temp_repo("proj");
        let git = Git;
        let wt_path = repo.parent().unwrap().join("wt-vanished");
        git.worktree_add(&repo, &wt_path, "vanished", "main")
            .unwrap();
        fs::remove_dir_all(&wt_path).unwrap();
        assert_eq!(
            git.worktree_list(&repo).unwrap().len(),
            2,
            "the directory is gone but git still lists it"
        );

        git.worktree_remove(&repo, &wt_path, false).unwrap();

        assert_eq!(git.worktree_list(&repo).unwrap().len(), 1);
        // with the registration gone the branch is free to delete
        git.branch_delete(&repo, "vanished").unwrap();
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
