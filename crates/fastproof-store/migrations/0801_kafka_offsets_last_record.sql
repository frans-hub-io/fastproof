set local lock_timeout = '5s';

alter table kafka_offsets add column if not exists last_record_offset bigint;
alter table kafka_offsets add column if not exists last_record_ms bigint;
