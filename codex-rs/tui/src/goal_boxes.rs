//! Goal boxes in the TUI (codex-1337): the `/goal timebox` and `/goal quotabox` grammar, and
//! notes on the boxes the server reports.

use codex_protocol::Usd;
use codex_protocol::goal_boxes::GoalBoxes;
use codex_protocol::goal_boxes::GoalTimebox;
use codex_protocol::goal_boxes::QuotaGauge;
use codex_protocol::goal_boxes::QuotaReading;
use codex_protocol::goal_boxes::Quotabox;
use jiff::SignedDuration;
use jiff::Span;
use jiff::Timestamp;
use jiff::Zoned;
use jiff::civil;
use jiff::tz::TimeZone;

const TIMEBOX_USAGE: &str = "Usage: /goal timebox [<duration>|until <time>|clear]";
const QUOTABOX_USAGE: &str = "Usage: /goal quotabox [<percent>%|$<dollars>|clear]";

/// A `/goal timebox` or `/goal quotabox` (alias `/goal stopat`) command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum GoalBoxCommand {
    /// Show the goal's boxes.
    Show,
    /// Set the timebox to end at a Unix second, or clear it.
    Timebox(Option<i64>),
    /// Set the quotabox, or clear it.
    Quotabox(Option<Quotabox>),
}

/// What the TUI says about goal boxes.
pub(crate) struct BoxNote {
    pub(crate) headline: &'static str,
    pub(crate) detail: Option<String>,
}

impl GoalBoxCommand {
    /// Parses `/goal timebox` arguments: a duration from `now`, `until` a time, or `clear`.
    pub(crate) fn timebox(args: &str, now: &Zoned) -> Result<Self, String> {
        let args = args.trim();
        if args.is_empty() {
            return Ok(Self::Show);
        }
        if args.eq_ignore_ascii_case("clear") {
            return Ok(Self::Timebox(None));
        }
        let deadline = match args.split_once(char::is_whitespace) {
            Some((keyword, time)) if keyword.eq_ignore_ascii_case("until") => {
                instant(time.trim(), now)
            }
            _ => args
                .parse::<Span>()
                .ok()
                .and_then(|span| now.checked_add(span).ok())
                .map(|end| end.timestamp()),
        };
        deadline
            .map(|deadline| Self::Timebox(Some(deadline.as_second())))
            .ok_or_else(|| format!("Cannot read '{args}' as a duration or a time. {TIMEBOX_USAGE}"))
    }

    /// Parses `/goal quotabox` arguments: a percent of the weekly quota left, dollars, or
    /// `clear`.
    pub(crate) fn quotabox(args: &str) -> Result<Self, String> {
        let args = args.trim();
        if args.is_empty() {
            return Ok(Self::Show);
        }
        if args.eq_ignore_ascii_case("clear") {
            return Ok(Self::Quotabox(None));
        }
        let quotabox = match args.strip_suffix('%') {
            Some(percent) => percent
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|percent| (1..=99).contains(percent))
                .map(|percent| Quotabox::WeeklyPercentLeft { percent }),
            None => args
                .strip_prefix('$')
                .or_else(|| args.strip_suffix('$'))
                .and_then(|dollars| dollars.trim().parse::<Usd>().ok())
                .filter(|usd| usd.nanodollars() > 0)
                .map(|usd| Quotabox::UsdAvailable { usd }),
        };
        quotabox
            .map(|quotabox| Self::Quotabox(Some(quotabox)))
            .ok_or_else(|| format!("Cannot read '{args}' as a quota level. {QUOTABOX_USAGE}"))
    }

    /// What the TUI says once the server answers this command with `boxes` and `gauge`.
    pub(crate) fn note(self, boxes: GoalBoxes, gauge: Option<&QuotaGauge>) -> BoxNote {
        let now = Timestamp::now().as_second();
        let (headline, detail) = match self {
            Self::Show if boxes.is_empty() => (
                "No goal boxes",
                Some(format!("{TIMEBOX_USAGE} · {QUOTABOX_USAGE}")),
            ),
            Self::Show => (
                "Goal boxes",
                Some(
                    boxes
                        .timebox
                        .map(|timebox| timebox_text(timebox, now))
                        .into_iter()
                        .chain(
                            boxes
                                .quotabox
                                .map(|quotabox| quotabox_text(quotabox, gauge)),
                        )
                        .collect::<Vec<_>>()
                        .join(" · "),
                ),
            ),
            Self::Timebox(_) => match boxes.timebox {
                Some(timebox) => ("Timebox set", Some(timebox_text(timebox, now))),
                None => ("Timebox cleared", None),
            },
            Self::Quotabox(_) => match boxes.quotabox {
                Some(quotabox) => ("Quotabox set", Some(quotabox_text(quotabox, gauge))),
                None => ("Quotabox cleared", None),
            },
        };
        BoxNote { headline, detail }
    }
}

