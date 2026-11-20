//! Shared PR management, refresh, and lead authorization commands for the CLI and UI.

use crate::{review_commands, storage};
use luthiai::{
    AuthorizationScope, HeadSha, InputRevision, PrKey, RecordOutcome, RefreshOutcome, StateError,
    github::{GithubClient, GithubError, PullRequest, discussion::PullObservation},
    review::RoundId,
};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::{fs, io};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecordError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("resolving the current PR head for {key}: {source}")]
    Github {
        key: PrKey,
        #[source]
        source: GithubError,
    },
    #[error("local state: {0}")]
    Storage(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
pub enum RefreshError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error("local state: {0}")]
    Storage(#[source] Box<dyn Error + Send + Sync>),
}

#[derive(Debug, Error)]
pub enum AuthorizationError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("local state: {0}")]
    Storage(#[source] Box<dyn Error + Send + Sync>),
}

impl From<anyhow::Error> for AuthorizationError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error.into())
    }
}

#[derive(Debug, Error)]
pub enum UnmanageError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("local state: {0}")]
    Storage(#[from] anyhow::Error),
    #[error("cannot remove round {round} while its runner lock is unavailable: {source}")]
    RunnerLock {
        round: RoundId,
        #[source]
        source: anyhow::Error,
    },
    #[error("could not remove saved files for round {round}; retry unmanage: {source}")]
    Artifacts {
        round: RoundId,
        #[source]
        source: io::Error,
    },
}

/// Resolve an omitted head before mutating state, without granting review permission.
pub fn record_request(
    state_file: &Path,
    key: &PrKey,
    head_sha: Option<HeadSha>,
    fetch_head: impl FnOnce(&PrKey) -> Result<HeadSha, GithubError>,
) -> Result<(HeadSha, RecordOutcome), RecordError> {
    let _pr_lock = storage::pr_lock(state_file, key)?;
    let head_sha = match head_sha {
        Some(head_sha) => head_sha,
        None => fetch_head(key).map_err(|source| RecordError::Github {
            key: key.clone(),
            source,
        })?,
    };
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let outcome = state.record_request(key.clone(), head_sha.clone())?;
    if outcome != RecordOutcome::Unchanged {
        storage::save(state_file, &state)?;
    }
    Ok((head_sha, outcome))
}

/// Authorize only the inputs inspected by the lead, under the shared state lock.
pub fn authorize(
    state_file: &Path,
    key: &PrKey,
    head: &HeadSha,
    revision: InputRevision,
    scope: AuthorizationScope,
) -> Result<bool, AuthorizationError> {
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let changed = state.authorize(key, head, revision, scope)?;
    if changed {
        storage::save(state_file, &state)?;
    }
    Ok(changed)
}

/// Revoke authorization without requiring the lead to match the current inputs.
pub fn revoke(state_file: &Path, key: &PrKey) -> Result<bool, AuthorizationError> {
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let changed = state.revoke(key)?;
    if changed {
        storage::save(state_file, &state)?;
    }
    Ok(changed)
}

/// Delete a PR's local records and files after all work and recovery have finished.
pub fn unmanage(state_file: &Path, key: &PrKey) -> Result<bool, UnmanageError> {
    let _pr_lock = storage::pr_lock(state_file, key)?;
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let removed = state.unmanage(key)?;
    let files = removed
        .iter()
        .map(|id| check_round_files(state_file, id).map(|path| (id, path)))
        .collect::<Result<Vec<_>, _>>()?;
    // Check every runner before deleting any files.
    // Delete files before forgetting their IDs so a failed removal can be retried.
    // A partial filesystem failure leaves the recorded request available for retry.
    for (id, path) in files {
        if let Some(path) = path {
            fs::remove_dir_all(&path).map_err(|source| UnmanageError::Artifacts {
                round: id.clone(),
                source,
            })?;
        }
    }
    storage::save(state_file, &state)?;
    Ok(true)
}

