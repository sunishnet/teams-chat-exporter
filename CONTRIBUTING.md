# Contributing

Thanks for helping improve Teams Chat Exporter. Contributions can include bug reports, documentation improvements, fixes, and focused feature proposals.

## Before opening an issue

- Search existing issues for the same problem or request.
- Include the app version, Windows version, and steps to reproduce.
- Share relevant error text from the Activity log.
- Remove personal data before sharing logs, screenshots, or sample Graph responses.

Never attach real chat exports, access or refresh tokens, client secrets, or private tenant data.

## Pull requests

1. Create a branch for your change.
2. Keep the change focused and explain the user-visible behavior in the PR description.
3. Update documentation when commands, permissions, or behavior change.
4. Run the checks below on Windows before submitting.
5. Link the related issue, if there is one.

```powershell
cargo fmt -- --check
cargo check
cargo test
```

For UI changes, include a screenshot when practical. Do not include generated exports, build output, or personal data in commits.

## Project conventions

- Keep Microsoft Graph calls in `graph.rs` or the domain module that owns the operation.
- Keep tokens in memory; do not add token persistence or logging.
- Use delegated Graph permissions and request only what the feature needs.
- Preserve cancellation and progress reporting for long-running work.
- Prefer clear user-facing errors over panics for runtime conditions.

By submitting a contribution, you agree that it may be distributed under the project's MIT License.
