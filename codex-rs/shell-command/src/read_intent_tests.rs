use super::*;
use pretty_assertions::assert_eq;

#[test]
fn literal_reads_exclude_composition_and_expansion() {
    for command in [
        "cat file | head",
        "cat file; cat other",
        "cat $FILE",
        "cat *.rs",
        "sed -i '1,2p' file",
        "cat a b",
    ] {
        assert!(ReadIntent::parse(command).is_none(), "{command}");
    }
    let read = ReadIntent::parse("sed -n '12,24p' 'a b.rs'").unwrap();
    assert_eq!(
        read,
        ReadIntent {
            path: "a b.rs".into(),
            range: ReadRange::Lines {
                first: 12,
                last: 24
            }
        }
    );
}
