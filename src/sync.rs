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
/// gives its slot back.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

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
