set local lock_timeout = '5s';

insert into launch_log_bytes (launch_id, bytes)
select id, log_bytes from launches where log_bytes > 0
on conflict (launch_id) do update set bytes = greatest(launch_log_bytes.bytes, excluded.bytes);
