-- no-transaction
create index concurrently if not exists cases_last_status on cases (project_id, last_status)
    where archived_at is null and last_status is not null;
