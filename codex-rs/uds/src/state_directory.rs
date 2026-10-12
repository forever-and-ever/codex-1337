use std::env::var_os;
use std::path::PathBuf;

// Resolves a storage location, not trusted input or authority for deletion.
pub fn xdg_state_directory() -> Option<PathBuf> {
    var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })
}
