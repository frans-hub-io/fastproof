# Getting started

This page runs FastProof on your machine, creates a project and uploads a first report. It takes about ten
minutes. You need Docker (or Podman) and `openssl`.

FastProof always needs PostgreSQL (14 to 18; 18 is recommended). Attachments go to an S3-compatible bucket or,
for a single replica, to a directory.

## Create an admin token

FastProof never runs without authentication. For a first start, give it a static admin token. The token file
holds only the SHA-256 hash of the token, so the file itself is not a secret:

```sh
mkdir fastproof-demo && cd fastproof-demo
export FASTPROOF_TOKEN="$(openssl rand -hex 32)"
printf 'admin:admin:sha256:%s\n' "$(printf %s "$FASTPROOF_TOKEN" | sha256sum | cut -d' ' -f1)" > admin-tokens
chmod 0644 admin-tokens
echo "$FASTPROOF_TOKEN"
```

Keep the printed token: it is the `Authorization: Bearer` value for the API and the CLI. The line format is
`name:role:sha256:<hex>`; see [Security](../operations/security.md) for roles. After the first start you can
create managed tokens with an expiry and drop the static one.

## Option 1: Docker with a directory for attachments

The smallest setup: one PostgreSQL container and FastProof storing attachments in a volume.

```sh
docker network create fastproof

docker run -d --name fastproof-db --network fastproof \
  -e POSTGRES_USER=fastproof -e POSTGRES_PASSWORD=fastproof -e POSTGRES_DB=fastproof \
  -v fastproof-db:/var/lib/postgresql \
  docker.io/library/postgres:18.7-trixie

docker run -d --name fastproof --network fastproof \
  -p 127.0.0.1:8080:8080 -p 127.0.0.1:9090:9090 \
  -e FASTPROOF_DATABASE_URL=postgres://fastproof:fastproof@fastproof-db:5432/fastproof \
  -e FASTPROOF_BLOB_BACKEND=fs \
  -e FASTPROOF_ADMIN_TOKENS_FILE=/etc/fastproof/admin-tokens \
  -e FASTPROOF_LOG_FORMAT=text \
  -v "$PWD/admin-tokens:/etc/fastproof/admin-tokens:ro" \
  -v fastproof-blobs:/var/lib/fastproof \
  docker.io/franshub/fastproof:0.1.0-dev2
```

FastProof runs its database migrations at start (`FASTPROOF_MIGRATE=auto`, guarded by an advisory lock).
Check that it is ready:

```sh
curl -fsS http://localhost:9090/readyz
```

`/readyz` answers 200 once PostgreSQL is reachable, the schema is at the expected version and the blob store
is writable. Open <http://localhost:8080/> for the UI.

## Sign in to the UI

Without single sign-on, the UI opens a sign-in page: paste the admin token printed above under **Use an API
token**. The token stays in that browser tab only (session storage, gone when the tab closes) and is sent as
`Authorization: Bearer` with every API call; **Sign out** in the account menu forgets it. Later, create
personal tokens with an expiry under **API tokens**, or configure OIDC
([Configuration](../operations/configuration.md)) and the page offers **Sign in with SSO** as well.

While signed in with a token, the Playwright trace viewer and opening HTML or SVG attachments in a sandboxed
tab are not available (they need a browser session); download those files instead.

## Option 2: Docker Compose with PostgreSQL and Garage

[Garage](https://garagehq.deuxfleurs.fr/) is a small S3-compatible server. With it, attachments live in a
bucket exactly as they would in production. Download the two example files next to `admin-tokens`:

- [docker-compose.yaml](../examples/docker-compose.yaml)
- [garage.toml](../examples/garage.toml)

Create `.env` with the Garage secrets and start everything:

```sh
cat > .env <<EOF
GARAGE_RPC_SECRET=$(openssl rand -hex 32)
S3_ACCESS_KEY=GK$(openssl rand -hex 12)
S3_SECRET_KEY=$(openssl rand -hex 32)
EOF
docker compose up -d
docker compose logs -f fastproof
```

Garage creates the access key and the `fastproof` bucket on its first start (`--single-node
--default-bucket`). Garage is AGPL-3.0 licensed; FastProof only talks to it over the S3 API.

## Create a project

Projects are the unit of access control; launches and results belong to one project. The key is lowercase
letters, digits and dashes:

```sh
curl -fsS -X POST http://localhost:8080/api/v1/projects \
  -H "Authorization: Bearer $FASTPROOF_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"key":"web","name":"Web shop","description":"Browser and API tests of the shop"}'
```

## Upload the first report

The `fastproof` binary is also the CLI. Without installing anything, run it from the same image. Here it
uploads an `allure-results` directory from the current directory:

```sh
docker run --rm --network host \
  -e FASTPROOF_URL=http://localhost:8080 \
  -e FASTPROOF_TOKEN -e FASTPROOF_PROJECT=web \
  -v "$PWD:/work:ro" -w /work \
  docker.io/franshub/fastproof:0.1.0-dev2 upload allure-results
```

The CLI creates a launch, uploads the directory as one archive, waits until the server has processed it,
finishes the launch and prints a summary. With an external id (`FASTPROOF_LAUNCH`, `--launch`, or the id
derived in CI) the launch is shared by several commands instead: the upload joins it, returns as soon as the
upload is queued, and the launch stays open until `fastproof launch finish` (see
[CI recipes](ci-recipes.md)). `--finish` and `--no-finish` override the default.

Any report format works the same way: a `junit.xml` file, a CTRF `ctrf-report.json`, a Playwright
`results.json` or a directory of them. See [Report formats](formats.md).

!!! tip "No reports at hand?"
    Run any test suite with the Allure adapter of your framework (for example `allure-pytest` with
    `pytest --alluredir=allure-results`) or with a JUnit reporter, then upload the output.

## Install the CLI

The CLI is a static Linux binary inside the image. Copy it out once:

```sh
id=$(docker create docker.io/franshub/fastproof:0.1.0-dev2)
docker cp "$id:/usr/local/bin/fastproof" ./fastproof
docker rm "$id"
./fastproof version
```

Or add it to your CI image:

```dockerfile
COPY --from=docker.io/franshub/fastproof:0.1.0-dev2 /usr/local/bin/fastproof /usr/local/bin/fastproof
```

## Run the server without a container

The same binary runs the server with `fastproof serve` (`fastproof` without a command prints the help; the
container image adds `serve` itself). Point the attachment and spool directories at places the user can write
to, because the defaults are system paths (`/var/lib/fastproof/blobs`, `/tmp/fastproof`):

```sh
export FASTPROOF_DATABASE_URL=postgres://fastproof:fastproof@localhost:5432/fastproof
export FASTPROOF_ADMIN_TOKENS_FILE="$PWD/admin-tokens"
export FASTPROOF_BLOB_DIR="$PWD/data/blobs"
export FASTPROOF_SPOOL_DIR="$PWD/data/spool"
export FASTPROOF_LISTEN=127.0.0.1:8080 FASTPROOF_OPS_LISTEN=127.0.0.1:9090
export FASTPROOF_LOG_FORMAT=text
./fastproof serve
```

All settings are on [Configuration](../operations/configuration.md).

## Next steps

- [CI recipes](ci-recipes.md): upload from GitLab CI, GitHub Actions and Jenkins, live with `fastproof watch`.
- [Concepts](concepts.md): how launches, shards, retries and flaky tests work.
- [Helm chart](../operations/helm.md): run FastProof in Kubernetes.
- [Configuration](../operations/configuration.md): every setting.
