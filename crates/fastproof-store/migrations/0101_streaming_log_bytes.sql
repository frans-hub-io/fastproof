set local lock_timeout = '5s';

create table if not exists launch_log_bytes (
    launch_id uuid primary key references launches (id) on delete cascade,
    bytes bigint not null default 0
);
