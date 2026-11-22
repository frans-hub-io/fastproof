alter table launches add column if not exists sync_blocked boolean not null default false;
alter table case_suggestions add column if not exists created_by text;
