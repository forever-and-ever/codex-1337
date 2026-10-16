use crate::ModelProviderAdapter;

impl ModelProviderAdapter {
    pub fn speaks_anthropic_messages(self) -> bool {
        match self {
            Self::ClaudeSubscription => true,
            Self::OpenaiCompatible | Self::OpenRouter => false,
        }
    }

    pub fn borrows_chatgpt(self) -> bool {
        match self {
            Self::ClaudeSubscription | Self::OpenRouter => true,
            Self::OpenaiCompatible => false,
        }
    }
}