fn check_round_files(state_file: &Path, id: &RoundId) -> Result<Option<PathBuf>, UnmanageError> {
    let path = review_commands::round_path(state_file, id);
    let artifacts = |source| UnmanageError::Artifacts {
        round: id.clone(),
        source,
    };
    let root = path.parent().ok_or_else(|| {
        artifacts(io::Error::new(
            io::ErrorKind::InvalidInput,
            "round directory has no parent",
        ))
    })?;
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(artifacts(error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(artifacts(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rounds directory must be a regular directory",
            )));
        }
        Ok(_) => {}
    }
    let metadata = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(artifacts(error)),
        Ok(metadata) => metadata,
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let runner = storage::try_recovery_lock(&path.join("runner")).map_err(|source| {
            UnmanageError::RunnerLock {
                round: id.clone(),
                source,
            }
        })?;
        // The PR lock prevents another runner from starting; release this handle
        // before deleting its directory so Windows can remove the lock file too.
        drop(runner);
    }
    Ok(Some(path))
}

fn storage_error(error: anyhow::Error) -> RefreshError {
    RefreshError::Storage(error.into())
}

/// Refresh one existing request using the same process locks for every caller.
pub fn refresh(state_file: &Path, key: &PrKey) -> Result<RefreshOutcome, RefreshError> {
    Ok(refresh_with_details(state_file, key)?.outcome())
}

/// Display context from the same observation that refreshed the recorded inputs.
pub struct RefreshedRequest {
    observation: PullObservation,
    revision: InputRevision,
    outcome: RefreshOutcome,
}

impl RefreshedRequest {
    pub fn pull(&self) -> &PullRequest {
        self.observation.pull()
    }
    pub fn revision(&self) -> InputRevision {
        self.revision
    }
    pub fn outcome(&self) -> RefreshOutcome {
        self.outcome
    }
}

