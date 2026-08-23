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

- [Development](docs/development.md) — setup, running locally, tests, conventions, debugging
- [Architecture](docs/architecture.md) — stack, process model, the native backend, the Claude SDK
- [Releasing](docs/releasing.md) — cutting a release, the guards, the update manifest

[`CLAUDE.md`](CLAUDE.md) is the full working notes behind those guides: every
decision, with the reasoning and the failures behind it.

### Prerequisites

- Node.js 22+ and npm
- Rust (stable)
- The [Claude Code CLI](https://claude.ai/code), installed and signed in
- [pre-commit](https://pre-commit.com/)
- On Linux: `libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`

### Install Pre-commit Hooks

We strongly encourage you to install pre-commit hooks during local development:

```bash
pre-commit install
```

The hooks enforce trailing-whitespace and end-of-file fixes, YAML/JSON
validation, and no direct commits to `main`. The Rust and TypeScript gates run
in CI rather than here — `cargo clippy --all-targets` is a minute-plus and does
not belong on every commit.

One exclusion is load-bearing:
`src-tauri/src/native/notifications/email.html` is exempt from the whitespace
hooks. It is a frozen artifact, asserted byte for byte against
`parity/notification_template_golden.json`, and its trailing whitespace is the
spaces either side of six elided HTML comments. Nothing can regenerate it.

### Running the Project

From the repository root:

```bash
npm install
npm run app          # the desktop window, with hot reload
```

`npm run app` runs against `~/.agento-desktop-dev`, **not** your real
`~/.agento`. Use `npm run app:alongside` to run it beside an installed Agento.

### Running Tests

```bash
cd src-tauri
cargo test                              # unit and integration tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings

cd .. && npm run build                  # tsc --noEmit plus the Vite build
```

CI runs exactly those four. Several suites are `#[ignore]`d because they need a
real Claude corpus or the Claude Code CLI — see
[Development](docs/development.md#tests-that-need-something).

## Pull Request Guidelines

### Before Submitting

- [ ] Your PR is linked to a GitHub issue.
- [ ] You have read the relevant developer documentation in `docs/`.
- [ ] All pre-commit hooks pass locally.
- [ ] `cargo test` passes.
- [ ] `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` pass.
- [ ] `npm run build` passes (it typechecks).
- [ ] You have checked whether your changes require a documentation update — if so, include the documentation changes in the same PR.

### Code Quality Standards

- Write clean, readable code that follows existing patterns in the codebase.
- Keep changes focused — one issue per pull request.
- Add tests for new functionality and bug fixes.
- Do not introduce security vulnerabilities (see OWASP top 10).
- Avoid over-engineering — solve the problem at hand without unnecessary abstractions.
- Respect the existing architecture (see `CLAUDE.md` for details).
- `parity/` holds frozen goldens that specify Agento's wire format. Change one by deliberate edit with a reason, never by refreshing until green — read [`parity/README.md`](parity/README.md) first.

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
