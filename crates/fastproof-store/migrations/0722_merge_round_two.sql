set local lock_timeout = '5s';

alter table launches add column if not exists created_by_key text;
alter table launch_merges add column if not exists created_by_key text;
alter table launch_merges add column if not exists log_marks bigint[];
alter table launch_merges add column if not exists fingerprints jsonb;

alter table launch_merges add column if not exists window_start timestamptz;

create table if not exists launch_merge_work (
    merge_id uuid not null references launch_merges (id) on delete cascade,
    project_id uuid not null,
    series_id uuid not null,
    delta integer not null default 0,
    primary key (merge_id, series_id)
);
create index if not exists launch_merge_work_project on launch_merge_work (project_id);
