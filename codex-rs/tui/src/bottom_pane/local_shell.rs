use super::BottomPane;

impl BottomPane {
    pub(crate) fn set_local_shell_notice(&mut self, notice: Option<String>) {
        self.composer.set_local_shell_notice(notice);
    }
}
