//! The commit model: the immutable, content-addressed unit of a contextgraph DAG.

use crate::error::{GraphError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

