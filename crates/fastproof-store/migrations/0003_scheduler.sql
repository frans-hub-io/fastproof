create table if not exists scheduled_jobs (
    name text primary key,
    schedule text not null,
    next_run_at timestamptz not null default now(),
    lease_owner text,
    lease_until timestamptz,
    last_started_at timestamptz,
    last_finished_at timestamptz,
    last_status text,
    last_error text,
    last_duration_ms bigint,
    run_count bigint not null default 0,
    failure_count bigint not null default 0,
    updated_at timestamptz not null default now()
);
