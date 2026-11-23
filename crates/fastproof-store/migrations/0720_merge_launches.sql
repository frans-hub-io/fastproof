set local lock_timeout = '5s';

alter table launches add column if not exists merged_into uuid;
alter table attempts add column if not exists merged_from uuid;
alter table launch_containers add column if not exists merged_from uuid;
alter table attempt_log_chunks add column if not exists merged_from uuid;
alter table attachment_links add column if not exists merged_from uuid;

create table if not exists launch_merges (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    target_id uuid not null,
    target_created boolean not null,
    sources uuid[] not null,
    source_seqs bigint[] not null,
    debug boolean not null default false,
    status text not null default 'active',
    final boolean not null default false,
    counts jsonb not null default '{}',
    created_by text not null,
    created_at timestamptz not null default now(),
    undo_until timestamptz not null,
    synced_at timestamptz not null default now(),
    undone_at timestamptz,
    undone_by text,
    finalized_at timestamptz,
    constraint launch_merges_status_check check (status in ('active', 'undone', 'final', 'deleted'))
);
create index if not exists launch_merges_project on launch_merges (project_id, created_at);
create index if not exists launch_merges_target on launch_merges (target_id);
create index if not exists launch_merges_sources on launch_merges using gin (sources);
create index if not exists launch_merges_open on launch_merges (undo_until) where status = 'active';
