# Secure Configuration & Defaults Checklist

## HTTP Server

- [ ] Read/Write/Idle timeouts configured
- [ ] Max header size limited
- [ ] Request body size limited
- [ ] TLS configured with modern cipher suites (TLS 1.2+)
- [ ] HTTP/2 enabled where supported
- [ ] Graceful shutdown implemented

## Security Headers

- [ ] `Strict-Transport-Security` (HSTS) — enforces HTTPS
- [ ] `Content-Security-Policy` (CSP) — prevents XSS, injection
- [ ] `X-Content-Type-Options: nosniff` — prevents MIME sniffing
- [ ] `X-Frame-Options: DENY` or CSP frame-ancestors — prevents clickjacking
- [ ] `X-XSS-Protection: 0` — disable broken browser XSS filter
- [ ] `Referrer-Policy: strict-origin-when-cross-origin` — limits referrer leakage
- [ ] `Permissions-Policy` — restricts browser features

## CORS

- [ ] Origins explicitly allowlisted (no wildcard `*` with credentials)
- [ ] Methods restricted to what's needed
- [ ] Headers restricted to what's needed
- [ ] Credentials only allowed for trusted origins
- [ ] Preflight cache duration set appropriately

## Authentication

- [ ] Passwords hashed with bcrypt/scrypt/argon2 (cost factor appropriate)
- [ ] Session tokens are cryptographically random, sufficient length (32+ bytes)
- [ ] Session expires on idle and absolute timeout
- [ ] Session invalidated on logout (server-side)
- [ ] Failed login attempts rate-limited
- [ ] Account lockout after repeated failures
- [ ] Password reset tokens single-use, time-limited

## API Security

- [ ] Authentication required on all non-public endpoints
- [ ] Authorization checked at handler level (not just middleware)
- [ ] Input validation on all parameters (type, length, format, range)
- [ ] Output encoding appropriate for context (HTML, JSON, URL)
- [ ] Rate limiting on all public-facing endpoints
- [ ] Request size limits enforced
- [ ] Pagination enforced on list endpoints (no unbounded queries)

## Secrets Management

- [ ] No secrets in source code
- [ ] No secrets in environment variable defaults
- [ ] No secrets in log output
- [ ] No secrets in error messages
- [ ] Secrets loaded from environment or secure vault at runtime
- [ ] `.env` files in `.gitignore`
- [ ] Secrets rotated periodically
- [ ] Different secrets per environment (dev/staging/prod)

## File Operations

- [ ] File paths validated against base directory
- [ ] Symlinks resolved and validated
- [ ] File permissions set appropriately (not world-readable)
- [ ] Temporary files cleaned up
- [ ] Upload file types validated (not just extension)
- [ ] Upload file sizes limited
- [ ] Uploaded files stored outside webroot

## Database

- [ ] All queries parameterized (no string concatenation)
- [ ] Database user has minimal required permissions
- [ ] Connection strings not logged
- [ ] Connection pooling configured with limits
- [ ] Prepared statements used for repeated queries
- [ ] Migrations versioned and reversible

## Logging

- [ ] Authentication events logged
- [ ] Authorization failures logged
- [ ] Input validation failures logged
- [ ] Security-relevant changes logged (permission changes, config changes)
- [ ] Logs DO NOT contain: passwords, tokens, API keys, PII
- [ ] Log entries include: timestamp, user ID, action, source IP, result
- [ ] Log injection prevented (user input sanitized before logging)
- [ ] Structured logging format (JSON) for machine parsing

## Dependency Management

- [ ] Lock file committed (go.sum, package-lock.json)
- [ ] Dependencies from trusted registries only
- [ ] No dependencies with known critical/high CVEs
- [ ] Automated vulnerability scanning in CI
- [ ] Minimal dependency footprint (remove unused)
- [ ] Dependency updates reviewed before merging

## Error Handling

- [ ] Internal errors not exposed to users (generic messages)
- [ ] Error details logged server-side with context
- [ ] Stack traces never in production responses
- [ ] Error responses don't reveal: file paths, DB schema, internal IPs
- [ ] 404 vs 403 — consistent (don't reveal resource existence to unauthorized users)
- [ ] Panic recovery middleware in place
