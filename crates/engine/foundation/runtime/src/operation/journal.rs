//! Journal of finished steps, so an Operation interrupted by a crash can resume.
//!
//! The journal sits next to the staging directory at
//! `data_root/.staging/<plan-id>.journal` and holds one line per event:
//! `done <step index>` after a step finishes and `rolled-back` after a
//! failed or cancelled run removed the staging directory. Only steps
//! finished after the last `rolled-back` line count as done.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bytes::Bytes;

use crate::{FileWriter, PlanId, StepId, Storage, StorageError};

/// Directory holding staging directories, saved Plans and journals.
pub(super) fn staging_dir(data_root: &Path) -> PathBuf {
    data_root.join(".staging")
}

pub(super) fn plan_file(data_root: &Path, plan: PlanId) -> PathBuf {
    staging_dir(data_root).join(format!("{plan}.plan.json"))
}

pub(super) fn journal_file(data_root: &Path, plan: PlanId) -> PathBuf {
    staging_dir(data_root).join(format!("{plan}.journal"))
}

/// Parses journal text into the set of finished steps.
pub(super) fn parse(text: &str) -> HashSet<StepId> {
    let mut done = HashSet::new();
    for line in text.lines() {
        if line == "rolled-back" {
            done.clear();
        } else if let Some(index) = line.strip_prefix("done ")
            && let Ok(index) = index.parse::<u32>()
        {
            done.insert(StepId(index));
        }
    }
    done
}

/// Reads the finished steps recorded for a Plan; a missing journal means none.
pub(super) async fn read(
    storage: &dyn Storage,
    path: &Path,
) -> Result<HashSet<StepId>, StorageError> {
    match storage.read(path).await {
        Ok(bytes) => Ok(parse(&String::from_utf8_lossy(&bytes))),
        Err(error) if error.is_not_found() => Ok(HashSet::new()),
        Err(error) => Err(error),
    }
}

/// Append-only writer for one journal.
pub(super) struct Journal {
    writer: Box<dyn FileWriter>,
}

impl Journal {
    /// Opens the journal for appending, creating it when missing.
    pub(super) async fn open(storage: &dyn Storage, path: &Path) -> Result<Self, StorageError> {
        let writer = match storage.metadata(path).await? {
            Some(_) => storage.append(path).await?,
            None => storage.create(path).await?,
        };
        Ok(Self { writer })
    }

    pub(super) async fn step_done(&mut self, step: StepId) -> Result<(), StorageError> {
        self.writer
            .write(Bytes::from(format!("done {}\n", step.0)))
            .await
    }

    pub(super) async fn rolled_back(mut self) -> Result<(), StorageError> {
        self.writer
            .write(Bytes::from_static(b"rolled-back\n"))
            .await?;
        self.writer.finish().await
    }

    pub(super) async fn close(self) -> Result<(), StorageError> {
        self.writer.finish().await
    }
}

#[cfg(test)]
#[path = "../../tests/operation/journal.rs"]
mod tests;
