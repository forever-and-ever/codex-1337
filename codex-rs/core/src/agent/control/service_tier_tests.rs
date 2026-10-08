use super::*;

#[test]
fn service_tier_inheritance_stops_at_the_nearest_explicit_ancestor() {
    // A root-wide broadcast would erase the middle override; copying at spawn would miss resets.
    let control = LocalAgentControl::default();
    let root = ThreadId::new();
    let parent = ThreadId::new();
    let child = ThreadId::new();
    let sibling = ThreadId::new();
    control.set_root_service_tier(Some("priority".to_owned()));
    control.set_agent_tier_binding(parent, root, Some("default".to_owned()));
    control.set_agent_tier_binding(child, parent, /*selected*/ None);
    control.set_agent_tier_binding(sibling, root, Some("priority".to_owned()));
    assert_eq!(
        control.agent_service_tier(child).as_deref(),
        Some("default")
    );
    control.set_root_service_tier(/*service_tier*/ None);
    assert_eq!(
        control.agent_service_tier(child).as_deref(),
        Some("default")
    );
    control.set_agent_tier_binding(parent, root, /*selected*/ None);
    assert_eq!(control.agent_service_tier(child), None);
    assert_eq!(
        control.agent_service_tier(sibling).as_deref(),
        Some("priority")
    );
    control.set_agent_tier_binding(child, parent, Some("default".to_owned()));
    control.set_agent_tier_binding(parent, root, Some("priority".to_owned()));
    assert_eq!(
        control.agent_service_tier(child).as_deref(),
        Some("default")
    );
}
