create table if not exists plan_dispatches (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    plan_id uuid not null,
    launch_id uuid not null references launches (id) on delete cascade,
    environment text,
    provider text not null,
    status text not null default 'dispatching',
    error text,
    ci_ref text,
    git_ref text not null,
    variables jsonb not null default '{}',
    requested_by text not null,
    created_at timestamptz not null default now(),
    dispatched_at timestamptz,
    constraint plan_dispatches_status_check check (status in ('dispatching', 'dispatched', 'failed'))
);
create index if not exists plan_dispatches_plan on plan_dispatches (plan_id, created_at desc);
create index if not exists plan_dispatches_project_created on plan_dispatches (project_id, created_at);
create index if not exists plan_dispatches_launch on plan_dispatches (launch_id);
