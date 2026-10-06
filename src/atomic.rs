//! Writing a file so that a crash leaves the old content or the new one,
//! never half of it.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};

/// Writes through a temporary file in the same directory, which is created
/// when it is missing.
pub fn write(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temporary = dir.join(name);
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("cannot write {}", path.display()))
}
