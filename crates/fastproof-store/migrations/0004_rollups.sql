create table if not exists series_daily (
    project_id uuid not null references projects (id) on delete cascade,
    series_id uuid not null,
    day date not null,
    branch text not null default '',
    env_key text not null default '',
    runs integer not null default 0,
    passed integer not null default 0,
    failed integer not null default 0,
    broken integer not null default 0,
    skipped integer not null default 0,
    unknown integer not null default 0,
    flaky integer not null default 0,
    duration_sum_ms bigint not null default 0,
    duration_min_ms bigint,
    duration_max_ms bigint,
    duration_buckets integer[] not null default '{}',
    updated_at timestamptz not null default now(),
    primary key (project_id, series_id, day, branch, env_key)
);
create index if not exists series_daily_series on series_daily (series_id, day);

create table if not exists series_stats (
    series_id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    last_statuses text not null default '',
    status_bits bigint not null default 0,
    runs_tracked integer not null default 0,
    flaky_score real,
    last_status text,
    last_passed_at timestamptz,
    last_failed_at timestamptz,
    first_seen_at timestamptz,
    last_seen_at timestamptz,
    updated_at timestamptz not null default now()
);
create index if not exists series_stats_project on series_stats (project_id, flaky_score);
