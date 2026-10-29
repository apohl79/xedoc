use dirs::home_dir;
use std::path::PathBuf;
use xedoc_utils_absolute_path::AbsolutePathBuf;

/// Returns the path to the Xedoc configuration directory, which can be
/// specified by the `XEDOC_HOME` environment variable. If not set, defaults to
/// `~/.xedoc`.
///
/// - If `XEDOC_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `XEDOC_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_xedoc_home() -> std::io::Result<AbsolutePathBuf> {
    let xedoc_home_env = std::env::var("XEDOC_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_xedoc_home_from_env(xedoc_home_env.as_deref())
}

fn find_xedoc_home_from_env(xedoc_home_env: Option<&str>) -> std::io::Result<AbsolutePathBuf> {
    // Honor the `XEDOC_HOME` environment variable when it is set to allow users
    // (and tests) to override the default location.
    match xedoc_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("XEDOC_HOME points to {val:?}, but that path does not exist"),
                ),
                _ => std::io::Error::new(
                    err.kind(),
                    format!("failed to read XEDOC_HOME {val:?}: {err}"),
                ),
            })?;

            if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("XEDOC_HOME points to {val:?}, but that path is not a directory"),
                ))
            } else {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize XEDOC_HOME {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".xedoc");
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}
