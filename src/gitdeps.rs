//! Dependencies fetched with git (placeholder; replaced below).

use crate::project::Dependency;
use std::path::{Path, PathBuf};

pub fn checkout(_project: &Path, _dep: &Dependency) -> Result<PathBuf, String> {
    Err("git dependencies are not supported yet".to_string())
}
