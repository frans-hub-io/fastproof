create table if not exists triage_memory (
    project_id uuid not null references projects (id) on delete cascade,
    fingerprint text not null,
    defect_type_id uuid not null,
    grp text not null,
    message_norm text not null default '',
    frames text[] not null default '{}',
    test_case_ids uuid[] not null default '{}',
    env_keys text[] not null default '{}',
    branches text[] not null default '{}',
    comment text,
    issue_url text,
    issue_key text,
    sample_result_id uuid,
    sample_launch_id uuid,
    confirmed integer not null default 0,
    overridden integer not null default 0,
    first_decided_at timestamptz not null,
    last_decided_at timestamptz not null,
    last_decided_by text not null,
    constraint triage_memory_group_check
        check (grp in ('productBug', 'automationBug', 'systemIssue', 'noDefect')),
    primary key (project_id, fingerprint, defect_type_id)
);
create index if not exists triage_memory_recent on triage_memory (project_id, last_decided_at desc);
create table if not exists triage_memory_events (
    id bigint generated always as identity primary key,
    project_id uuid not null references projects (id) on delete cascade,
    decided_at timestamptz not null,
    batch_id uuid not null,
    fingerprint text not null,
    defect_type_id uuid not null,
    grp text not null,
    previous_type_id uuid,
    message_norm text not null default '',
    frames text[] not null default '{}',
    test_case_id uuid,
    env_key text,
    branch text,
    results integer not null,
    by text not null,
    via text not null,
    suggested_type_id uuid,
    suggested_band text
);
create index if not exists triage_memory_events_project on triage_memory_events (project_id, decided_at, id);
