set local lock_timeout = '5s';

alter table integration_cursors add column if not exists pending uuid[] not null default '{}';
