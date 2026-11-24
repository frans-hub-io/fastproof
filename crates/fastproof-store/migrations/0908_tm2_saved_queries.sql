alter table test_plans add column if not exists query text;
create table if not exists saved_queries (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    entity text not null,
    name text not null,
    query text not null,
    owner text not null,
    shared boolean not null default false,
    position integer not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint saved_queries_entity_check check (entity in ('cases', 'requirements')),
    constraint saved_queries_name_unique unique (project_id, owner, entity, name)
);
create index if not exists saved_queries_shared on saved_queries (project_id, entity, position) where shared;
