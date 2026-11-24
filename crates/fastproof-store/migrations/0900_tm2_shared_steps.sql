alter table projects add column if not exists shared_step_seq bigint not null default 0;
create table if not exists shared_steps (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    number bigint not null,
    name text not null,
    description text not null default '',
    steps jsonb not null,
    version integer not null default 1,
    approved_version integer,
    review_state text,
    search_text text not null default '',
    revision bigint not null default 1,
    origin text not null default 'ui',
    ai_unreviewed boolean not null default false,
    created_by text,
    created_at timestamptz not null default now(),
    updated_by text,
    updated_at timestamptz not null default now(),
    archived_at timestamptz,
    constraint shared_steps_number_unique unique (project_id, number),
    constraint shared_steps_origin_check check (origin in ('ui', 'import', 'ai')),
    constraint shared_steps_review_state_check check (review_state in ('pending', 'changes_requested', 'approved'))
);
create index if not exists shared_steps_list on shared_steps (project_id, number) where archived_at is null;
create index if not exists shared_steps_fts on shared_steps using gin (to_tsvector('simple', search_text));
create index if not exists shared_steps_review_queue on shared_steps (project_id, review_state, updated_at desc)
    where review_state in ('pending', 'changes_requested');
create table if not exists shared_step_versions (
    shared_step_id uuid not null references shared_steps (id) on delete cascade,
    version integer not null,
    project_id uuid not null,
    source text not null,
    changed text[] not null default '{}',
    snapshot_z bytea not null,
    created_by text not null,
    created_at timestamptz not null default now(),
    constraint shared_step_versions_source_check check (source in ('ui', 'import', 'ai', 'restore', 'bulk')),
    primary key (shared_step_id, version)
);
create table if not exists shared_step_usages (
    shared_step_id uuid not null references shared_steps (id) on delete restrict,
    case_id uuid not null references cases (id) on delete cascade,
    project_id uuid not null,
    primary key (shared_step_id, case_id)
);
create index if not exists shared_step_usages_case on shared_step_usages (case_id);
create table if not exists shared_step_keys (
    project_id uuid not null references projects (id) on delete cascade,
    kind text not null,
    value text not null,
    shared_step_id uuid not null references shared_steps (id) on delete cascade,
    constraint shared_step_keys_kind_check check (kind in ('testops', 'qase', 'testrail', 'external')),
    primary key (project_id, kind, value)
);
create index if not exists shared_step_keys_step on shared_step_keys (shared_step_id);
alter table run_items add column if not exists shared_pins jsonb;
alter table run_item_executions add column if not exists shared_pins jsonb;
