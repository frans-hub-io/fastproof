create table if not exists launch_exports (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    launch_id uuid not null,
    format text not null,
    query text not null default '',
    status text not null default 'queued',
    total bigint not null default 0,
    done bigint not null default 0,
    blob_key text,
    size_bytes bigint,
    error text,
    created_by text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    finished_at timestamptz,
    constraint launch_exports_format_check check (format in ('html', 'xlsx')),
    constraint launch_exports_status_check check (status in ('queued', 'running', 'done', 'failed', 'expired'))
);
create index if not exists launch_exports_project on launch_exports (project_id, created_at);
create index if not exists launch_exports_active on launch_exports (status, updated_at) where status in ('queued', 'running');
