create table if not exists dashboards (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    name text not null,
    description text not null default '',
    owner text not null,
    locked boolean not null default false,
    revision bigint not null default 1,
    widgets jsonb not null default '[]',
    updated_by text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint dashboards_name_len check (char_length(name) between 1 and 120),
    constraint dashboards_widgets_array check (jsonb_typeof(widgets) = 'array')
);
create index if not exists dashboards_project on dashboards (project_id, created_at);
