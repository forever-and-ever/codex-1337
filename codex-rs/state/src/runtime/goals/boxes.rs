//! Goal boxes (codex-1337) on the upstream goal row: `goals_migrations/0003_thread_goal_boxes.sql`
//! adds their columns and ends them when their goal completes; deleting the goal deletes them.

use anyhow::Context as _;
use codex_protocol::ThreadId;
use codex_protocol::Usd;
use codex_protocol::goal_boxes::GoalBoxes;
use codex_protocol::goal_boxes::GoalTimebox;
use codex_protocol::goal_boxes::Quotabox;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::GoalStore;

impl GoalStore {
    /// The boxes on the thread's unfinished goal, or `None` when it has no such goal.
    pub async fn goal_boxes(&self, thread_id: ThreadId) -> anyhow::Result<Option<GoalBoxes>> {
        let row = sqlx::query(
            "SELECT timebox_set_at_ms, timebox_deadline_ms, quotabox_percent, quotabox_usd_nanos \
             FROM thread_goals WHERE thread_id = ? AND status != 'complete'",
        )
        .bind(thread_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.as_ref().map(boxes_from_row).transpose()
    }

    /// Sets or clears the timebox on the thread's unfinished goal; `None` when it has none.
    pub async fn set_goal_timebox(
        &self,
        thread_id: ThreadId,
        timebox: Option<GoalTimebox>,
    ) -> anyhow::Result<Option<GoalBoxes>> {
        let row = sqlx::query(
            "UPDATE thread_goals SET timebox_set_at_ms = ?, timebox_deadline_ms = ? \
             WHERE thread_id = ? AND status != 'complete' \
             RETURNING timebox_set_at_ms, timebox_deadline_ms, quotabox_percent, quotabox_usd_nanos",
        )
        .bind(timebox.map(|timebox| timebox.set_at.saturating_mul(1000)))
        .bind(timebox.map(|timebox| timebox.deadline.saturating_mul(1000)))
        .bind(thread_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.as_ref().map(boxes_from_row).transpose()
    }

    /// Sets or clears the quotabox on the thread's unfinished goal; `None` when it has none.
    pub async fn set_goal_quotabox(
        &self,
        thread_id: ThreadId,
        quotabox: Option<Quotabox>,
    ) -> anyhow::Result<Option<GoalBoxes>> {
        let (percent, usd) = match quotabox {
            Some(Quotabox::WeeklyPercentLeft { percent }) => (Some(i64::from(percent)), None),
            Some(Quotabox::UsdAvailable { usd }) => (None, Some(usd.nanodollars())),
            None => (None, None),
        };
        let row = sqlx::query(
            "UPDATE thread_goals SET quotabox_percent = ?, quotabox_usd_nanos = ? \
             WHERE thread_id = ? AND status != 'complete' \
             RETURNING timebox_set_at_ms, timebox_deadline_ms, quotabox_percent, quotabox_usd_nanos",
        )
        .bind(percent)
        .bind(usd)
        .bind(thread_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.as_ref().map(boxes_from_row).transpose()
    }
}

fn boxes_from_row(row: &SqliteRow) -> anyhow::Result<GoalBoxes> {
    let set_at: Option<i64> = row.try_get("timebox_set_at_ms")?;
    let deadline: Option<i64> = row.try_get("timebox_deadline_ms")?;
    let percent: Option<i64> = row.try_get("quotabox_percent")?;
    let usd: Option<i64> = row.try_get("quotabox_usd_nanos")?;
    let quotabox = match (percent, usd) {
        (Some(percent), None) => Some(Quotabox::WeeklyPercentLeft {
            percent: u8::try_from(percent).context("quotabox percent out of range")?,
        }),
        (None, Some(nanodollars)) => Some(Quotabox::UsdAvailable {
            usd: Usd::from_nanodollars(nanodollars),
        }),
        (None, None) => None,
        (Some(_), Some(_)) => anyhow::bail!("goal row holds two quotaboxes"),
    };
    Ok(GoalBoxes {
        timebox: set_at.zip(deadline).map(|(set_at, deadline)| GoalTimebox {
            set_at: set_at / 1000,
            deadline: deadline / 1000,
        }),
        quotabox,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GoalUpdate;
    use crate::StateRuntime;
    use crate::ThreadGoalStatus;
    use crate::runtime::test_support::test_thread_metadata;
    use crate::runtime::test_support::unique_temp_dir;
    use codex_utils_absolute_path::test_support::PathExt;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn boxes_outlive_rewording_and_end_with_completion() {
        let runtime = StateRuntime::init(
            crate::SqliteConfig::new_for_testing(unique_temp_dir().as_path().abs()),
            "test-provider".to_string(),
        )
        .await
        .expect("state db should initialize");
        let thread_id = ThreadId::new();
        let home = runtime.sqlite().home();
        runtime
            .upsert_thread(&test_thread_metadata(
                home,
                thread_id,
                home.join("workspace"),
            ))
            .await
            .expect("test thread should be upserted");
        let goals = runtime.thread_goals();
        let update = |objective: Option<&str>, status| GoalUpdate {
            objective: objective.map(str::to_string),
            status: Some(status),
            token_budget: None,
            expected_goal_id: None,
        };
        let timebox = GoalTimebox {
            set_at: 1_000,
            deadline: 4_600,
        };
        let quotabox = Quotabox::UsdAvailable {
            usd: Usd::from_nanodollars(20_000_000_000),
        };
        let boxed = GoalBoxes {
            timebox: Some(timebox),
            quotabox: Some(quotabox),
        };

        assert_eq!(
            goals
                .set_goal_timebox(thread_id, Some(timebox))
                .await
                .expect("no goal"),
            None
        );
        goals
            .replace_thread_goal(thread_id, "ship it", ThreadGoalStatus::Active, None)
            .await
            .expect("goal");
        goals
            .set_goal_timebox(thread_id, Some(timebox))
            .await
            .expect("timebox");
        assert_eq!(
            goals
                .set_goal_quotabox(thread_id, Some(quotabox))
                .await
                .expect("quotabox"),
            Some(boxed)
        );
        goals
            .update_thread_goal(
                thread_id,
                update(Some("ship it well"), ThreadGoalStatus::Paused),
            )
            .await
            .expect("reword");
        let goal = goals
            .get_thread_goal(thread_id)
            .await
            .expect("read")
            .expect("goal");
        goals
            .replace_thread_goal_snapshot(&goal)
            .await
            .expect("reconcile");
        assert_eq!(
            goals.goal_boxes(thread_id).await.expect("boxes"),
            Some(boxed)
        );

        goals
            .update_thread_goal(thread_id, update(None, ThreadGoalStatus::Complete))
            .await
            .expect("complete");
        let row = sqlx::query(
            "SELECT timebox_set_at_ms, timebox_deadline_ms, quotabox_percent, quotabox_usd_nanos \
             FROM thread_goals WHERE thread_id = ?",
        )
        .bind(thread_id.to_string())
        .fetch_one(goals.pool.as_ref())
        .await
        .expect("goal row");
        assert_eq!(boxes_from_row(&row).expect("boxes"), GoalBoxes::default());
        assert_eq!(
            goals
                .set_goal_quotabox(thread_id, Some(quotabox))
                .await
                .expect("complete"),
            None
        );
    }
}
