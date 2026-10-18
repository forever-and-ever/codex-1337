use super::GoalExtension;
use super::GoalExtensionConfig;
use super::goal_runtime_handle;
use codex_extension_api::ContextContributor;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::PreviousWorldStateSection;
use codex_extension_api::RenderedWorldStateFragment;
use codex_extension_api::WorldStateContributionInput;
use codex_extension_api::WorldStateSectionContribution;
use codex_prompts::without_update_plan_instructions;
use serde_json::Value;
use std::sync::LazyLock;

const SECTION_ID: &str = "goal_continuation";
const START_MARKER: &str = "<codex_internal_context source=\"goal\">";
const END_MARKER: &str = "</codex_internal_context>";
const INSTRUCTIONS: &str = include_str!("../templates/goals/instructions.md");
static INSTRUCTIONS_WITHOUT_UPDATE_PLAN: LazyLock<String> =
    LazyLock::new(|| without_update_plan_instructions(INSTRUCTIONS));

impl<C: Send + Sync> ContextContributor for GoalExtension<C> {
    fn contribute_world_state<'a>(
        &'a self,
        input: WorldStateContributionInput<'a>,
    ) -> ExtensionFuture<'a, Vec<WorldStateSectionContribution>> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return Vec::new();
            };
            if !runtime.tools_available()
                || runtime
                    .accounting_state()
                    .current_active_goal_id_for_turn(input.turn_id)
                    .is_none()
            {
                return Vec::new();
            }
            let Some(config) = input.thread_store.get::<GoalExtensionConfig>() else {
                unreachable!(
                    "goal extension initializes thread configuration before rendering world state"
                );
            };
            vec![continuation_rules_section(config.update_plan_enabled)]
        })
    }
}

fn continuation_rules_section(update_plan_enabled: bool) -> WorldStateSectionContribution {
    let instructions = if update_plan_enabled {
        INSTRUCTIONS
    } else {
        INSTRUCTIONS_WITHOUT_UPDATE_PLAN.as_str()
    };
    let body = format!("\n{instructions}\n");
    let retained = format!("{START_MARKER}{body}{END_MARKER}");
    WorldStateSectionContribution::new(SECTION_ID, move |previous| {
        let fragment = match previous {
            PreviousWorldStateSection::Unknown => None,
            PreviousWorldStateSection::Known(previous)
                if previous.as_str() == Some(instructions) =>
            {
                None
            }
            PreviousWorldStateSection::Absent | PreviousWorldStateSection::Known(_) => Some(
                RenderedWorldStateFragment::new("user", (START_MARKER, END_MARKER), body.clone()),
            ),
        };
        (Some(Value::String(instructions.to_owned())), fragment)
    })
    .with_retained_fragment_matcher(move |role, text| role == "user" && text == retained)
}
