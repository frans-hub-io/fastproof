alter table results add column if not exists label_values jsonb;
alter table results add column if not exists suite_tree text[];
alter table results add column if not exists suite_titled boolean;

create or replace function fastproof_label_values(labels jsonb) returns jsonb
language sql immutable parallel safe as $$
    select coalesce(jsonb_object_agg(n, v), '{}'::jsonb)
    from (
        select e->>'name' as n, jsonb_agg(coalesce(e->'value', '""'::jsonb) order by o) as v
        from jsonb_array_elements(case when jsonb_typeof(labels) = 'array' then labels else '[]'::jsonb end)
            with ordinality as x(e, o)
        where jsonb_typeof(e->'name') = 'string'
        group by 1
    ) s
$$;

create table if not exists explorer_gen (
    launch_id uuid primary key references launches (id) on delete cascade,
    gen bigint not null default 1
);

create table if not exists explorer_cache (
    launch_id uuid not null references launches (id) on delete cascade,
    key text not null,
    gen bigint not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    primary key (launch_id, key)
);
create index if not exists explorer_cache_created on explorer_cache (created_at);
