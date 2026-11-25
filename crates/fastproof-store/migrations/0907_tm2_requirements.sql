alter table projects add column if not exists requirement_seq bigint not null default 0;
create table if not exists requirements (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    number bigint not null,
    title text not null,
    description text not null default '',
    type text not null default 'requirement',
    status text not null default 'active',
    parent_id uuid references requirements (id) on delete set null,
    path_ids uuid[] not null,
    depth smallint not null,
    tracker_id text,
    issue_key text,
    url text,
    external_state text,
    synced_at timestamptz,
    content_changed_at timestamptz not null default now(),
    origin text not null default 'ui',
    revision bigint not null default 1,
    created_by text,
    created_at timestamptz not null default now(),
    updated_by text,
    updated_at timestamptz not null default now(),
    archived_at timestamptz,
    constraint requirements_number_unique unique (project_id, number),
    constraint requirements_status_check check (status in ('draft', 'active', 'done', 'obsolete')),
    constraint requirements_origin_check check (origin in ('ui', 'import', 'tracker', 'ai'))
);
create index if not exists requirements_parent on requirements (project_id, parent_id, number) where archived_at is null;
create index if not exists requirements_path on requirements using gin (path_ids);
create unique index if not exists requirements_issue
    on requirements (project_id, coalesce(tracker_id, ''), issue_key) where issue_key is not null;
create table if not exists requirement_links (
    requirement_id uuid not null references requirements (id) on delete cascade,
    case_id uuid not null references cases (id) on delete cascade,
    project_id uuid not null,
    origin text not null,
    status text not null default 'active',
    created_by text not null,
    created_at timestamptz not null default now(),
    constraint requirement_links_origin_check check (origin in ('ui', 'code', 'import', 'ai')),
    constraint requirement_links_status_check check (status in ('active', 'proposed')),
    primary key (requirement_id, case_id)
);
create index if not exists requirement_links_case on requirement_links (case_id);
create index if not exists requirement_links_proposed on requirement_links (project_id, created_at) where status = 'proposed';
