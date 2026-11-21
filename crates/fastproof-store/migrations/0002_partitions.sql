create or replace function fastproof_ensure_partitions(months_back integer, months_ahead integer)
returns integer
language plpgsql
as $$
declare
    parent text;
    month_start date;
    part_name text;
    created integer := 0;
begin
    perform pg_advisory_xact_lock(7378823790196457536);
    foreach parent in array array['results', 'attempts'] loop
        for i in -months_back..months_ahead loop
            month_start := (date_trunc('month', now() at time zone 'UTC') + make_interval(months => i))::date;
            part_name := format('%s_p%s', parent, to_char(month_start, 'YYYYMM'));
            if to_regclass(part_name) is null then
                begin
                    execute format(
                        'create table %I partition of %I for values from (%L) to (%L)',
                        part_name,
                        parent,
                        month_start::text || ' 00:00:00+00',
                        (month_start + interval '1 month')::date::text || ' 00:00:00+00'
                    );
                    created := created + 1;
                exception when others then
                    raise warning 'creating partition % failed: %', part_name, sqlerrm;
                end;
            end if;
        end loop;
    end loop;
    return created;
end;
$$;

select fastproof_ensure_partitions(1, 3);
