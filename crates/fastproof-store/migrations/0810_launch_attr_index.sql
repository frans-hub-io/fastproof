alter table launches add column if not exists attrs_indexed_at timestamptz;
create index if not exists launches_attrs_pending on launches (created_at desc) where attrs_indexed_at is null and not debug;

create table if not exists launch_attr_index (
    project_id uuid not null references projects (id) on delete cascade,
    launch_id uuid not null references launches (id) on delete cascade,
    key text not null,
    value text not null,
    launch_created_at timestamptz not null,
    primary key (launch_id, key, value)
);
create index if not exists launch_attr_index_key on launch_attr_index (project_id, key, launch_created_at desc);
