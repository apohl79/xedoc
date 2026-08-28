use std::path::Path;
use std::path::PathBuf;

use dirs::home_dir;

/// If `path` is absolute and inside the home directory, return the relative part.
pub fn relativize_to_home<P>(path: P) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    if !path.is_absolute() {
        return None;
    }

    let home_dir = home_dir()?;
    let rel = path.strip_prefix(&home_dir).ok()?;
    Some(rel.to_path_buf())
}
