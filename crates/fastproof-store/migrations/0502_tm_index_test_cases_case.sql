-- no-transaction
create index concurrently if not exists test_cases_case on test_cases (case_id) where case_id is not null;
