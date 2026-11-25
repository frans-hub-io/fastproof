create table if not exists result_patterns (
    result_id uuid not null,
    launch_id uuid not null,
    launch_created_at timestamptz not null,
    project_id uuid not null,
    pattern text not null,
    rule_id uuid not null,
    attempt integer not null,
    line_seq bigint,
    excerpt text not null default '',
    created_at timestamptz not null default now(),
    primary key (result_id, pattern)
);
create index if not exists result_patterns_launch on result_patterns (launch_id, pattern);
create index if not exists result_patterns_project on result_patterns (project_id, pattern, created_at desc);