/// The instant `text` names, judged from `now`: a Unix second (`@1793000000`, or ten bare
/// digits), an RFC 9557 or RFC 3339 instant, or a civil datetime, date or time in `now`'s
/// zone. A bare date means its start; a bare time, its next occurrence.
fn instant(text: &str, now: &Zoned) -> Option<Timestamp> {
    let unix = text.strip_prefix('@').or_else(|| {
        (text.len() == 10 && text.bytes().all(|byte| byte.is_ascii_digit())).then_some(text)
    });
    if let Some(seconds) = unix {
        return Timestamp::from_second(seconds.parse().ok()?).ok();
    }
    if let Ok(zoned) = text.parse::<Zoned>() {
        return Some(zoned.timestamp());
    }
    if let Ok(timestamp) = text.parse::<Timestamp>() {
        return Some(timestamp);
    }
    let zone = now.time_zone().clone();
    if let Ok(datetime) = text.parse::<civil::DateTime>() {
        return datetime.to_zoned(zone).ok().map(|zoned| zoned.timestamp());
    }
    if let Ok(date) = text.parse::<civil::Date>() {
        return date.to_zoned(zone).ok().map(|zoned| zoned.timestamp());
    }
    let today = now
        .with()
        .time(text.parse::<civil::Time>().ok()?)
        .build()
        .ok()?;
    let next = if today > *now {
        today
    } else {
        today.tomorrow().ok()?
    };
    Some(next.timestamp())
}

fn timebox_text(timebox: GoalTimebox, now: i64) -> String {
    let at = Timestamp::from_second(timebox.deadline).map_or_else(
        |_| timebox.deadline.to_string(),
        |at| {
            at.to_zoned(TimeZone::system())
                .strftime("%b %-d %H:%M")
                .to_string()
        },
    );
    let left = timebox.deadline - now;
    if left > 0 {
        // Whole minutes, rounded up, read better than a ticking count of seconds.
        let left = SignedDuration::from_secs((left + 59) / 60 * 60);
        format!("Timebox: the goal pauses at {at}, {left:#} from now")
    } else {
        format!("Timebox: ended at {at}")
    }
}

fn quotabox_text(quotabox: Quotabox, gauge: Option<&QuotaGauge>) -> String {
    let now = match gauge {
        Some(QuotaGauge::Read {
            reading: QuotaReading::WeeklyPercentLeft { percent },
        }) => format!("; now {percent:.0}% left"),
        Some(QuotaGauge::Read {
            reading: QuotaReading::UsdAvailable { usd },
        }) => format!("; now {usd} available"),
        Some(QuotaGauge::Unavailable { message }) => format!("; quota unreadable: {message}"),
        None => String::new(),
    };
    format!("Quotabox: the goal pauses at {quotabox}{now}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn at(instant: &str) -> i64 {
        instant
            .parse::<Zoned>()
            .expect("test instant")
            .timestamp()
            .as_second()
    }

    #[test]
    fn goal_box_grammar() {
        let now: Zoned = "2026-10-23T09:00[America/New_York]".parse().expect("now");
        let timebox = |args: &str| GoalBoxCommand::timebox(args, &now);
        let set = |instant: &str| Ok(GoalBoxCommand::Timebox(Some(at(instant))));
        assert_eq!(timebox(" "), Ok(GoalBoxCommand::Show));
        assert_eq!(timebox("CLEAR"), Ok(GoalBoxCommand::Timebox(None)));
        for (args, deadline) in [
            ("12h", "2026-10-23T21:00[America/New_York]"),
            ("1h30m", "2026-10-23T10:30[America/New_York]"),
            ("2 hours", "2026-10-23T11:00[America/New_York]"),
            ("PT12H", "2026-10-23T21:00[America/New_York]"),
            ("3d", "2026-10-26T09:00[America/New_York]"),
            (
                "until 2026-10-24T08:00[Europe/Paris]",
                "2026-10-24T08:00[Europe/Paris]",
            ),
            (
                "until 2026-10-24T12:00Z",
                "2026-10-24T08:00[America/New_York]",
            ),
            (
                "until 2026-10-24T08:00",
                "2026-10-24T08:00[America/New_York]",
            ),
            (
                "Until 2026-10-24 08:00",
                "2026-10-24T08:00[America/New_York]",
            ),
            ("until 2026-10-24", "2026-10-24T00:00[America/New_York]"),
            ("until 17:30", "2026-10-23T17:30[America/New_York]"),
            ("until 08:00", "2026-10-24T08:00[America/New_York]"),
        ] {
            assert_eq!(timebox(args), set(deadline), "{args}");
        }
        let deadline = at("2026-10-24T08:00[America/New_York]");
        for args in [format!("until @{deadline}"), format!("until {deadline}")] {
            assert_eq!(
                timebox(&args),
                Ok(GoalBoxCommand::Timebox(Some(deadline))),
                "{args}"
            );
        }
        for args in ["soon", "12", "until whenever", "until 25:00"] {
            assert!(timebox(args).is_err(), "{args}");
        }

        let quotabox = |level| Ok(GoalBoxCommand::Quotabox(Some(level)));
        let usd = |nanos| Quotabox::UsdAvailable {
            usd: Usd::from_nanodollars(nanos),
        };
        assert_eq!(GoalBoxCommand::quotabox(""), Ok(GoalBoxCommand::Show));
        assert_eq!(
            GoalBoxCommand::quotabox("clear"),
            Ok(GoalBoxCommand::Quotabox(None))
        );
        for (args, level) in [
            ("30%", Quotabox::WeeklyPercentLeft { percent: 30 }),
            ("5 %", Quotabox::WeeklyPercentLeft { percent: 5 }),
            ("$20", usd(20_000_000_000)),
            ("20$", usd(20_000_000_000)),
            ("$12.50", usd(12_500_000_000)),
        ] {
            assert_eq!(GoalBoxCommand::quotabox(args), quotabox(level), "{args}");
        }
        for args in ["0%", "100%", "30", "$0", "$-5", "much"] {
            assert!(GoalBoxCommand::quotabox(args).is_err(), "{args}");
        }
    }
}
