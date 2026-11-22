-- no-transaction
create index concurrently if not exists test_cases_unlinked on test_cases (project_id, last_seen_at) where case_id is null;
