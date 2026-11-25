create table if not exists attempt_signals (
    id bigint generated always as identity primary key,
    project_id uuid not null,
    launch_id uuid,
    launch_created_at timestamptz,
    result_id uuid,
    attempt integer,
    trace_id text,
    result_client_key text,
    source text not null,
    kind text not null,
    at timestamptz not null,
    confidence text not null,
    detail jsonb not null default '{}',
    dedup_key text not null,
    resolved boolean not null default false,
    created_at timestamptz not null default now(),
    constraint attempt_signals_source_check check (source in ('mock', 'grid', 'ci', 'k8s')),
    constraint attempt_signals_confidence_check check (confidence in ('exact', 'window')),
    constraint attempt_signals_dedup unique (project_id, dedup_key)
);
create index if not exists attempt_signals_result on attempt_signals (result_id) where result_id is not null;
create index if not exists attempt_signals_launch on attempt_signals (launch_id) where launch_id is not null;
create index if not exists attempt_signals_pending on attempt_signals (project_id, created_at) where not resolved;
