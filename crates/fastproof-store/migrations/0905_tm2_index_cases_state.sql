-- no-transaction
create index concurrently if not exists cases_state on cases (project_id, state) where state is not null;
