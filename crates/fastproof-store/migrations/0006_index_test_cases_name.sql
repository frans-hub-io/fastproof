-- no-transaction
create index concurrently if not exists test_cases_project_name on test_cases (project_id, full_name);
