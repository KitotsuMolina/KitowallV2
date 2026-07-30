use std::cmp::Reverse;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{JsonStore, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Refresh,
    Hydrate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    CancelRequested,
    Canceled,
    Completed,
    Failed,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Canceled | Self::Completed | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub pack: String,
    pub completed: usize,
    pub total: usize,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JobStore {
    directory: PathBuf,
}

impl JobStore {
    pub fn from_environment() -> Result<Self, StoreError> {
        let base = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .ok_or(StoreError::HomeUnavailable)?;
        Ok(Self::new(base.join("kitowall/jobs")))
    }

    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn create(
        &self,
        kind: JobKind,
        pack: impl Into<String>,
        total: usize,
    ) -> Result<JobRecord, StoreError> {
        let now = current_time_ms()?;
        let record = JobRecord {
            id: format!("{now}-{}-{}", std::process::id(), current_time_ns()?),
            kind,
            status: JobStatus::Queued,
            pack: pack.into(),
            completed: 0,
            total,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            error: None,
        };
        self.save(&record)?;
        Ok(record)
    }

    pub fn load(&self, id: &str) -> Result<JobRecord, StoreError> {
        let path = self.path(id)?;
        if !path.is_file() {
            return Err(StoreError::InvalidData(format!("job not found: {id}")));
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    pub fn save(&self, record: &JobRecord) -> Result<(), StoreError> {
        JsonStore::new(self.path(&record.id)?).save(record)
    }

    pub fn list(&self) -> Result<Vec<JobRecord>, StoreError> {
        if !self.directory.exists() {
            return Ok(Vec::new());
        }
        let mut records = fs::read_dir(&self.directory)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "json")
            })
            .filter_map(|entry| {
                serde_json::from_slice::<JobRecord>(&fs::read(entry.path()).ok()?).ok()
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| Reverse(record.created_at_unix_ms));
        Ok(records)
    }

    pub fn request_cancel(&self, id: &str) -> Result<JobRecord, StoreError> {
        let mut record = self.load(id)?;
        if !record.status.is_terminal() {
            record.status = if record.status == JobStatus::Queued {
                JobStatus::Canceled
            } else {
                JobStatus::CancelRequested
            };
            record.updated_at_unix_ms = current_time_ms()?;
            self.save(&record)?;
        }
        Ok(record)
    }

    fn path(&self, id: &str) -> Result<PathBuf, StoreError> {
        if id.is_empty()
            || !id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(StoreError::InvalidData("invalid job id".into()));
        }
        Ok(self.directory.join(format!("{id}.json")))
    }
}

fn current_time_ms() -> Result<u64, StoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .as_millis() as u64)
}

fn current_time_ns() -> Result<u128, StoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_are_persistent_ordered_and_cancelable() {
        let root = std::env::temp_dir().join(format!(
            "kitowall-jobs-{}-{}",
            std::process::id(),
            current_time_ns().unwrap()
        ));
        let store = JobStore::new(&root);
        let refresh = store.create(JobKind::Refresh, "remote", 1).unwrap();
        let canceled = store.request_cancel(&refresh.id).unwrap();
        assert_eq!(canceled.status, JobStatus::Canceled);
        assert_eq!(store.load(&refresh.id).unwrap(), canceled);
        assert_eq!(store.list().unwrap(), [canceled]);
        let _ = fs::remove_dir_all(root);
    }
}
