create table if not exists projects (
    id uuid primary key,
    key text not null unique,
    name text not null,
    description text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    launch_seq bigint not null default 0,
    launch_count bigint not null default 0,
    last_launch_at timestamptz,
    settings jsonb not null default '{}'
);

create table if not exists launches (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    number bigint not null,
    external_id text,
    name text not null,
    status text not null default 'running',
    debug boolean not null default false,
    created_at timestamptz not null default now(),
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    updated_at timestamptz not null default now(),
    ci jsonb,
    vcs jsonb,
    environment jsonb,
    attributes jsonb not null default '[]',
    branch text,
    env_key text,
    created_by text,
    attempt_seq bigint not null default 0,
    first_result_at timestamptz,
    last_result_at timestamptz,
    count_total integer not null default 0,
    count_passed integer not null default 0,
    count_failed integer not null default 0,
    count_broken integer not null default 0,
    count_skipped integer not null default 0,
    count_unknown integer not null default 0,
    count_flaky integer not null default 0,
    count_muted integer not null default 0,
    constraint launches_status_check check (status in ('running', 'finished', 'interrupted')),
    constraint launches_number_unique unique (project_id, number),
    constraint launches_external_unique unique (project_id, external_id)
);
create index if not exists launches_project_created on launches (project_id, created_at);
create index if not exists launches_project_branch on launches (project_id, branch, number);

create table if not exists uploads (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    launch_id uuid not null references launches (id) on delete cascade,
    format text not null default 'auto',
    file_name text,
    status text not null default 'queued',
    size_bytes bigint not null default 0,
    sha256 text,
    blob_key text,
    counts jsonb not null default '{}',
    warnings jsonb not null default '[]',
    error jsonb,
    globals jsonb,
    created_by text,
    created_at timestamptz not null default now(),
    started_at timestamptz,
    finished_at timestamptz,
    constraint uploads_status_check check (status in ('queued', 'processing', 'done', 'failed'))
);
create index if not exists uploads_launch on uploads (launch_id, created_at);

create table if not exists ingest_jobs (
    id uuid primary key,
    upload_id uuid not null unique references uploads (id) on delete cascade,
    status text not null default 'queued',
    attempts integer not null default 0,
    max_attempts integer not null default 3,
    run_after timestamptz not null default now(),
    locked_by text,
    locked_until timestamptz,
    heartbeat_at timestamptz,
    last_error text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint ingest_jobs_status_check check (status in ('queued', 'running', 'done', 'failed'))
);
create index if not exists ingest_jobs_ready on ingest_jobs (run_after, created_at) where status = 'queued';
create index if not exists ingest_jobs_running on ingest_jobs (locked_until) where status = 'running';

create table if not exists test_cases (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    test_case_hash text not null,
    full_name text not null,
    display_name text not null,
    source_file text,
    source_line integer,
    code_ref text,
    explicit_id text,
    explicit_id_source text,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    constraint test_cases_hash_unique unique (project_id, test_case_hash)
);

create table if not exists series (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    test_case_id uuid not null references test_cases (id) on delete cascade,
    series_key text not null,
    parameters_hash text not null,
    env_key text,
    parameters jsonb not null default '[]',
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    constraint series_key_unique unique (project_id, series_key)
);
create index if not exists series_test_case on series (test_case_id);

create table if not exists results (
    id uuid not null,
    launch_created_at timestamptz not null,
    project_id uuid not null,
    launch_id uuid not null references launches (id) on delete cascade,
    test_case_id uuid not null,
    series_id uuid not null,
    name text not null,
    full_name text not null,
    suite_path text[] not null default '{}',
    status text not null,
    flaky_in_run boolean not null default false,
    attempt_count integer not null default 1,
    started_at timestamptz,
    stopped_at timestamptz,
    duration_ms bigint,
    message text,
    labels jsonb not null default '[]',
    fingerprint text,
    muted boolean not null default false,
    defect jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (id, launch_created_at),
    constraint results_series_unique unique (launch_id, series_id, launch_created_at)
) partition by range (launch_created_at);
create index if not exists results_launch_name on results (launch_id, full_name, id);
create index if not exists results_series on results (series_id, launch_created_at);
create index if not exists results_test_case on results (test_case_id, launch_created_at);
create index if not exists results_message_fts on results using gin (to_tsvector('simple', coalesce(message, '')));
create table if not exists results_default partition of results default;

create table if not exists attempts (
    id uuid not null,
    launch_created_at timestamptz not null,
    project_id uuid not null,
    launch_id uuid not null references launches (id) on delete cascade,
    result_id uuid not null,
    series_id uuid not null,
    upload_id uuid,
    client_key text not null,
    seq bigint not null,
    status text not null,
    raw_status text,
    started_at timestamptz,
    stopped_at timestamptz,
    duration_ms bigint,
    message text,
    trace text,
    source_uuid text,
    trace_id text,
    detail jsonb not null default '{}',
    steps_z bytea,
    logs_z bytea,
    created_at timestamptz not null default now(),
    primary key (id, launch_created_at),
    constraint attempts_client_unique unique (launch_id, client_key, launch_created_at)
) partition by range (launch_created_at);
create index if not exists attempts_result on attempts (result_id, launch_created_at);
create index if not exists attempts_series on attempts (launch_id, series_id);
create table if not exists attempts_default partition of attempts default;

create table if not exists launch_containers (
    launch_id uuid not null references launches (id) on delete cascade,
    uuid text not null,
    name text,
    children text[] not null default '{}',
    fixtures_z bytea not null,
    start_ms bigint,
    created_at timestamptz not null default now(),
    primary key (launch_id, uuid)
);
create index if not exists launch_containers_children on launch_containers using gin (children);

create table if not exists attachments (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    sha256 text not null,
    size bigint not null,
    content_type text not null,
    original_name text,
    blob_key text not null,
    created_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    constraint attachments_sha_unique unique (project_id, sha256)
);

create table if not exists attachment_links (
    attachment_id uuid not null references attachments (id) on delete cascade,
    launch_id uuid not null references launches (id) on delete cascade,
    primary key (attachment_id, launch_id)
);
create index if not exists attachment_links_launch on attachment_links (launch_id);

create table if not exists events (
    id bigint generated always as identity primary key,
    type text not null,
    project text,
    payload jsonb not null,
    created_at timestamptz not null default now()
);
create index if not exists events_created on events (created_at);

create table if not exists blob_deletions (
    id bigint generated always as identity primary key,
    prefix text not null,
    project_id uuid,
    requested_at timestamptz not null default now(),
    attempts integer not null default 0,
    last_error text
);
