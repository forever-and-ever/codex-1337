use codex_utils_absolute_path::AbsolutePathBuf;

use crate::ExecServerRuntimeOptions;

pub(super) fn read_roots(runtime_paths: &ExecServerRuntimeOptions) -> Vec<AbsolutePathBuf> {
    let mut roots = Vec::new();
    for helper in std::iter::once(&runtime_paths.codex_self_exe)
        .chain(runtime_paths.codex_linux_sandbox_exe.iter())
    {
        if !roots.contains(helper) {
            roots.push(helper.clone());
        }
        // The sandbox re-execs current_exe, which resolves an arg0 alias to its image.
        // Grant that file, never its parent. Failed resolution remains an execution error.
        if let Ok(image) = helper.canonicalize()
            && !roots.contains(&image)
        {
            roots.push(image);
        }
    }
    roots
}
