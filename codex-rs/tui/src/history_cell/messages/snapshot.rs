//! Cloning freezes the mutable live-cell revision for background rendering.

use super::AgentMarkdownCell;
use super::MarkdownRenderCache;

impl Clone for AgentMarkdownCell {
    fn clone(&self) -> Self {
        Self {
            timestamp: self.timestamp,
            item_id: self.item_id.clone(),
            interrupted_item: self.interrupted_item.clone(),
            markdown_source: self.markdown_source.clone(),
            copy_source: self.copy_source.clone(),
            cwd: self.cwd.clone(),
            inline_visualization_context: self.inline_visualization_context.clone(),
            rendered_lines: self
                .rendered_lines
                .as_ref()
                .map(|_| MarkdownRenderCache::default()),
            spoken_artifacts: self.spoken_artifacts,
            question_titles: self.question_titles.clone(),
        }
    }
}
