# PR Security Checks

Apply these checks proportionally — not every PR touches security-sensitive code.

## Input Handling
- [ ] User input validated at entry point (type, length, format, range)
- [ ] No raw user input in SQL queries, shell commands, file paths, templates
- [ ] No user input reflected in HTML without encoding (XSS)
- [ ] URL parameters and headers treated as untrusted
- [ ] Request body size limits enforced on new endpoints
- [ ] File uploads validated (type, size, content — not just extension)

## Authentication & Authorization
- [ ] New endpoints require authentication (unless intentionally public)
- [ ] Authorization checked — user can only access their own data
- [ ] No IDOR — object IDs validated against user's permissions
- [ ] Admin-only operations restricted appropriately
- [ ] Token/session handling follows existing patterns

## Data Protection
- [ ] No secrets hardcoded (API keys, passwords, tokens)
- [ ] No sensitive data in logs (passwords, tokens, PII)
- [ ] No sensitive data in error messages returned to client
- [ ] No sensitive data in URL query parameters
- [ ] New config values with secrets use environment variables

## Dependency Changes
- [ ] New dependencies checked for known CVEs
- [ ] New dependencies from trusted sources
- [ ] Dependency changes are intentional (not accidental lockfile churn)

## API Changes
- [ ] New fields don't expose internal data unintentionally
- [ ] Response doesn't leak other users' data
- [ ] Error responses are generic (no stack traces, internal paths)
- [ ] CORS not loosened without justification
