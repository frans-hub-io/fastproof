set local lock_timeout = '5s';

alter table explorer_cache add column if not exists warmed boolean not null default false;
