-- no-transaction
create index concurrently if not exists test_cases_name_fts on test_cases using gin (to_tsvector('simple', full_name));
