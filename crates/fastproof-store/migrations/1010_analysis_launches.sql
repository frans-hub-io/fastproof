create table if not exists analysis_launches (
    launch_id uuid primary key references launches (id) on delete cascade,
    project_id uuid not null references projects (id) on delete cascade,
    state text not null default 'queued',
    reason text not null default 'finished',
    due_at timestamptz not null default now(),
    attempts integer not null default 0,
    lease_owner text,
    lease_until timestamptz,
    steps jsonb not null default '{}',
    error text,
    queued_at timestamptz not null default now(),
    started_at timestamptz,
    finished_at timestamptz,
    constraint analysis_launches_state_check check (state in ('queued', 'running', 'done', 'failed')),
    constraint analysis_launches_reason_check check (reason in ('finished', 'recompute', 'backfill'))
);
create index if not exists analysis_launches_due on analysis_launches (due_at) where state in ('queued', 'running');
create index if not exists analysis_launches_project on analysis_launches (project_id, finished_at desc);
create table if not exists analysis_backtests (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    source text not null,
    params jsonb not null default '{}',
    report jsonb not null,
    decisions integer not null,
    created_by text not null,
    created_at timestamptz not null default now(),
    constraint analysis_backtests_source_check check (source in ('project', 'synthetic', 'events'))
);
create index if not exists analysis_backtests_project on analysis_backtests (project_id, created_at desc);
