create table if not exists direct_upload_installation (
    id boolean primary key default true check (id),
    installation text not null
);
create table if not exists direct_uploads (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    launch_id uuid not null,
    sha256 text not null,
    size bigint not null,
    content_type text not null,
    original_name text,
    status text not null default 'pending',
    created_by text not null,
    created_at timestamptz not null default now(),
    expires_at timestamptz not null,
    verify_started_at timestamptz,
    claim text,
    size_checked_at timestamptz,
    swept boolean not null default false,
    finished_at timestamptz,
    attachment_id uuid,
    error text,
    constraint direct_uploads_status check (status in ('pending', 'verifying', 'done', 'failed', 'expired')),
    constraint direct_uploads_size check (size >= 0)
);
create index if not exists direct_uploads_launch on direct_uploads (launch_id, status);
create index if not exists direct_uploads_open on direct_uploads (project_id, expires_at) where status in ('pending', 'verifying');
create index if not exists direct_uploads_unswept on direct_uploads (expires_at) where not swept;
create index if not exists direct_uploads_finished on direct_uploads (created_at) where swept;
