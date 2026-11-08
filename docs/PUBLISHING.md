# Publishing checklist

This folder is prepared for GitHub. The repository links use `sunishnet/teams-chat-exporter`; the Buy Me a Coffee link is configured.

## Before the first public push

- [ ] Create a public GitHub repository named `teams-chat-exporter` (or update links in `docs/index.html`).
- [x] Configure the Buy Me a Coffee link in `.github/FUNDING.yml` and `docs/index.html`.
- [x] Set the MIT license copyright line to Sunish Issac.
- [ ] Register a dedicated public client app for this project and replace the default Microsoft Graph Command Line Tools client ID before release. Configure localhost redirect and the delegated Graph scopes; do not add a client secret to this desktop app. Test consent and sign-in with an account that has not previously consented.
- [ ] Confirm repository owner, README details, and supported Windows versions.
- [ ] Check that no executable, archive, chat export, token, secret, or tenant data is staged. `.gitignore` excludes local build output and common archives.
- [ ] Enable private vulnerability reporting in repository **Settings → Security** if you want to receive reports there.
- [ ] Enable GitHub Pages using **Settings → Pages → Deploy from a branch**, select `main` and `/docs`.

## Create the repository and push

After creating the empty repository on GitHub, run these commands from the project folder. Review `git status` before committing.

```powershell
git init -b main
git add .
git status --short
git commit -m "Prepare Teams Chat Exporter for open source"
git remote add origin https://github.com/sunishnet/teams-chat-exporter.git
git push -u origin main
```

Or install GitHub CLI and use `gh repo create` to create the remote from this folder.

## Publish a release

After CI passes on `main`, set Cargo's version to `YYYY.MM.DD` (for example, `2026.11.6`) and update `CHANGELOG.md`. Tag the release using the compact date form:

```powershell
git tag v20261106
git push origin v20261106
```

The Windows release workflow builds `bin/teams-chat-exporter-20261106.exe`, creates a SHA-256 checksum, and attaches both files to a GitHub Release. Use an eight-digit date tag such as `v20261107` for the next release. The binary's Windows version metadata comes from Cargo's version.

## Update links

GitHub Pages is optional; the repository and Releases page work without it.
