-- no-transaction
create index concurrently if not exists attachments_media_pending on attachments (created_at)
    where media_state = 0 and content_type in ('video/webm', 'video/mp4', 'video/quicktime', 'video/x-matroska');
