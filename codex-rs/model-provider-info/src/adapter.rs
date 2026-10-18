use crate::ModelProviderAdapter;

impl ModelProviderAdapter {
    #[must_use]
    pub fn speaks_anthropic_messages(self) -> bool {
        match self {
            Self::ClaudeSubscription => true,
            Self::OpenaiCompatible | Self::OpenRouter => false,
        }
    }

    #[must_use]
    pub fn borrows_chatgpt(self) -> bool {
        match self {
            Self::ClaudeSubscription | Self::OpenRouter => true,
            Self::OpenaiCompatible => false,
        }
    }

    #[must_use]
    pub fn supports_background_scoring(self) -> bool {
        match self {
            Self::OpenaiCompatible | Self::ClaudeSubscription => true,
            Self::OpenRouter => false,
        }
    }
}
