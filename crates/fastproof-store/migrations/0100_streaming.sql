create table if not exists kafka_consumers (
    group_name text not null,
    member_id text not null,
    joined_at timestamptz not null default now(),
    heartbeat_at timestamptz not null default now(),
    primary key (group_name, member_id)
);

create table if not exists kafka_partition_leases (
    group_name text not null,
    topic text not null,
    partition integer not null,
    owner text,
    lease_until timestamptz,
    primary key (group_name, topic, partition)
);

create table if not exists kafka_offsets (
    group_name text not null,
    topic text not null,
    partition integer not null,
    next_offset bigint not null,
    high_watermark bigint,
    updated_at timestamptz not null default now(),
    primary key (group_name, topic, partition)
);

create table if not exists stream_keys (
    name text primary key,
    secret bytea not null,
    created_at timestamptz not null default now()
);

create table if not exists attempt_log_chunks (
    id bigint generated always as identity primary key,
    project_id uuid not null,
    launch_id uuid not null references launches (id) on delete cascade,
    result_key text not null,
    attempt integer not null,
    first_seq bigint not null,
    last_seq bigint not null,
    line_count integer not null,
    levels smallint not null default 0,
    bytes integer not null,
    first_ts timestamptz,
    last_ts timestamptz,
    lines_z bytea not null,
    created_at timestamptz not null default now()
);
create index if not exists attempt_log_chunks_launch on attempt_log_chunks (launch_id, id);
create index if not exists attempt_log_chunks_result on attempt_log_chunks (launch_id, result_key, attempt, first_seq);
create index if not exists attempt_log_chunks_project on attempt_log_chunks (project_id, created_at);

alter table launches add column if not exists log_bytes bigint not null default 0;
