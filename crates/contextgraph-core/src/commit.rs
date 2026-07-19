//! The commit model: the immutable, content-addressed unit of a contextgraph DAG.

use crate::error::{GraphError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// A BLAKE3 content hash identifying a commit. Two commits with byte-identical
/// `parent_ids` + `author` + `delta` + `metadata` always produce the same id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitId(pub [u8; 32]);

impl CommitId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl FromStr for CommitId {
    type Err = GraphError;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = hex::decode(s).map_err(|e| GraphError::InvalidCommitId(format!("{s}: {e}")))?;
        if bytes.len() != 32 {
            return Err(GraphError::InvalidCommitId(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(CommitId(arr))
    }
}

impl TryFrom<String> for CommitId {
    type Error = GraphError;
    fn try_from(value: String) -> Result<Self> {
        CommitId::from_str(&value)
    }
}

impl From<CommitId> for String {
    fn from(value: CommitId) -> Self {
        value.to_hex()
    }
}
