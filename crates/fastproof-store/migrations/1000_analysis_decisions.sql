create table if not exists triage_decisions (
    id bigint generated always as identity primary key,
    project_id uuid not null,
    launch_id uuid not null,
    launch_created_at timestamptz not null,
    result_id uuid not null,
    series_id uuid not null,
    fingerprint text,
    batch_id uuid not null,
    via text not null,
    source text not null,
    by text not null,
    at timestamptz not null default now(),
    before jsonb not null,
    after jsonb not null,
    suggestion_id uuid,
    reverts_batch uuid,
    folded_at timestamptz,
    constraint triage_decisions_via_check
        check (via in ('api', 'mcp', 'rp', 'rule', 'retro', 'suggestion', 'auto', 'undo')),
    constraint triage_decisions_source_check check (source in ('user', 'ai', 'rule', 'auto'))
);
create index if not exists triage_decisions_batch on triage_decisions (project_id, batch_id);
create index if not exists triage_decisions_result on triage_decisions (result_id, id);
create index if not exists triage_decisions_launch on triage_decisions (launch_id);
create index if not exists triage_decisions_project on triage_decisions (project_id, id);
create index if not exists triage_decisions_unfolded on triage_decisions (id)
    where folded_at is null and source = 'user';
