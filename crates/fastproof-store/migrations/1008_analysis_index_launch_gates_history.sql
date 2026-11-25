-- no-transaction
create index concurrently if not exists launch_gates_history
    on launch_gates (project_id, evaluated_at desc) where state = 'done';
