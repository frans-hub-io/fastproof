alter table projects add column if not exists case_seq bigint not null default 0;

create table if not exists case_folders (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    parent_id uuid references case_folders (id) on delete cascade,
    name text not null,
    position integer not null default 0,
    path_ids uuid[] not null,
    depth smallint not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
create unique index if not exists case_folders_sibling_name
    on case_folders (project_id, coalesce(parent_id, '00000000-0000-0000-0000-000000000000'::uuid), lower(name));
create index if not exists case_folders_path on case_folders using gin (path_ids);
create index if not exists case_folders_parent on case_folders (parent_id);

create table if not exists case_field_defs (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    key text not null,
    name text not null,
    type text not null,
    options jsonb not null default '[]',
    required boolean not null default false,
    default_value jsonb,
    source_label text,
    built_in boolean not null default false,
    position integer not null default 0,
    archived_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint case_field_defs_type_check check (type in ('select', 'multiselect', 'text', 'number', 'date')),
    constraint case_field_defs_key_unique unique (project_id, key)
);

create table if not exists cases (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    number bigint not null,
    name text not null,
    status text not null default 'active',
    automated boolean not null default false,
    origin text not null,
    ai_unreviewed boolean not null default false,
    folder_id uuid references case_folders (id) on delete set null,
    body jsonb not null,
    fields jsonb not null default '{}',
    tags text[] not null default '{}',
    search_text text not null default '',
    code_hash text,
    revision bigint not null default 1,
    version integer not null default 1,
    last_trusted_at timestamptz,
    created_by text,
    created_at timestamptz not null default now(),
    updated_by text,
    updated_at timestamptz not null default now(),
    archived_at timestamptz,
    constraint cases_status_check check (status in ('draft', 'active', 'outdated')),
    constraint cases_origin_check check (origin in ('ui', 'code', 'import', 'ai')),
    constraint cases_number_unique unique (project_id, number)
);
create index if not exists cases_folder on cases (project_id, folder_id, number) where archived_at is null;
create index if not exists cases_fields on cases using gin (fields jsonb_path_ops);
create index if not exists cases_tags on cases using gin (tags);
create index if not exists cases_fts on cases using gin (to_tsvector('simple', search_text));
create index if not exists cases_updated on cases (project_id, updated_at desc);

create table if not exists case_versions (
    case_id uuid not null references cases (id) on delete cascade,
    version integer not null,
    project_id uuid not null,
    source text not null,
    launch_id uuid,
    changed text[] not null default '{}',
    snapshot_z bytea not null,
    created_by text not null,
    created_at timestamptz not null default now(),
    constraint case_versions_source_check check (source in ('ui', 'code', 'import', 'ai', 'restore', 'bulk')),
    primary key (case_id, version)
);

create table if not exists case_keys (
    project_id uuid not null references projects (id) on delete cascade,
    kind text not null,
    value text not null,
    case_id uuid not null references cases (id) on delete cascade,
    constraint case_keys_kind_check check (kind in ('allure_id', 'testrail', 'qase', 'testops', 'external')),
    primary key (project_id, kind, value)
);
create index if not exists case_keys_case on case_keys (case_id);

create table if not exists case_attachments (
    case_id uuid not null references cases (id) on delete cascade,
    attachment_id uuid not null references attachments (id) on delete restrict,
    name text not null,
    content_type text not null,
    created_by text,
    created_at timestamptz not null default now(),
    primary key (case_id, attachment_id)
);
create index if not exists case_attachments_attachment on case_attachments (attachment_id);

create table if not exists case_suggestions (
    id uuid primary key,
    project_id uuid not null,
    case_id uuid not null references cases (id) on delete cascade,
    base_revision bigint not null,
    proposal jsonb not null,
    rationale text not null default '',
    agent text not null,
    status text not null default 'pending',
    created_at timestamptz not null default now(),
    decided_by text,
    decided_at timestamptz,
    constraint case_suggestions_status_check check (status in ('pending', 'accepted', 'rejected', 'stale'))
);
create index if not exists case_suggestions_case on case_suggestions (case_id, created_at);

alter table test_cases add column if not exists case_id uuid;
alter table test_cases add column if not exists kind text not null default 'code';

do $$
begin
    if not exists (select 1 from pg_constraint where conname = 'test_cases_kind_check') then
        alter table test_cases add constraint test_cases_kind_check check (kind in ('code', 'manual')) not valid;
    end if;
    if not exists (select 1 from pg_constraint where conname = 'test_cases_case_fk') then
        alter table test_cases add constraint test_cases_case_fk
            foreign key (case_id) references cases (id) on delete set null not valid;
    end if;
end
$$;
