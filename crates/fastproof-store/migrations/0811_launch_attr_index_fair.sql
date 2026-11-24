drop index if exists launches_attrs_pending;
create index if not exists launches_attrs_pending_project on launches (project_id, created_at desc) where attrs_indexed_at is null and not debug;
