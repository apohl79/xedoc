pub(crate) use xedoc_skills::install_system_skills;
pub(crate) use xedoc_skills::system_cache_root_dir;

use xedoc_utils_absolute_path::AbsolutePathBuf;

pub(crate) fn uninstall_system_skills(xedoc_home: &AbsolutePathBuf) {
    let _ = std::fs::remove_dir_all(system_cache_root_dir(xedoc_home));
}
