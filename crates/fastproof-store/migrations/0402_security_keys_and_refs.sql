create table if not exists deleted_project_keys (
    key text primary key,
    project_id uuid not null,
    deleted_at timestamptz not null default now()
);

create table if not exists attachment_refs (
    id uuid primary key,
    attachment_id uuid not null references attachments (id) on delete cascade,
    launch_id uuid not null references launches (id) on delete cascade,
    content_type text not null,
    original_name text,
    created_at timestamptz not null default now()
);
create index if not exists attachment_refs_attachment on attachment_refs (attachment_id);
create index if not exists attachment_refs_launch on attachment_refs (launch_id);
create unique index if not exists attachment_refs_variant on attachment_refs (attachment_id, launch_id, content_type, coalesce(original_name, ''));
