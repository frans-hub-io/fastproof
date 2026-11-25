alter table saved_queries add column if not exists owner_key text not null default '';
update saved_queries set owner_key = 'user:' || owner where owner_key = '';
alter table saved_queries drop constraint if exists saved_queries_name_unique;
alter table saved_queries add constraint saved_queries_name_unique unique (project_id, owner_key, entity, name);
