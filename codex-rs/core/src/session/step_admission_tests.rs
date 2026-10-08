use super::*;
use pretty_assertions::assert_eq;

#[test]
fn apportion_measures_splits_and_releases() {
    use AdmissionPrecision::Apportioned;
    use AdmissionPrecision::Disturbed;
    use AdmissionPrecision::Exact;
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
