//! The stock library: sheets and remnants saved between jobs.
//!
//! Stored as one JSON file. Point it at a shared folder (for example an SMB share on the shop
//! server) and everyone on the floor sees the same stock.

use crate::{CoreError, Stock, StockId};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Library {
    pub stock: Vec<Stock>,
}

impl Library {
    /// Loads the library, or returns an empty one if the file doesn't exist yet.
    pub fn load(path: &Path) -> Result<Library, CoreError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Library::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes to a temp file and renames, so a crash mid-save can't corrupt the library.
    pub fn save(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn next_id(&self) -> StockId {
        self.stock.iter().map(|s| s.id).max().unwrap_or(0) + 1
    }
}
