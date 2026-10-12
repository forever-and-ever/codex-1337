use super::BottomPane;
use crate::local_shell::LocalShellAdmission;

impl BottomPane {
    pub(crate) fn set_local_shell_context(
        &mut self,
        admission: LocalShellAdmission,
        notice: Option<String>,
    ) {
        self.composer.set_local_shell_context(admission, notice);
    }
}
