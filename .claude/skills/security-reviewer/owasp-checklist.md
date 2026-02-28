# OWASP Top 10 (2021) Mapped Checklist

## A01:2021 — Broken Access Control

- [ ] Authorization checked on every endpoint (not just UI hiding)
- [ ] Server-side enforcement, not client-side only
- [ ] Default deny — access blocked unless explicitly granted
- [ ] No IDOR — object references validated against user's permissions
- [ ] CORS properly configured (no wildcard with credentials)
- [ ] No directory traversal via URL manipulation
- [ ] Rate limiting on privileged operations
- [ ] JWT/token claims validated server-side on every request

## A02:2021 — Cryptographic Failures

- [ ] No sensitive data transmitted in cleartext
- [ ] Strong algorithms used (AES-256, SHA-256+, bcrypt/scrypt/argon2)
- [ ] No deprecated algorithms (MD5, SHA1, DES, RC4)
- [ ] Keys not hardcoded in source
- [ ] Proper key management (rotation, storage)
- [ ] TLS enforced for all external communication
- [ ] Passwords hashed with salt using adaptive algorithms
- [ ] crypto/rand used for security-sensitive randomness (not math/rand)

## A03:2021 — Injection

- [ ] Parameterized queries for all database access
- [ ] No string concatenation for queries with user input
- [ ] Command arguments passed as arrays, not shell strings
- [ ] File paths sanitized (no path traversal)
- [ ] User input not passed to eval/exec/template engines raw
- [ ] Log messages sanitize user input (no log injection)
- [ ] HTTP headers sanitize user input (no header injection)

## A04:2021 — Insecure Design

- [ ] Threat modeling performed for critical flows
- [ ] Business logic validated server-side
- [ ] Rate limiting on resource-intensive operations
- [ ] Abuse cases considered (what if user sends 10M requests?)
- [ ] Security requirements defined for each feature
- [ ] Defense in depth (multiple layers of security)

## A05:2021 — Security Misconfiguration

- [ ] No default credentials in production
- [ ] Debug mode disabled in production
- [ ] Error messages don't expose stack traces or internals
- [ ] Security headers present (HSTS, CSP, X-Frame-Options, X-Content-Type-Options)
- [ ] Unnecessary features/services disabled
- [ ] Cloud/infrastructure permissions follow least privilege
- [ ] CORS not overly permissive

## A06:2021 — Vulnerable and Outdated Components

- [ ] All dependencies at latest stable versions
- [ ] No dependencies with known CVEs
- [ ] Dependency sources are trusted
- [ ] Unused dependencies removed
- [ ] Automated vulnerability scanning configured
- [ ] License compliance checked

## A07:2021 — Identification and Authentication Failures

- [ ] Multi-factor authentication where appropriate
- [ ] No default/weak passwords
- [ ] Brute force protection (rate limiting, lockout)
- [ ] Session IDs not in URLs
- [ ] Sessions invalidated on logout
- [ ] Session timeout implemented
- [ ] Password strength requirements enforced
- [ ] Credential recovery is secure

## A08:2021 — Software and Data Integrity Failures

- [ ] CI/CD pipeline integrity protected
- [ ] Dependencies verified (checksums, signatures)
- [ ] Deserialization input validated
- [ ] No unsigned/unverified updates
- [ ] Critical data has integrity verification

## A09:2021 — Security Logging and Monitoring Failures

- [ ] Authentication events logged (success and failure)
- [ ] Authorization failures logged
- [ ] Input validation failures logged
- [ ] Logs include sufficient context (who, what, when, where)
- [ ] Logs don't contain sensitive data (passwords, tokens, PII)
- [ ] Log injection prevented
- [ ] Alerting configured for suspicious patterns

## A10:2021 — Server-Side Request Forgery (SSRF)

- [ ] URL inputs validated against allowlist
- [ ] No fetching arbitrary user-supplied URLs
- [ ] Internal network access blocked from user-controlled requests
- [ ] DNS rebinding protections where applicable
- [ ] Response from fetched URLs not returned raw to user
