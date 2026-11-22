alter table test_plans add column if not exists kind text not null default 'adhoc';
alter table test_plans add column if not exists description text not null default '';
alter table test_plans add column if not exists filter jsonb;
alter table test_plans add column if not exists revision bigint not null default 1;
alter table test_plans add column if not exists updated_at timestamptz;
alter table test_plans add column if not exists updated_by text;
alter table test_plans add column if not exists archived_at timestamptz;

do $$
begin
    if not exists (select 1 from pg_constraint where conname = 'test_plans_kind_check') then
        alter table test_plans add constraint test_plans_kind_check check (kind in ('adhoc', 'managed'));
    end if;
end
$$;

create table if not exists test_plan_items (
    plan_id uuid not null references test_plans (id) on delete cascade,
    case_id uuid not null references cases (id) on delete cascade,
    position integer not null default 0,
    assignee text,
    via_filter boolean not null default false,
    primary key (plan_id, case_id)
);
create index if not exists test_plan_items_case on test_plan_items (case_id);

alter table launches add column if not exists test_plan_id uuid;
alter table launches add column if not exists manual_state text;
alter table launches add column if not exists count_untested integer not null default 0;
alter table launches add column if not exists count_in_progress integer not null default 0;
alter table launches add column if not exists created_by_can_sync boolean not null default false;

do $$
begin
    if not exists (select 1 from pg_constraint where conname = 'launches_manual_state_check') then
        alter table launches add constraint launches_manual_state_check check (manual_state in ('open', 'closed')) not valid;
    end if;
end
$$;

create table if not exists run_items (
    id uuid primary key,
    project_id uuid not null,
    launch_id uuid not null references launches (id) on delete cascade,
    launch_created_at timestamptz not null,
    case_id uuid not null references cases (id) on delete cascade,
    case_version integer not null,
    series_id uuid not null,
    status text not null default 'untested',
    execution_no integer not null default 1,
    assignee text,
    executed_by text,
    executed_kind text not null default 'user',
    comment text not null default '',
    defect jsonb,
    result_id uuid,
    revision bigint not null default 1,
    started_at timestamptz,
    finished_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint run_items_status_check check (status in ('untested', 'in_progress', 'passed', 'failed', 'blocked', 'skipped')),
    constraint run_items_executed_kind_check check (executed_kind in ('user', 'ai')),
    constraint run_items_case_unique unique (launch_id, case_id)
);
create index if not exists run_items_launch on run_items (launch_id, status);
create index if not exists run_items_queue on run_items (project_id, assignee, status) where status in ('untested', 'in_progress');
create index if not exists run_items_case on run_items (case_id, created_at);

create table if not exists run_step_results (
    run_item_id uuid not null references run_items (id) on delete cascade,
    step_path text not null,
    status text not null,
    actual text not null default '',
    comment text not null default '',
    attachment_ids uuid[] not null default '{}',
    updated_by text not null,
    updated_at timestamptz not null default now(),
    constraint run_step_results_status_check check (status in ('untested', 'passed', 'failed', 'blocked', 'skipped')),
    primary key (run_item_id, step_path)
);
create index if not exists run_step_results_attachments on run_step_results using gin (attachment_ids);

alter table results add column if not exists run_item_id uuid;

create table if not exists case_sync_queue (
    launch_id uuid primary key,
    queued_at timestamptz not null default now(),
    lease_owner text,
    lease_until timestamptz,
    attempts integer not null default 0,
    last_error text
);

create table if not exists tm_jobs (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    kind text not null,
    params jsonb not null,
    status text not null default 'queued',
    progress jsonb not null default '{}',
    report jsonb,
    blob_key text,
    created_by text,
    created_at timestamptz not null default now(),
    finished_at timestamptz,
    lease_owner text,
    lease_until timestamptz,
    constraint tm_jobs_kind_check check (kind in ('import', 'export', 'backfill')),
    constraint tm_jobs_status_check check (status in ('queued', 'dry_run', 'ready', 'running', 'done', 'failed', 'cancelled'))
);
create index if not exists tm_jobs_project on tm_jobs (project_id, created_at);
create index if not exists tm_jobs_active on tm_jobs (kind, status) where status in ('queued', 'running');
