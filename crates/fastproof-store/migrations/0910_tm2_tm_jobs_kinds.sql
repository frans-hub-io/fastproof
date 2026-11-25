set local lock_timeout = '5s';
alter table tm_jobs drop constraint if exists tm_jobs_kind_check;
alter table tm_jobs add constraint tm_jobs_kind_check check (kind in ('import', 'export', 'backfill', 'last_status',
    'shared_inline', 'workflow_remap', 'requirements', 'coverage_export')) not valid;
alter table tm_jobs validate constraint tm_jobs_kind_check;
