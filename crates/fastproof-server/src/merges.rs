//! The scheduled job of launch merges (`launches.merges`, every minute): attempts that reached
//! a merged launch after its merge (a Kafka writer or an upload in flight) are copied into the
//! target, merges whose undo window ended are finalized, and launches merged into a launch
//! that was deleted are deleted too.

use crate::scheduler::Scheduler;
use crate::state::AppState;

pub fn register_jobs(sch: &mut Scheduler) -> anyhow::Result<()> {
    sch.register("launches.merges", "1m", |s: AppState| async move {
        let r = s.store.merge_maintenance().await?;
        if r != Default::default() {
            tracing::info!(
                synced = r.synced,
                finalized = r.finalized,
                orphans_deleted = r.orphans_deleted,
                "launch merges maintained"
            );
        }
        Ok(())
    })
}
