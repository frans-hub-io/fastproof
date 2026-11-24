-- no-transaction
create index concurrently if not exists launches_finished on launches (finished_at, id) where finished_at is not null;
