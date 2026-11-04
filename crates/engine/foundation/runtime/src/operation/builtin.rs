//! Steps the executor runs itself: everything that only needs [`Storage`].

use std::path::Path;

use super::context::Shared;
use super::{StepContext, StepError};
use crate::{
    ComputedHashes, EntryKind, Expectation, MetadataStep, MoveStep, Precondition, RemoveMode,
    RemoveStep, StepOutcome, Storage, StorageError, StreamHasher, VerifyHashStep, WriteFileStep,
};

/// Hashes a file chunk by chunk with the algorithms `expect` names.
///
/// # Errors
///
/// Returns the [`StorageError`] of opening or reading the file.
pub(super) async fn hash_file(
    storage: &dyn Storage,
    path: &Path,
    expect: &Expectation,
) -> Result<ComputedHashes, StorageError> {
    let mut hasher = StreamHasher::for_expectation(expect);
    let mut reader = storage.open(path).await?;
    while let Some(chunk) = reader.next_chunk().await? {
        hasher.update(&chunk);
    }
    Ok(hasher.finish())
}

/// Checks every precondition, returning the first stale path.
pub(super) async fn check_preconditions(
    shared: &Shared,
    preconditions: &[Precondition],
) -> Result<(), StepError> {
    for precondition in preconditions {
        match precondition {
            Precondition::FileUnchanged { path, hash } => {
                let resolved = shared.resolve(path)?;
                let exists = shared.storage.metadata(&resolved).await?.is_some();
                let unchanged = match hash {
                    None => !exists,
                    Some(hash) if exists => {
                        let expect = Expectation::hash(hash.clone(), None);
                        hash_file(&*shared.storage, &resolved, &expect)
                            .await?
                            .check(&expect)
                            .is_ok()
                    }
                    Some(_) => false,
                };
                if !unchanged {
                    return Err(StepError::PlanStale { path: path.clone() });
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn write_file(
    cx: &StepContext,
    step: &WriteFileStep,
) -> Result<StepOutcome, StepError> {
    let path = cx.resolve(&step.path)?;
    cx.storage()
        .write_atomic(&path, step.contents.clone().into())
        .await?;
    Ok(StepOutcome::Succeeded)
}

pub(super) async fn write_metadata(
    cx: &StepContext,
    step: &MetadataStep,
    preconditions: &[Precondition],
) -> Result<StepOutcome, StepError> {
    // Another launcher may have written the file while earlier steps ran.
    check_preconditions(&cx.shared, preconditions).await?;
    let path = cx.resolve(&step.path)?;
    cx.storage()
        .write_atomic(&path, step.contents.clone().into())
        .await?;
    Ok(StepOutcome::Succeeded)
}

pub(super) async fn move_into_place(
    cx: &StepContext,
    step: &MoveStep,
) -> Result<StepOutcome, StepError> {
    let from = cx.resolve(&step.from)?;
    let to = cx.resolve(&step.to)?;
    let storage = cx.storage();
    // rename() replaces files but not directories on every platform.
    if let Some(existing) = storage.metadata(&to).await?
        && existing.kind == EntryKind::Directory
    {
        storage.remove_dir_all(&to).await?;
    }
    storage.rename(&from, &to).await?;
    Ok(StepOutcome::Succeeded)
}

pub(super) async fn remove(cx: &StepContext, step: &RemoveStep) -> Result<StepOutcome, StepError> {
    let path = cx.resolve(&step.path)?;
    let storage = cx.storage();
    let Some(existing) = storage.metadata(&path).await? else {
        return Ok(StepOutcome::Skipped);
    };
    match step.mode {
        RemoveMode::Trash => {
            let trash = step
                .path
                .path
                .under(&cx.data_root().join("trash").join(cx.plan().to_string()));
            storage.rename(&path, &trash).await?;
        }
        RemoveMode::Delete if existing.kind == EntryKind::Directory => {
            storage.remove_dir_all(&path).await?;
        }
        RemoveMode::Delete => storage.remove_file(&path).await?,
    }
    Ok(StepOutcome::Succeeded)
}

pub(super) async fn verify_hash(
    cx: &StepContext,
    step: &VerifyHashStep,
) -> Result<StepOutcome, StepError> {
    let path = cx.resolve(&step.path)?;
    hash_file(cx.storage(), &path, &step.expect)
        .await?
        .check(&step.expect)
        .map_err(|source| StepError::Verify {
            path: step.path.clone(),
            source,
        })?;
    Ok(StepOutcome::Succeeded)
}
