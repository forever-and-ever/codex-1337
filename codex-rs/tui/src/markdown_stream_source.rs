//! Worker rendering borrows the collector's source; it never assembles a second message buffer.

impl super::MarkdownStreamCollector {
    pub(crate) fn source(&self) -> &str {
        &self.buffer
    }
}