pub fn refresh_with_details(
    state_file: &Path,
    key: &PrKey,
) -> Result<RefreshedRequest, RefreshError> {
    let _pr_lock = storage::pr_lock(state_file, key).map_err(storage_error)?;
    let state = storage::load(state_file).map_err(storage_error)?;
    let prior_revision = state
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .revision();
    let client = GithubClient::new()?;
    let observation = client.observe_pull_excluding(key, &state.confirmed_review_ids(key))?;
    let _lock = storage::lock(state_file).map_err(storage_error)?;
    let mut state = storage::load(state_file).map_err(storage_error)?;
    let current_revision = state
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .revision();
    if current_revision != prior_revision {
        return Err(StateError::RevisionMismatch {
            inspected: prior_revision,
            recorded: current_revision,
        }
        .into());
    }
    let outcome = state.refresh(
        observation.key(),
        observation.head_sha(),
        observation.discussion(),
    )?;
    if outcome != RefreshOutcome::Unchanged {
        storage::save(state_file, &state).map_err(storage_error)?;
    }
    Ok(RefreshedRequest {
        observation,
        revision: state
            .get(key)
            .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
            .revision(),
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use luthiai::{Authorization, ObservedDiscussion, State};
    use std::num::NonZeroU64;

    fn key() -> PrKey {
        PrKey::new("owner/repo", 42).unwrap()
    }

    fn head(digit: char) -> HeadSha {
        HeadSha::new(digit.to_string().repeat(40)).unwrap()
    }

    #[test]
    fn omitted_head_records_the_resolved_sha_without_authorizing_or_starting_a_review() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let (recorded, outcome) = record_request(&state_file, &key(), None, |requested| {
            assert_eq!(requested, &key());
            assert!(!state_file.exists());
            Ok(head('a'))
        })
        .unwrap();

        assert_eq!(recorded, head('a'));
        assert_eq!(outcome, RecordOutcome::New);
        let state = storage::load(&state_file).unwrap();
        let request = state.get(&key()).unwrap();
        assert_eq!(request.head_sha(), &recorded);
        assert_eq!(request.revision().get(), 1);
        assert_eq!(request.authorization(), Authorization::AwaitingLead);
        assert!(request.discussion().is_none());
        assert!(request.model_validation().is_none());
        assert!(state.rounds().is_empty());
    }

    #[test]
    fn explicit_head_does_not_invoke_the_lookup() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let (recorded, outcome) = record_request(&state_file, &key(), Some(head('b')), |_| {
            panic!("an explicit head must not access GitHub")
        })
        .unwrap();
        assert_eq!(recorded, head('b'));
        assert_eq!(outcome, RecordOutcome::New);
        assert_eq!(
            storage::load(&state_file)
                .unwrap()
                .get(&key())
                .unwrap()
                .head_sha(),
            &recorded
        );
    }

    #[test]
    fn failed_head_lookups_leave_new_and_existing_state_unchanged() {
        for existing in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let state_file = directory.path().join("state.json");
            if existing {
                let mut state = State::default();
                state.record_request(key(), head('a')).unwrap();
                storage::save(&state_file, &state).unwrap();
            }
            let before = fs::read(&state_file).ok();
            let invalid_sha = serde_json::from_value::<HeadSha>("invalid".into()).unwrap_err();
            for source in [
                GithubError::AuthenticationRequired {
                    resource: "pull request owner/repo#42".into(),
                },
                GithubError::InvalidCredentials,
                GithubError::NotFound {
                    resource: "pull request owner/repo#42".into(),
                },
                GithubError::PermissionDenied {
                    resource: "pull request owner/repo#42".into(),
                },
                GithubError::Transport {
                    resource: "pull request owner/repo#42".into(),
                    source: Box::new(io::Error::new(io::ErrorKind::TimedOut, "timed out")),
                },
                GithubError::DecodeResponse {
                    resource: "pull request owner/repo#42".into(),
                    source: invalid_sha,
                },
            ] {
                let expected = std::mem::discriminant(&source);
                let error = record_request(&state_file, &key(), None, |_| Err(source)).unwrap_err();
                let RecordError::Github {
                    key: failed,
                    source,
                } = error
                else {
                    panic!("expected a GitHub lookup error");
                };
                assert_eq!(failed, key());
                assert_eq!(std::mem::discriminant(&source), expected);
                assert_eq!(fs::read(&state_file).ok(), before);
            }
        }
    }

    #[test]
    fn existing_requests_keep_the_same_transition_rules_for_both_head_sources() {
        for authorization in [
            Authorization::AwaitingLead,
            Authorization::OneRound,
            Authorization::PrCycle,
        ] {
            for explicit in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let state_file = directory.path().join("state.json");
                let mut state = State::default();
                state.record_request(key(), head('a')).unwrap();
                state
                    .refresh(
                        &key(),
                        &head('a'),
                        &ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
                    )
                    .unwrap();
                let revision = InputRevision::new(2).unwrap();
                let scope = match authorization {
                    Authorization::AwaitingLead => None,
                    Authorization::OneRound => Some(AuthorizationScope::OneRound),
                    Authorization::PrCycle => Some(AuthorizationScope::PrCycle),
                };
                if let Some(scope) = scope {
                    state
                        .authorize(&key(), &head('a'), revision, scope)
                        .unwrap();
                    let mut saved = serde_json::to_value(&state).unwrap();
                    saved["requests"][0]["model_validation"] = serde_json::json!({
                        "model": "model-alpha",
                        "agent": "claude",
                        "head_sha": head('a'),
                        "revision": revision,
                        "prompt_sha256": "a".repeat(64),
                        "format_sha256": "b".repeat(64),
                        "execution_sha256": "c".repeat(64),
                        "runner_sha256": "d".repeat(64),
                        "vm_archive": {
                            "path": directory.path().join("reviewer.tar"),
                            "sha256": "e".repeat(64),
                            "config_sha256": "f".repeat(64)
                        }
                    });
                    state = serde_json::from_value(saved).unwrap();
                }
                storage::save(&state_file, &state).unwrap();
                let before = fs::read(&state_file).unwrap();
                let (_, outcome) =
                    record_request(&state_file, &key(), explicit.then(|| head('a')), |_| {
                        Ok(head('a'))
                    })
                    .unwrap();
                assert_eq!(outcome, RecordOutcome::Unchanged);
                assert_eq!(fs::read(&state_file).unwrap(), before);

                let (recorded, outcome) =
                    record_request(&state_file, &key(), explicit.then(|| head('b')), |_| {
                        Ok(head('b'))
                    })
                    .unwrap();
                let reset = authorization == Authorization::OneRound;
                assert_eq!(recorded, head('b'));
                assert_eq!(
                    outcome,
                    if reset {
                        RecordOutcome::HeadChangedAuthorizationReset
                    } else {
                        RecordOutcome::HeadChangedAuthorizationPreserved
                    }
                );
                let state = storage::load(&state_file).unwrap();
                let request = state.get(&key()).unwrap();
                assert_eq!(request.head_sha(), &recorded);
                assert_eq!(request.revision().get(), 3);
                assert_eq!(
                    request.authorization(),
                    if reset {
                        Authorization::AwaitingLead
                    } else {
                        authorization
                    }
                );
                assert!(request.discussion().is_none());
                assert!(request.model_validation().is_none());
                assert!(state.rounds().is_empty());
            }
        }
    }
}
