-- no-transaction
create index concurrently if not exists launches_merged_into on launches (merged_into) where merged_into is not null;
