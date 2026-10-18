use super::*;
use pretty_assertions::assert_eq;

#[test]
fn apportion_measures_splits_and_releases() {
    use AdmissionPrecision::Apportioned;
    use AdmissionPrecision::Disturbed;
    use AdmissionPrecision::Exact;

    // Token accounting may join blocks for its estimate; retained evidence must not.
    let original = vec!["first".to_owned(), "  second\n".to_owned()];
    let item: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "message",
        "role": "developer",
        "content": original.iter().map(|text| serde_json::json!({"type": "input_text", "text": text})).collect::<Vec<_>>(),
    })).unwrap();
    let admitted = admit(&[item]);
    assert_eq!(admitted.arrivals.len(), 1);
    assert_eq!(
        admitted.arrivals[0].source,
        codex_protocol::step_admission::ArrivalSource::Captured { blocks: original },
    );
    // Missing aliases on the nearest actual user cannot be crossed in either direction.
    let user = |id: Option<&str>| {
        serde_json::from_value::<ResponseItem>(serde_json::json!({
            "type": "message", "id": id, "role": "user",
            "content": [{"type": "input_text", "text": "question"}],
        }))
        .unwrap()
    };
    let context = || {
        serde_json::from_value::<ResponseItem>(serde_json::json!({
            "type": "message", "role": "developer",
            "content": [{"type": "input_text", "text": "context"}],
        }))
        .unwrap()
    };
    assert_eq!(
        arrival_placement(&[context(), user(None), user(Some("farther"))], 0),
        None
    );
    assert_eq!(
        arrival_placement(&[user(Some("farther")), user(None), context()], 2),
        None
    );
    assert_eq!(
        arrival_placement(&[user(Some("earlier")), context(), user(None)], 1),
        None
    );
    let cases = [
        // A lone result owns the whole delta, whatever its local count.
        (1_200, vec![Some(700)], vec![1_200], 0, Exact),
        // Media cannot be counted locally, but a lone share needs no count.
        (5_000, vec![None], vec![5_000], 0, Exact),
        // A final answer followed by the next prompt: only the prompt contributes.
        (90, vec![Some(0), Some(60)], vec![0, 90], 0, Exact),
        // Claude counts run above o200k; the split keeps local proportions.
        (
            1_700,
            vec![Some(600), Some(400)],
            vec![1_020, 680],
            0,
            Apportioned,
        ),
        // An encrypted message takes what the measured shares leave.
        (1_000, vec![Some(600), None], vec![600, 400], 0, Apportioned),
        // Dropped earlier thinking: the delta is below the local lower bound.
        (
            -30_000,
            vec![Some(0), Some(50)],
            vec![0, 50],
            30_050,
            Disturbed,
        ),
    ];
    for (received, local, shares, released, precision) in cases {
        assert_eq!(
            apportion(received, &local),
            Apportionment {
                shares,
                released,
                precision
            },
            "received {received}, local {local:?}"
        );
    }
}
