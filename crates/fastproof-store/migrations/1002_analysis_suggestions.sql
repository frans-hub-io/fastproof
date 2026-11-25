create table if not exists triage_suggestions (
    id uuid primary key,
    project_id uuid not null,
    launch_id uuid not null,
    fingerprint text not null,
    rank smallint not null,
    band text not null,
    score real not null,
    defect_type_id uuid not null,
    grp text not null,
    comment text,
    issue_url text,
    issue_key text,
    why jsonb not null,
    source_ref jsonb not null default '{}',
    signal text not null,
    state text not null default 'open',
    created_at timestamptz not null default now(),
    decided_at timestamptz,
    decided_by text,
    reject_reason text,
    constraint triage_suggestions_band_check check (band in ('same', 'high', 'low')),
    constraint triage_suggestions_signal_check check (signal in ('memory', 'similar', 'rule', 'history', 'signal')),
    constraint triage_suggestions_state_check
        check (state in ('open', 'accepted', 'rejected', 'superseded', 'expired'))
);
create unique index if not exists triage_suggestions_open on triage_suggestions (launch_id, fingerprint, rank)
    where state = 'open';
create index if not exists triage_suggestions_launch on triage_suggestions (launch_id, fingerprint);
create index if not exists triage_suggestions_project on triage_suggestions (project_id, created_at);
