create table if not exists launch_reruns (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    source_launch_id uuid not null references launches (id) on delete cascade,
    external_id text not null,
    plan_id uuid,
    provider text not null,
    mode text not null,
    test_count integer not null default 0,
    status text not null default 'dispatching',
    error text,
    ci_ref text,
    requested_by text not null,
    created_at timestamptz not null default now(),
    dispatched_at timestamptz,
    constraint launch_reruns_status_check check (status in ('dispatching', 'dispatched', 'failed')),
    constraint launch_reruns_external_unique unique (project_id, external_id)
);
create index if not exists launch_reruns_source on launch_reruns (source_launch_id, created_at desc);
create index if not exists launch_reruns_project_created on launch_reruns (project_id, created_at);
