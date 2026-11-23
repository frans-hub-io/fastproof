-- no-transaction
create index concurrently if not exists test_cases_project_first_seen on test_cases (project_id, first_seen_at);
