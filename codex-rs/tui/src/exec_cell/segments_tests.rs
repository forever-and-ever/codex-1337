use super::*;
use pretty_assertions::assert_eq;

#[test]
fn outer_boundaries_preserve_shell_source() {
    for (script, commands) in [
        (
            "echo 'a;b';cat x&&git status||true",
            vec!["echo 'a;b'", "cat x", "git status", "true"],
        ),
        (
            "echo $(a;b) && (c;d) || cat x | tail -n 2",
            vec!["echo $(a;b) ", " (c;d) ", " cat x | tail -n 2"],
        ),
        (
            "if a; then b && c; fi; d",
            vec!["if a; then b && c; fi", " d"],
        ),
        (
            "cat <<'EOF'\na;b && c\nEOF\ngit status",
            vec!["cat <<'EOF'\na;b && c\nEOF\ngit status"],
        ),
        ("echo 'unfinished; cat x", vec!["echo 'unfinished; cat x"]),
    ] {
        let segments = shell_segments(script);
        assert_eq!(
            segments
                .iter()
                .map(|segment| &script[segment.source.clone()])
                .collect::<Vec<_>>(),
            commands
        );
        let rebuilt = segments.iter().fold(String::new(), |mut rebuilt, segment| {
            rebuilt.push_str(&script[segment.source.clone()]);
            rebuilt.push_str(segment.connector.map_or("", Connector::suffix).trim_start());
            rebuilt
        });
        assert_eq!(rebuilt, script);
    }
}
