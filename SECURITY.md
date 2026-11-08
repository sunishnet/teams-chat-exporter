# Security policy

## Supported versions

Security fixes are made against the latest version on the default branch. When a release is published, use the latest release unless its notes say otherwise.

## Reporting a vulnerability

Please do not report vulnerabilities in public issues. Use GitHub's **Report a vulnerability** feature on the repository's Security tab, if enabled. If private vulnerability reporting is unavailable, contact the repository maintainer privately before sharing details publicly.

Include the affected version, impact, steps to reproduce, and any safe proof of concept. Do not include real chat content, personal data, access tokens, or refresh tokens.

The maintainer will acknowledge the report and coordinate a fix and disclosure timeline. Please allow time for users to update before publishing details.

## Security notes

- Sign-in uses OAuth authorization code flow with PKCE and a loopback redirect.
- Tokens are held in process memory and should never be included in diagnostics.
- Chat exports contain sensitive personal and organizational information. Store and share them accordingly.
- The released Windows executable is currently unsigned. Verify release checksums and/or build from source if required by your security process.
