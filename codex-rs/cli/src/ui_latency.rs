use clap::{Args, Subcommand};
use codex_tui::ui_latency::{self, Report};

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Action,
}
#[derive(Clone, Copy, Debug, Subcommand)]
enum Action {
    /// Find responsive TUI workers, including separate reload generations.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Read wall-time counters directly from one TUI worker.
    Dump {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        json: bool,
    },
    /// Request a fresh epoch, without waiting for the UI to make progress.
    Reset {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        json: bool,
    },
}
pub(crate) async fn run(command: &Command) -> anyhow::Result<()> {
    match command.action {
        Action::List { json } => {
            let workers = ui_latency::discover().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&workers)?);
            } else {
                for worker in workers.workers {
                    let id = worker.identity;
                    println!(
                        "{} start={} generation={} channel={} build={} tty={}",
                        id.pid,
                        id.start_ticks,
                        id.reload_generation,
                        id.channel,
                        id.build,
                        id.terminal_device
                    );
                }
                for pid in workers.unresponsive_pids {
                    println!("{pid}: diagnostic endpoint unresponsive");
                }
                if workers.partial {
                    println!(
                        "Discovery incomplete: bounded scan/deadline reached; use dump --pid PID directly."
                    );
                }
            }
        }
        Action::Dump { pid, json } => display(ui_latency::query(pid, false).await?, json)?,
        Action::Reset { pid, json } => display(ui_latency::query(pid, true).await?, json)?,
    }
    Ok(())
}
fn display(report: Report, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let id = &report.identity;
    println!(
        "UI {} start={} generation={} channel={} build={}",
        id.pid, id.start_ticks, id.reload_generation, id.channel, id.build
    );
    println!(
        "Epoch {} (active {}{}) • {} bytes • {} incomplete records",
        report.requested_epoch,
        report.active_epoch,
        if report.reset_pending {
            ", reset pending"
        } else {
            ""
        },
        report.recorder_bytes,
        report.incomplete_records
    );
    if report.suppressed_scopes != 0 {
        println!(
            "Coverage incomplete: {} nested scopes suppressed at fixed capacity",
            report.suppressed_scopes
        );
    }
    if let Some(current) = &report.current {
        if let Some(stage) = current.stage {
            let ms = report.observed_ns.saturating_sub(current.started_ns) as f64 / 1e6;
            let warning = if ms >= 50.0 && stage != "IdleSelect" {
                "SLOW: "
            } else {
                ""
            };
            println!(
                "{warning}Current: {stage} ({ms:.3} ms), parent={}, entry epoch={}",
                current.parent.unwrap_or("none"),
                current.entry_epoch
            );
        } else {
            println!("Current: between stages");
        }
    } else {
        println!("Current stage publication incomplete; consult last committed marker.");
    }
    if let Some(last) = &report.last_committed {
        println!(
            "Last committed: {} {:.3} ms, epoch {}, {:.3} ms ago",
            last.stage.unwrap_or("none"),
            last.duration_ns as f64 / 1e6,
            last.epoch,
            report.observed_ns.saturating_sub(last.completed_ns) as f64 / 1e6
        );
    }
    if let Some(input) = &report.input {
        println!(
            "Input gate: {} • backlog app/thread/primary={}/{}/{} • marker {:.3} ms old",
            input.gate,
            input.app_event_backlog,
            input.active_thread_backlog,
            input.primary_backlog,
            report.observed_ns.saturating_sub(input.published_ns) as f64 / 1e6
        );
    }
    if report.last_input_consumed_ns != 0 {
        println!(
            "Last consumed input: {:.3} ms ago (not physical key latency)",
            report
                .observed_ns
                .saturating_sub(report.last_input_consumed_ns) as f64
                / 1e6
        );
    }
    println!(
        "Inclusive wall time; totals overlap. IdleSelect includes polling/wait; RPC waits are not CPU work."
    );
    for stats in &report.stages {
        if stats.stage == "ReactionLatency" {
            let percentile_upper = |percentile: u128| {
                let rank = (u128::from(stats.count) * percentile).div_ceil(100);
                let mut cumulative = 0_u128;
                stats
                    .histogram
                    .iter()
                    .zip(report.histogram_upper_ns)
                    .find_map(|(count, upper)| {
                        cumulative += u128::from(*count);
                        (cumulative >= rank).then_some(upper.min(stats.max_ns))
                    })
                    .unwrap_or(stats.max_ns) as f64
                    / 1e6
            };
            println!(
                "Consumed input → first post-handler frame flush: p50 ≤{:.3} ms, p99 ≤{:.3} ms (histogram bounds; inputs coalesced per frame)",
                percentile_upper(50),
                percentile_upper(99),
            );
        }
        println!(
            "{}: {} spans, total {:.3} ms, max {:.3} ms{}",
            stats.stage,
            stats.count,
            stats.total_ns as f64 / 1e6,
            stats.max_ns as f64 / 1e6,
            if stats.max_ns >= 50_000_000 && stats.stage != "IdleSelect" {
                " SLOW"
            } else {
                ""
            }
        );
    }
    for slow in &report.recent_slow {
        println!(
            "{}{} {:.3} ms @ {:.3} ms",
            if slow.duration_ns >= 50_000_000 {
                "SLOW: "
            } else {
                ""
            },
            slow.stage.unwrap_or("unknown"),
            slow.duration_ns as f64 / 1e6,
            slow.started_ns as f64 / 1e6
        );
    }
    Ok(())
}
