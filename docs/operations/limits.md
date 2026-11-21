# Limits

Every upload is parsed with size, depth and count limits, so a malformed or hostile report cannot exhaust
memory or disk. One byte budget per upload is charged for every expansion: each byte read from an archive
entry after decompression, inline attachment bodies and fixture copies. That also stops zip bombs and nested
archives early. Sizes declared in archive headers are never trusted.

When a limit is hit, the upload fails with an error that names the limit and its value, for example
`limit max_entry_bytes exceeded (max 536870912): allure-results/video.webm`. Truncation limits (logs) do not
fail the upload.

## Defaults

| Limit | Default | Applies to |
|---|---|---|
| `max_upload_bytes` | 1 GiB | Size of the upload as received (archive or single file); set with `FASTPROOF_MAX_UPLOAD_BYTES` |
| `max_archive_entries` | 100 000 | Entries in an archive or files in a directory |
| `max_total_uncompressed_bytes` | 4 GiB | Byte budget of one upload across all expansions |
| `max_entry_bytes` | 512 MiB | One archive entry after decompression |
| `max_document_bytes` | 64 MiB | One report document (JSON or XML) parsed in memory |
| `max_path_bytes` | 1024 | Length of an entry path inside an archive |
| `max_json_depth` | 128 | Nesting depth of JSON documents |
| `max_xml_depth` | 64 | Nesting depth of XML documents |
| `max_xml_attributes` | 64 | Attributes per XML element |
| `max_string_bytes` | 8 MiB | One JSON string or XML text or attribute value |
| `max_log_bytes` | 1 MiB | Kept size of stdout, stderr, message and trace per result; longer text is truncated |
| `max_steps_per_result` | 10 000 | Steps per result, counted across nesting, fixtures included |
| `max_attachments` | 50 000 | Attachment references resolved and stored per upload |
| `max_results` | 200 000 | Results per upload; each attempt counts |

## Other protections

- XML is parsed without DTDs and without external entities.
- Archives with entries that have absolute paths, `..` segments or NUL bytes, or that are symbolic or hard
  links, are rejected.
- Only the formats listed in [Report formats](../guide/formats.md) are parsed; other files are ignored and
  counted.
- Uploaded content is data: it is never executed on the server, HTML descriptions are sanitized, and
  attachments are served with a sandboxing Content Security Policy (see [Security](security.md)).

## Sizing hints

- The spool directory (`FASTPROOF_SPOOL_DIR`) holds uploads while they are received and processed. Plan for
  `max_upload_bytes` times the number of concurrent uploads per replica; the Helm chart sets a 4 GiB
  `emptyDir` by default.
- Large Playwright traces and videos dominate attachment storage. Retention per project removes launches
  together with the objects no other launch references.
