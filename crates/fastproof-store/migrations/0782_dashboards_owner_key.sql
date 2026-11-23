alter table dashboards add column if not exists owner_key text not null default '';
update dashboards set owner_key = 'user:' || owner where owner_key = '';
