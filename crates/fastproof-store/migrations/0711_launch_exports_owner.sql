alter table launch_exports add column if not exists owner_key text not null default '';
create index if not exists launch_exports_owner on launch_exports (project_id, owner_key, created_at);
