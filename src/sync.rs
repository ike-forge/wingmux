//! Upstream fetches off the event loop.
//!
//! One thread per fetch, at most `max` running; the rest wait in a FIFO.
//! [`Fetcher::poll`] collects finished fetches and starts queued ones, so
//! the cap is enforced from the event loop and needs no pool.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::git::Git;

/// Long enough for a slow remote, short enough that a black-holed one
/// (corporate proxy, VPN down) gives its slot back quickly.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Done {
    pub repo: PathBuf,
    pub result: Result<()>,
}

pub struct Fetcher {
    queued: VecDeque<PathBuf>,
    running: Vec<PathBuf>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl Default for Fetcher {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Fetcher {
            queued: VecDeque::new(),
            running: Vec::new(),
            tx,
            rx,
        }
    }
}

impl Fetcher {
    /// Queue a fetch; a repository already queued or running is not queued
    /// twice.
    pub fn submit(&mut self, repo: &Path) {
        if !self.is_fetching(repo) {
            self.queued.push_back(repo.to_path_buf());
        }
    }

    /// Queued or running.
    pub fn is_fetching(&self, repo: &Path) -> bool {
        self.running.iter().any(|p| p == repo) || self.queued.iter().any(|p| p == repo)
    }

    /// Finished fetches, without blocking; then start queued ones up to
    /// `max` running (0 reads as 1).
    pub fn poll(&mut self, max: usize) -> Vec<Done> {
        let done: Vec<Done> = self.rx.try_iter().collect();
        for d in &done {
            self.running.retain(|p| *p != d.repo);
        }
        while self.running.len() < max.max(1)
            && let Some(repo) = self.queued.pop_front()
        {
            self.running.push(repo.clone());
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                // A panic would leave the repository "running" for good.
                let result = std::panic::catch_unwind(|| Git.fetch_upstream(&repo, FETCH_TIMEOUT))
                    .unwrap_or_else(|_| Err(Error::Git("fetch thread panicked".into())));
                let _ = tx.send(Done { repo, result });
            });
        }
        done
    }
}

/// Slower than a fetch: a push may run pre-push hooks, a pull a merge.
const PUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// User-initiated pushes or pulls, one thread each, never queued behind
/// fetches.
pub struct Pusher {
    run: fn(&Path) -> Result<()>,
    verb: &'static str,
    running: Vec<PathBuf>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl Pusher {
    pub fn push() -> Self {
        Self::new("push", |d| Git.push_upstream(d, PUSH_TIMEOUT))
    }

    pub fn pull() -> Self {
        Self::new("pull", |d| Git.pull_upstream(d, PUSH_TIMEOUT))
    }

    /// Runs `run` instead of git (tests only).
    #[cfg(test)]
    pub fn fake(verb: &'static str, run: fn(&Path) -> Result<()>) -> Self {
        Self::new(verb, run)
    }

    fn new(verb: &'static str, run: fn(&Path) -> Result<()>) -> Self {
        let (tx, rx) = mpsc::channel();
        Pusher {
            run,
            verb,
            running: Vec::new(),
            tx,
            rx,
        }
    }

    /// Start the operation; false when one is already running for the
    /// repository.
    pub fn submit(&mut self, repo: &Path) -> bool {
        if self.is_pushing(repo) {
            return false;
        }
        self.running.push(repo.to_path_buf());
        let (repo, tx, run, verb) = (repo.to_path_buf(), self.tx.clone(), self.run, self.verb);
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| run(&repo))
                .unwrap_or_else(|_| Err(Error::Git(format!("{verb} thread panicked"))));
            let _ = tx.send(Done { repo, result });
        });
        true
    }

    pub fn is_pushing(&self, repo: &Path) -> bool {
        self.running.iter().any(|p| p == repo)
    }

    /// Finished pushes, without blocking.
    pub fn poll(&mut self) -> Vec<Done> {
        let done: Vec<Done> = self.rx.try_iter().collect();
        for d in &done {
            self.running.retain(|p| *p != d.repo);
        }
        done
    }
}

/// Pulls or pushes started together, reported together: one toast for a
/// selection of ten, not ten toasts replacing each other.
#[derive(Debug, Default)]
pub struct Batch {
    pub pending: std::collections::HashSet<PathBuf>,
    pub ok: usize,
    /// Names, in the order they came back.
    pub failed: Vec<String>,
}

impl Batch {
    pub fn record(&mut self, repo: &Path, name: String, ok: bool) {
        self.pending.remove(repo);
        if ok {
            self.ok += 1;
        } else {
            self.failed.push(name);
        }
    }

    /// `pushed 3 · failed 1 (repo-e)`; the errors are in the log.
    pub fn summary(&self, done: &str) -> String {
        let mut s = format!("{done} {}", self.ok);
        if !self.failed.is_empty() {
            s += &format!(
                " · failed {} ({}) — see the log",
                self.failed.len(),
                self.failed.join(", ")
            );
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn drain(f: &mut Fetcher, max: usize, n: usize) -> Vec<Done> {
        let start = Instant::now();
        let mut out = Vec::new();
        while out.len() < n {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "fetches never finished"
            );
            out.extend(f.poll(max));
            std::thread::sleep(Duration::from_millis(10));
        }
        out
    }

    #[test]
    fn a_batch_summary_names_only_the_failures() {
        let mut b = Batch::default();
        for r in ["a", "b", "c"] {
            b.pending.insert(r.into());
        }
        b.record(Path::new("a"), "a".into(), true);
        b.record(Path::new("b"), "b".into(), false);
        assert!(!b.pending.is_empty());
        b.record(Path::new("c"), "c".into(), true);
        assert!(b.pending.is_empty());
        assert_eq!(b.summary("pushed"), "pushed 2 · failed 1 (b) — see the log");
        assert_eq!(Batch::default().summary("pulled"), "pulled 0");
    }

    #[test]
    fn caps_running_dedupes_and_reports_each_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let repos: Vec<PathBuf> = (0..3).map(|i| dir.path().join(format!("r{i}"))).collect();
        let mut f = Fetcher::default();
        for r in &repos {
            f.submit(r);
            f.submit(r);
        }
        assert_eq!(f.queued.len(), 3);
        f.poll(2);
        assert_eq!(f.running.len(), 2);
        assert!(repos.iter().all(|r| f.is_fetching(r)));
        let mut done: Vec<PathBuf> = drain(&mut f, 2, 3).into_iter().map(|d| d.repo).collect();
        done.sort();
        assert_eq!(done, repos);
        assert!(repos.iter().all(|r| !f.is_fetching(r)));
    }
}
