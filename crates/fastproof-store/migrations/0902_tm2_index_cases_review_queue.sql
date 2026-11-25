-- no-transaction
create index concurrently if not exists cases_review_queue on cases (project_id, review_state, updated_at desc)
    where review_state in ('pending', 'changes_requested');
