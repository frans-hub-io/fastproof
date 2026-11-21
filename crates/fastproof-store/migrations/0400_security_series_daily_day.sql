-- no-transaction
create index concurrently if not exists series_daily_project_day on series_daily (project_id, day);
