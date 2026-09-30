use std::collections::BTreeMap;

use pretty_assertions::assert_eq;

use super::*;

struct FailedReads(StaticSkillProvider);

impl SkillProvider for FailedReads {
    fn list(&self, query: SkillListQuery) -> SkillProviderFuture<'_, SkillCatalog> {
        self.0.list(query)
    }

    fn read<'a>(
        &'a self,
        _request: SkillReadRequest<'a>,
    ) -> SkillProviderFuture<'a, SkillReadResult> {
        Box::pin(async { Err(SkillProviderError::new("read unavailable")) })
    }

    fn search(&self, request: SkillSearchRequest) -> SkillProviderFuture<'_, SkillSearchResult> {
        self.0.search(request)
    }
}

fn text_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

fn explicit_input() -> Vec<UserInput> {
    vec![UserInput::Skill {
        name: "x".to_string(),
        path: PathBuf::from("x/SKILL.md"),
    }]
}
