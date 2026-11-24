create table if not exists history_imports (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    source text not null,
    branch text,
    environment text,
    status text not null default 'queued',
    files jsonb not null default '[]',
    points_total bigint not null default 0,
    points_done bigint not null default 0,
    launches_created bigint not null default 0,
    results_imported bigint not null default 0,
    results_existing bigint not null default 0,
    results_unmatched bigint not null default 0,
    results_skipped bigint not null default 0,
    warnings jsonb not null default '[]',
    error text,
    created_by text not null,
    owner_key text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    finished_at timestamptz,
    constraint history_imports_source_check check (source in ('allure2', 'allure3')),
    constraint history_imports_status_check check (status in ('queued', 'running', 'done', 'failed'))
);
create index if not exists history_imports_project on history_imports (project_id, created_at);
create index if not exists history_imports_active on history_imports (status, updated_at) where status in ('queued', 'running');
