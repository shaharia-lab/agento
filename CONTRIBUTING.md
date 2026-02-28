# Contributing to Agento

Thank you for your interest in contributing to Agento! We welcome contributions from everyone — whether you are a human developer or an AI agent.

Regardless of the source, all contributions go through the same review process and must meet the same quality standards. We maintain strict coding standards, automated checks, and thorough reviews to keep the codebase clean, reliable, and maintainable.

## Ways to Contribute

- **Report bugs** — Open a GitHub issue describing the problem, steps to reproduce, and expected behavior.
- **Request features** — Open a GitHub issue describing the feature, the motivation behind it, and any ideas for implementation.
- **Propose ideas** — Start a discussion via a GitHub issue to gather community feedback before diving into code.
- **Submit pull requests** — Fix bugs, implement features, improve documentation, or refactor code.

## Issues Before Pull Requests

**Every pull request must be linked to a GitHub issue.**

Opening an issue first gives the community the opportunity to discuss the problem or feature, provide feedback on the approach, and ensure visibility into the work being planned. Pull requests created without a corresponding issue may be closed.

1. Search existing issues to avoid duplicates.
2. Open a new issue if none exists.
3. Wait for acknowledgment or feedback before starting significant work.
4. Reference the issue in your pull request (e.g., `Fixes #42` or `Closes #42`).

## Development Setup

Before making code changes, read the developer documentation in the [`docs/`](docs/) directory:

- [Getting Started](docs/getting-started.md)
- [Development](docs/development.md)
- [Agents](docs/agents.md)
- [Integrations](docs/integrations.md)

### Prerequisites

- Go 1.25+
- Node.js and npm
- [pre-commit](https://pre-commit.com/)

### Install Pre-commit Hooks

We strongly encourage you to install pre-commit hooks during local development. They automatically catch many issues before you commit:

```bash
pre-commit install
```

The hooks enforce:

- Trailing whitespace and end-of-file fixes
- YAML/JSON validation
- No direct commits to `main`
- Go linting via `golangci-lint` (errcheck, govet, staticcheck, gosec, revive, and more)
- Frontend ESLint, Prettier formatting, and TypeScript type checking

### Running the Project

Two terminals are needed for development:

```bash
# Terminal 1 — Go API server on :8990
make dev-backend

# Terminal 2 — Vite dev server on :5173 (proxies API to :8990)
make dev-frontend
```

### Running Tests

```bash
make test          # Run all Go tests
make lint          # Run Go linters
cd frontend && npm run lint       # Frontend linting
cd frontend && npm run typecheck  # TypeScript checks
```

## Pull Request Guidelines

### Before Submitting

- [ ] Your PR is linked to a GitHub issue.
- [ ] You have read the relevant developer documentation in `docs/`.
- [ ] All pre-commit hooks pass locally.
- [ ] Tests pass (`make test`).
- [ ] Go linting passes (`make lint`).
- [ ] Frontend checks pass (lint, typecheck, format).
- [ ] You have checked whether your changes require a documentation update — if so, include the documentation changes in the same PR.

### Code Quality Standards

- Write clean, readable code that follows existing patterns in the codebase.
- Keep changes focused — one issue per pull request.
- Add tests for new functionality and bug fixes.
- Do not introduce security vulnerabilities (see OWASP top 10).
- Avoid over-engineering — solve the problem at hand without unnecessary abstractions.
- Respect the existing architecture and import rules (see `CLAUDE.md` for details).

### Documentation

Check whether your changes require documentation updates. This includes:

- Changes to API endpoints or behavior
- New features or configuration options
- Changes to the development setup or build process
- Architecture changes

Include documentation updates in the same pull request as the code changes.

### Commit Messages

Write clear, descriptive commit messages. Use the imperative mood (e.g., "Add pagination to list endpoints" not "Added pagination").

## For AI Agent Contributors

AI-generated contributions are welcome and go through the same process as human contributions:

1. An issue must exist before a pull request is created.
2. All automated checks (linting, tests, type checking) must pass.
3. Code must meet the same quality and security standards.
4. Pull requests are reviewed with the same rigor.

The `CLAUDE.md` file at the root of the repository contains project-specific instructions, architecture details, and conventions that AI agents should follow when contributing.

## License

By contributing to Agento, you agree that your contributions will be licensed under the [MIT License](LICENSE).
