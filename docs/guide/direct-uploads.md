# Direct uploads

Screenshots, videos and Playwright traces are often most of the bytes a CI job sends. With direct uploads the
CLI and the reporters put such files straight into the server's S3 bucket through short-lived pre-signed URLs,
so the bytes do not pass through the FastProof server on the way in. The server still checks every object
before it becomes an attachment, and the attachment is stored and served exactly like one uploaded through
the server: deduplicated by SHA-256 per project, served with byte ranges and media URLs, prepared as a
seekable video, collected when no launch uses it.

Direct uploads need the S3 backend. With the filesystem backend, when they are turned off, or when the CI
runner cannot reach the bucket, the clients send attachments through the server as before, without any
setting on their side.

## How it works

1. The client hashes the file and asks the server for an upload URL, declaring SHA-256, size, content type
   and name: `POST /api/v1/projects/{key}/launches/{id}/attachments/direct`. Content the project already
   stores and still uses (in a launch, a test case or a manual step) is linked to the launch at once
   (`exists`) and needs no upload.
2. The client PUTs the file to the returned URL with exactly the returned headers. The URL is valid for 15
   minutes by default and can write only one temporary object.
3. The client completes the upload: `POST .../attachments/direct/{uploadId}/complete`. The server copies the
   object to a location the URL cannot write, reads the copy back and compares size and SHA-256 with the
   declaration. Only a matching copy is stored as the attachment; anything else is deleted and the
   completion answers `422 verification_failed`.
4. The client references the returned attachment `id` in its results, as with `POST .../attachments`.

Unfinished uploads expire with their URL; the `direct_uploads.sweep` job deletes what they left in the
bucket.

## Server settings

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_DIRECT_UPLOADS` | `true` | Offer direct uploads (S3 backend only) |
| `FASTPROOF_S3_PUBLIC_ENDPOINT` | none | Endpoint written into upload URLs when CI reaches the bucket under another address than the server: an origin such as `https://s3.example.com`, without user, path or query |
| `FASTPROOF_DIRECT_UPLOAD_ALLOW_HTTP` | `false` | Hand out `http://` upload URLs; without it, direct uploads are offered only when the URLs use https |
| `FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS` | `900` | Lifetime of an upload URL, 60 to 21600 |
| `FASTPROOF_DIRECT_UPLOAD_MAX_BYTES` | `1073741824` | Largest attachment sent directly, at most 5 GiB (one PUT), and never more than `FASTPROOF_MAX_UPLOAD_BYTES` or the per-entry limit of the server path (512 MiB) |
| `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_LAUNCH` | `1000` | Uploads of one launch that are not completed, failed or expired yet |
| `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_PROJECT` | `10000` | The same per project |
| `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_BYTES_PER_PROJECT` | `107374182400` | Declared bytes of such open uploads per project |
| `FASTPROOF_DIRECT_UPLOAD_MAX_BYTES_PER_LAUNCH` | `53687091200` | Declared bytes of open and completed direct uploads of one launch |
| `FASTPROOF_DIRECT_UPLOAD_CHECKSUM` | `true` | Bind `x-amz-checksum-sha256` to the URL. Each replica checks once that the store refuses other content, and offers direct uploads only then. Turn it off for a store that rejects or ignores the header; lower open limits (100 per launch, 1000 and 10 GiB per project) then apply |
| `FASTPROOF_DIRECT_UPLOAD_VERIFY_CONCURRENCY` | `4` | Completions verified at once per replica (others wait up to 10 s, then get `503` with `Retry-After`) |
| `FASTPROOF_DIRECT_UPLOAD_VERIFY_TIMEOUT_SECONDS` | `600` | Time allowed for one verification |

The bucket needs nothing special for PUTs with pre-signed URLs. Browsers are not involved, so no CORS rules are
needed. Objects live under `direct/<installation>/` while they are uploaded and verified; each installation
has its own id there. With the Helm chart, direct uploads are on when `blob.directUploads.publicEndpoint` is set
(the bundled Garage exposed to CI, or the address CI uses for an external S3), or with
`blob.directUploads.enabled: true` for an external S3 endpoint that CI reaches as it is configured;
`enabled: false` turns them off, `allowHttp: true` permits an `http://` endpoint.

