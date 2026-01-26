# kcidb-restd-rs

Simple REST receiver for KCIDB submissions and log uploads.

## Endpoints

### `POST /submit`
Accepts a JSON body and writes it to the spool directory as `submission-<id>.json`.
Authentication uses the JWT `origin` claim.

Example:
```bash
curl -X POST http://localhost:8080/submit \
  -H "Authorization: Bearer <JWT>" \
  -H "Content-Type: application/json" \
  --data-binary @submission.json
```

### `POST /submitlogs`
Accepts a gzipped log file via `multipart/form-data` and writes it to
`<spool>/logs/` with a filename:

`<origin>_<submission_id>_<filename>.log.gz`

The `origin` is taken from the JWT. The `filename` is taken from the uploaded
file's multipart filename and normalized to end with `.log.gz`.

Required multipart fields:
- `submission_id` (string, `^[a-z0-9_]+$`, max 64 chars)
- `log` (file upload; multipart filename is used)
Only one `log` file is accepted per request; additional files are ignored.
If the `log` part includes a `Content-Length` header, it is validated against the received bytes; it is strongly recommended to include it for uploads.

The response is JSON:
```json
{"log_url":"https://files.kernelci.org/<origin>/<submission_id>/<filename>.log.gz"}
```

Filename rules:
- The uploaded filename must match `^[a-z0-9_]+\.log\.gz$` (lowercase only).

The base URL can be overridden with:
```
KCIDB_STORAGE_URL="https://files-staging.kernelci.org/"
```

Example:
```bash
curl -X POST http://localhost:8080/submitlogs \
  -H "Authorization: Bearer <JWT>" \
  -F "submission_id=abcd1234" \
  -F "log=@./build.log.gz"
```

Example responses:
- Success (200):
```json
{"log_url":"https://files.kernelci.org/<origin>/abcd1234/build.log.gz"}
```
- Missing/invalid fields (400):
```json
{"id":"0","status":"error","message":"submission_id must match [a-z0-9_]+"}
```
- Unauthorized (401):
```json
{"id":"0","status":"error","message":"JWT is required"}
```
- Log already exists (409):
```json
{"id":"0","status":"error","message":"log file already exists"}
```

## Environment

- `JWT_SECRET`: JWT verification secret (required for auth unless empty).
- `KCIDB_STORAGE_URL`: Base URL used in `log_url` (default:
  `https://files.kernelci.org`).
