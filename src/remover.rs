//! Worktree removal off the event loop.
//!
//! `git worktree remove` deletes the whole checkout and takes seconds on a
//! big tree. One worker thread runs the jobs in submission order, so two
//! removals never contend for the same repository's ref locks.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::error::{Error, Result};
use crate::git::Git;

pub struct Job {
    pub repo: PathBuf,
    pub path: PathBuf,
    pub name: String,
    pub branch: String,
    pub force: bool,
    pub delete_branch: bool,
}

pub struct Done {
    pub path: PathBuf,
    pub name: String,
    pub result: Result<()>,
}

struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

#[derive(Default)]
pub struct Remover {
    worker: Option<Worker>,
    /// Submitted and not yet handed back by [`Remover::poll`]; name kept for
    /// the error a dead worker owes.
    in_flight: Vec<(PathBuf, String)>,
}

impl Remover {
    pub fn submit(&mut self, job: Job) {
        self.in_flight.push((job.path.clone(), job.name.clone()));
        let worker = self.worker.get_or_insert_with(spawn);
        // A send only fails once the worker has died; poll reports it.
        let _ = worker.jobs.send(job);
    }

    pub fn is_removing(&self, path: &Path) -> bool {
        self.in_flight.iter().any(|(p, _)| p == path)
    }

    pub fn pending(&self) -> usize {
        self.in_flight.len()
    }

    /// Finished jobs, without blocking.
    pub fn poll(&mut self) -> Vec<Done> {
        self.collect(false)
    }

    /// Block until every submitted job has finished.
    pub fn wait(&mut self) -> Vec<Done> {
        self.collect(true)
    }

    fn collect(&mut self, block: bool) -> Vec<Done> {
        let mut out = Vec::new();
        while !self.in_flight.is_empty() {
            let Some(w) = &self.worker else { break };
            let next = if block {
                w.done.recv().map_err(|_| TryRecvError::Disconnected)
            } else {
                w.done.try_recv()
            };
            match next {
                Ok(d) => {
                    self.in_flight.retain(|(p, _)| *p != d.path);
                    out.push(d);
                }
                Err(TryRecvError::Empty) => break,
                // The worker panicked: fail what it still owed, respawn on
                // the next submit.
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    out.extend(self.in_flight.drain(..).map(|(path, name)| Done {
                        path,
                        name,
                        result: Err(Error::Git("worktree removal thread died".into())),
                    }));
                }
            }
        }
        out
    }
}

fn spawn() -> Worker {
    let (jobs, rx) = mpsc::channel::<Job>();
    let (tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        for job in rx {
            let result = run(&job);
            let d = Done {
                path: job.path,
                name: job.name,
                result,
            };
            if tx.send(d).is_err() {
                break;
            }
        }
    });
    Worker { jobs, done }
}

fn run(job: &Job) -> Result<()> {
    Git.worktree_remove(&job.repo, &job.path, job.force)?;
    if job.delete_branch {
        match Git.branch_delete(&job.repo, &job.branch) {
            Ok(()) => log::info!("deleted branch '{}'", job.branch),
            Err(e) => log::warn!("branch delete: {e}"),
        }
    }
    Ok(())
}