Each completion reads the object back once from the store, so the server still downloads what is uploaded
directly; what it saves is the upload from CI and the request body handling. Place replicas close to the
bucket.

## CLI

`fastproof upload <directory>` sends attachment files of 1 MiB and more directly when the server offers it, and
the archive carries a short reference in their place. Report files (`.json`, `.xml` and similar) always stay in
the archive, except Allure `*-attachment*` files.

| Option | Variable | Default |
|---|---|---|
| `--direct-uploads auto\|off` | `FASTPROOF_DIRECT_UPLOAD_MODE` | `auto` |
| `--direct-min-bytes N` | `FASTPROOF_DIRECT_UPLOAD_MIN_BYTES` | `1048576` |

Uploads of a single file or an archive and `fastproof watch` send everything through the server.

A reference in the archive is a file whose whole content is `fastproof-attachment-ref:v1:<attachment id>`
followed by a newline (at most 80 bytes). The server reads such a file in any uploaded report as a reference
to an attachment of the same launch, never as content; a reference to anything else is reported as an upload
warning and the result has no attachment for it.

## Reporters

The Playwright reporter and pytest-fastproof use direct uploads for attachments of 1 MiB and more. The same
variables `FASTPROOF_DIRECT_UPLOAD_MODE=off` and `FASTPROOF_DIRECT_UPLOAD_MIN_BYTES` (Playwright also takes the
options `directUploads` and `directUploadMinBytes`) change that. When the bucket does not answer from the CI
runner, the reporter prints one warning and sends the rest of the run's attachments through the server. The
wire contract for other reporters, including the Allure enhancers, is in `reporters/PROTOCOL.md` (section 5a).

## Responses

`POST .../attachments/direct` takes `{"items": [{"sha256", "size", "contentType", "name"}]}` (1 to 100 items)
and answers an item per request item, in order:

```json
{"items": [
  {"sha256": "9f86d0…", "status": "upload", "uploadId": "0193…", "method": "PUT",
   "url": "https://s3.example.com/bucket/direct/0193…/in?X-Amz-…",
   "headers": {"content-type": "application/octet-stream", "x-amz-checksum-sha256": "n4bQgY…"},
   "expiresAt": "2026-11-23T10:15:00Z"},
  {"sha256": "2c26b4…", "status": "exists", "id": "0193…"},
  {"sha256": "fcde2b…", "status": "refused", "code": "too_large", "limit": 1073741824}
]}
```

`refused` codes: `too_large`, `launch_bytes`, `project_bytes`, `launch_open`, `project_open`. A refused item, a `422` on
completion or a failed PUT means: send that attachment through `POST .../attachments`.

| Completion | Meaning |
|---|---|
| `201` | Stored: `{id, sha256, size}` |
| `200` | Completed before (the same body) |
| `409 not_uploaded` | The object is not in the store yet |
| `409 verification_in_progress` | Another call is verifying it; retry |
| `410 upload_expired` | The URL expired before completion |
| `422 verification_failed` | Size or SHA-256 differ from the declaration; the object was deleted |
| `501 direct_uploads_unavailable` | No S3 store, or direct uploads are off |

## Security

- The calls need `launches:write` in the project, like `POST .../attachments`; `exists` answers are given only
  to callers who may also read the launch.
- A URL allows PUTs of one temporary key until it expires. It cannot read, list or delete anything, and it
  cannot write an attachment: the server copies the object before checking it, so writing the URL again after
  completion changes nothing that is stored.
- The URL is bound to `content-type: application/octet-stream` and, by default, to the declared checksum.
- Clients never send the FastProof token to the store. Treat the URL as a secret until it expires: do not log
  it.
- Rejected objects are recorded in the audit log (`attachment.direct.rejected`), issued URLs per request
  (`attachment.direct.create`).
