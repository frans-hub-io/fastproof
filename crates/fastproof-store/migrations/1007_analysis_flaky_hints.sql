create table if not exists flaky_hints (
    series_id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    computed_at timestamptz not null default now(),
    window_runs integer not null,
    failures integer not null default 0,
    infra_failures integer not null default 0,
    backend text not null default 'postgres',
    hints jsonb not null default '[]',
    constraint flaky_hints_backend_check check (backend in ('postgres', 'clickhouse'))
);
create index if not exists flaky_hints_project on flaky_hints (project_id, computed_at desc);
