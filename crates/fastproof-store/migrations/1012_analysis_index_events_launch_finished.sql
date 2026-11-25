-- no-transaction
create index concurrently if not exists events_launch_finished
    on events (id) where type = 'launch.finished';
