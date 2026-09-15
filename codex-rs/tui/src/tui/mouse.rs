//! Mouse reporting is owned by the visible roster, not by terminal focus.
use super::*;

impl Tui {
    pub(crate) fn set_mouse_capture(&mut self, capture: bool) -> io::Result<()> {
        let capture = capture && cfg!(target_os = "linux");
        if capture != self.mouse_capture {
            if capture {
                execute!(
                    self.terminal.backend_mut(),
                    crossterm::event::EnableMouseCapture
                )?;
            } else {
                execute!(
                    self.terminal.backend_mut(),
                    crossterm::event::DisableMouseCapture
                )?;
            }
            self.mouse_capture = capture;
        }
        Ok(())
    }
}
