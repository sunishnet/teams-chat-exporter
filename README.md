# Teams Chat Exporter

A native Windows app for exporting Microsoft Teams 1:1, group, and meeting chats to readable HTML transcripts, Excel-friendly CSV files, and raw JSON. Optional pasted images are saved alongside each export.

The app is built with Rust, egui, and Microsoft Graph. It loads chats progressively, supports searching by chat name, participant, or email, and selects exact email matches for export.

> Vibe coded by [Sunish Issac](https://sunish.net) with Codex.

## What it exports

Each selected chat gets its own timestamped folder containing:

| File | Contents |
| --- | --- |
| `*.html` | Transcript with sender, timestamp, replies, reactions, attachments, edited/deleted markers, and system events |
| `*.csv` | One row per message, suitable for Excel |
| `*.json` | Raw Microsoft Graph message data |
| `images/` | Downloaded pasted images, when enabled |

The transcript identifies the signed-in user and the other chat participants when their names and email addresses are available. Chat messages are exported in the order returned by Microsoft Graph, sorted by creation time for the transcript.

## Get the app

Download the latest date-versioned `teams-chat-exporter-YYYYMMDD.exe` from the repository's **Releases** page. The first prepared release uses version `20261106` (Cargo package version `2026.11.6`). The app currently targets 64-bit Windows 10 and 11. No Rust installation is needed to run a released executable.

The executable includes the project icon and Windows version metadata, but is not code-signed. Windows may show a SmartScreen warning. If you prefer, build it from source using the instructions below.

## Use it

1. Launch the versioned executable, such as `bin\teams-chat-exporter-20261106.exe`, and choose **Sign in with Microsoft**.
2. Your browser opens for Microsoft sign-in. Complete the prompt and return to the app.
3. Chats appear as Graph pages load. Filter by chat type or search by name, person, or email.
4. An exact email query selects chats that include that participant. Review the selection, then choose **Export**.
5. Choose an output folder and whether to download images or include system events.
6. When export finishes, use **View** to open a transcript or **Folder** to open its export folder.

When local chat caching is enabled, the last saved chat list appears after sign-in while the app refreshes it from Graph. Sign-in is still required each time because authentication tokens are not saved.

Use **Connection** to choose a tenant or client ID. The default tenant is `organizations`. For a specific organization, enter its tenant domain or ID.

## Permissions and sign-in

The app requests delegated Microsoft Graph permissions `Chat.Read` and `User.ReadBasic.All`, plus standard OpenID sign-in scopes. It acts as the signed-in user and reads chats available to that account. Your organization may require administrator consent.

The default client ID is Microsoft's public client ID for Microsoft Graph Command Line Tools. Consent to one application is not guaranteed to apply to another application or to be approved by every organization. For a public release, users and organizations can configure their own app registration:

1. Register a public client application in Microsoft Entra ID.
2. Add a **Mobile and desktop applications** redirect URI of `http://localhost`.
3. Add delegated Microsoft Graph permissions `Chat.Read` and `User.ReadBasic.All`.
4. Complete any consent process required by the organization.
5. Enter the app's client ID in **Connection**.

Authentication uses OAuth 2.0 authorization code flow with PKCE and a temporary localhost redirect listener. Access and refresh tokens are kept in memory and are not persisted by the app.

## Privacy

- The app has no analytics or telemetry code.
- Microsoft sign-in and chat access communicate with Microsoft identity and Graph endpoints.
- Exported messages and images are written to the folder you choose.
- The app stores preferences such as tenant, client ID, output folder, and export options in local application storage. It does not store sign-in tokens there.
- Optional chat caching stores only chat IDs, names, participant email addresses, and last activity, scoped to the signed-in account and tenant. It does not cache messages, attachments, or tokens. Disable or clear it under **Local chat cache & privacy**.
- Treat exported files as sensitive conversation data. Apply your organization's retention and sharing rules.

See [SECURITY.md](SECURITY.md) for reporting security issues.

## Build from source

### Requirements

- Windows 10 or 11, 64-bit
- Rust stable with the MSVC toolchain (Rust 1.95 or newer)
- Visual Studio 2022 Build Tools with **Desktop development with C++** and the Windows SDK
- Network access to download Rust crates on the first build

Install the prerequisites using the official Rust and Visual Studio installers. Then open PowerShell in the repository and run:

```powershell
rustup default stable-msvc
cargo build --release
```

The executable is created at `target\release\teams-chat-exporter.exe`. The build script copies the dated executable and a latest-name copy into the bin folder. Run:

```powershell
.\build.ps1
```

If the window is blank in a VM or remote desktop session, try the OpenGL renderer:

```powershell
.\target\release\teams-chat-exporter.exe --opengl
```

### Checks

```powershell
cargo fmt -- --check
cargo check
cargo test
```

The tests include unit coverage and an export flow against a local mock Graph server. They do not access a real Teams account.

## Project structure

```text
src/main.rs    Desktop UI, settings, worker threads, and export selection
src/auth.rs    OAuth authorization code flow with PKCE and token refresh
src/graph.rs   Microsoft Graph HTTP client, pagination, retry, and downloads
src/model.rs   Chat and participant models, chat list loading
src/export.rs  HTML, CSV, JSON, and image export
src/util.rs    Text, date, HTML, and filename helpers
```

## Contributing

Bug reports, fixes, and feature ideas are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening an issue or pull request. Please do not include real chat exports, access tokens, or private tenant details in reports.

## License

This project is licensed under the [MIT License](LICENSE).
