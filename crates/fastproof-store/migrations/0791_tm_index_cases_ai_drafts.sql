-- no-transaction
create index concurrently if not exists cases_ai_drafts on cases (project_id, created_by) where ai_unreviewed;
