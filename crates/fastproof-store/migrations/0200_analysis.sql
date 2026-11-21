alter table attempts add column if not exists fingerprint text;
alter table results add column if not exists transition text;
alter table results add column if not exists flaky boolean not null default false;
alter table results add column if not exists mute_until timestamptz;
alter table launches add column if not exists important boolean not null default false;
alter table launches add column if not exists rolled_up_at timestamptz;
alter table series_stats add column if not exists last_launch_id uuid;
alter table series_stats add column if not exists flaky boolean not null default false;
alter table series_stats add column if not exists last_flaky_at timestamptz;
alter table series_daily add column if not exists product_bug integer not null default 0;
alter table series_daily add column if not exists automation_bug integer not null default 0;
alter table series_daily add column if not exists system_issue integer not null default 0;
alter table series_daily add column if not exists no_defect integer not null default 0;
alter table series_daily add column if not exists to_investigate integer not null default 0;

create table if not exists defect_types (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    grp text not null,
    name text not null,
    short_name text not null default '',
    color text not null default '',
    built_in boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint defect_types_group_check check (grp in ('productBug', 'automationBug', 'systemIssue', 'noDefect', 'toInvestigate')),
    constraint defect_types_name_unique unique (project_id, name)
);
create unique index if not exists defect_types_builtin on defect_types (project_id, grp) where built_in;

insert into defect_types (id, project_id, grp, name, short_name, color, built_in)
select gen_random_uuid(), p.id, b.grp, b.name, b.short_name, b.color, true
from projects p
cross join (values
    ('productBug', 'Product bug', 'PB', '#e5484d'),
    ('automationBug', 'Automation bug', 'AB', '#f5a524'),
    ('systemIssue', 'System issue', 'SI', '#3e63dd'),
    ('noDefect', 'No defect', 'ND', '#8b8d98'),
    ('toInvestigate', 'To investigate', 'TI', '#a855f7')
) as b (grp, name, short_name, color)
on conflict do nothing;

create table if not exists rules (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    name text not null,
    enabled boolean not null default true,
    ord integer not null default 0,
    match jsonb not null default '{}',
    action jsonb not null default '{}',
    hits bigint not null default 0,
    last_hit_at timestamptz,
    created_by text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
create index if not exists rules_project on rules (project_id, ord, created_at);

create table if not exists series_mutes (
    series_id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    until timestamptz,
    reason text not null default '',
    created_by text,
    created_at timestamptz not null default now()
);
create index if not exists series_mutes_until on series_mutes (until) where until is not null;

create table if not exists failure_fingerprints (
    project_id uuid not null references projects (id) on delete cascade,
    fingerprint text not null,
    message text not null default '',
    first_seen_at timestamptz not null,
    first_launch_id uuid,
    last_seen_at timestamptz not null,
    last_launch_id uuid,
    occurrences bigint not null default 0,
    primary key (project_id, fingerprint)
);
create index if not exists failure_fingerprints_recent on failure_fingerprints (project_id, last_seen_at);

create table if not exists launch_daily (
    project_id uuid not null references projects (id) on delete cascade,
    day date not null,
    branch text not null default '',
    env_key text not null default '',
    launches integer not null default 0,
    primary key (project_id, day, branch, env_key)
);

create table if not exists test_plans (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    name text not null,
    from_launch_id uuid,
    statuses text[] not null default '{}',
    tests jsonb not null default '[]',
    test_count integer not null default 0,
    created_by text,
    created_at timestamptz not null default now()
);
create index if not exists test_plans_project on test_plans (project_id, created_at);

create table if not exists analysis_tasks (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    kind text not null,
    params jsonb not null default '{}',
    status text not null default 'queued',
    progress jsonb not null default '{}',
    error text,
    created_by text,
    created_at timestamptz not null default now(),
    started_at timestamptz,
    finished_at timestamptz,
    lease_owner text,
    lease_until timestamptz,
    constraint analysis_tasks_status_check check (status in ('queued', 'running', 'done', 'failed'))
);
create index if not exists analysis_tasks_open on analysis_tasks (created_at) where status in ('queued', 'running');

create table if not exists analysis_state (
    name text primary key,
    value jsonb not null default '{}',
    updated_at timestamptz not null default now()
);

create or replace function fastproof_int_array_add(a integer[], b integer[])
returns integer[]
language sql
immutable
as $$
    select coalesce(array_agg(coalesce(a[i], 0) + coalesce(b[i], 0) order by i), '{}')
    from generate_series(1, greatest(coalesce(array_length(a, 1), 0), coalesce(array_length(b, 1), 0))) as i
$$;
