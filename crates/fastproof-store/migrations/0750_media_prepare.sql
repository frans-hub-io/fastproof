alter table attachments add column if not exists media_state smallint not null default 0;

create table if not exists attachment_media (
    attachment_id uuid primary key references attachments (id) on delete cascade,
    project_id uuid not null,
    status text not null,
    attempts integer not null default 0,
    started_at timestamptz,
    finished_at timestamptz,
    format text,
    blob_key text,
    sha256 text,
    size bigint,
    error text,
    retry_at timestamptz,
    constraint attachment_media_status check (status in ('running', 'ready', 'native', 'skipped', 'failed'))
);
create index if not exists attachment_media_running on attachment_media (started_at) where status = 'running';
create index if not exists attachment_media_project on attachment_media (project_id, started_at);
