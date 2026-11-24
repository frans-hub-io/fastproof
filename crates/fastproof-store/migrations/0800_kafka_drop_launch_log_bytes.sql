set local lock_timeout = '5s';

alter table launches drop column if exists log_bytes;
