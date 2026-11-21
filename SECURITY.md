# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://github.com/frans-hub-io/fastproof/security/advisories/new) for this
repository. Do not open public issues, pull requests or discussions for security problems.

Include what you can of:

- the affected version or commit;
- a description of the problem and its impact;
- steps or a minimal report file that reproduces it;
- any suggested fix.

We acknowledge reports within a few working days, keep you informed while we work on a fix, and credit you in
the advisory unless you prefer otherwise. Please give us reasonable time to release a fix before disclosing.

## Supported versions

FastProof is in early development. Security fixes go into the latest release; there are no maintained older
release lines yet.

## Principles

- Every API and UI route requires authentication, except health, readiness and metrics on a separate port and
  the sign-in flow.
- Access is checked per project with explicit permissions; tokens are stored only as hashes.
- Uploaded reports are data: they are never executed, parsers have size, depth and count limits, and
  attachments are served in a sandbox.
- Secrets are read from files, never logged and never returned by the API.
- The container image is minimal and runs as a non-root user with a read-only root filesystem.

More in the [security documentation](https://fastproof.franshub.io/operations/security/).
