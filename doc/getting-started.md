# Install and sign in

[Home](../README.md) · [简体中文](getting-started.zh-CN.md) · [Everyday use](usage.md)

> [!WARNING]
> Start with expendable test files and independent backups. TwoDrive is experimental; deleting inside the mount also deletes the cloud item.

## 1. Install

The published `.deb` targets **amd64 Ubuntu/Zorin OS with GNOME**. It includes the CLI, daemon, user service, Nautilus extension, status emblems, GTK 3 tray, and GTK 4 Settings helper. FUSE is required; the extension targets Nautilus 4. Other distributions and desktop combinations are not covered by this package target.

Download the `.deb` and matching `.deb.sha256` from [Releases](https://github.com/LumiaBlack51/twodrive/releases/latest). In the download directory, verify and install. This example uses `0.2.8-1`; substitute the filenames you downloaded:

```bash
sha256sum -c twodrive_0.2.8-1_amd64.deb.sha256 && \
  sudo apt install ./twodrive_0.2.8-1_amd64.deb
```

Tray visibility depends on the desktop's AppIndicator/StatusNotifier support; its Python library alone does not guarantee GNOME Shell will display it. For source installation, see [CONTRIBUTING](../CONTRIBUTING.md#build-and-check).

## 2. Sign in

TwoDrive 0.2.8 and later include a shared public Microsoft application ID. **You do not need to register an application, edit configuration, or create a client secret.** Sign in with your own Microsoft account using OAuth 2.0 Authorization Code + PKCE:

```bash
twodrive login
```

Authorize in a browser on the same desktop, keeping the terminal open for the local callback. The CLI also prints the sign-in URL. Tokens are stored in `~/.config/twodrive/tokens.json` with mode `0600`: a plaintext fallback, not Secret Service storage. Never publish your tokens or local configuration.

## 3. Start the mount

```bash
systemctl --user enable --now twodrive-daemon.service
```

Close Nautilus windows, restart it to load the extension, and open the mount:

```bash
nautilus -q
twodrive open-folder
```

The default location is `~/TwoDrive/OneDrive`. Local metadata is mounted before the network refresh, so first-time browsing may initially show an empty folder. Refreshing metadata does not download the whole drive.

The tray has an autostart entry for subsequent desktop logins. Run `twodrive-tray` in a terminal to start it in this session. Open the read-only Settings window with `twodrive settings` or the tray's **Settings** item.

```bash
twodrive status
systemctl --user status twodrive-daemon.service
mountpoint ~/TwoDrive/OneDrive
```

A token path in `status` means a file exists, not that authentication was verified or all uploads finished. [Next: keep or release files →](usage.md)

## Optional: use your own app registration

Use this only if you need a separately managed application. Normal installation uses the built-in TwoDrive application. A client ID is public; tokens and client secrets are not.

1. In Microsoft Entra, create an **App registration**. Select accounts in any organizational directory and personal Microsoft accounts to match the `common` configuration below.
2. Under **Authentication**, add **Mobile and desktop applications** with redirect URI `http://localhost:53682`. Keep it identical in the local configuration.
3. Add Microsoft Graph **delegated** permissions corresponding to the scopes `Files.ReadWrite`, `User.Read`, `offline_access`, `openid`, and `profile`.
4. Copy the **Application (client) ID**. Organizational consent policies may require administrator approval.

Microsoft's [desktop application configuration guide](https://learn.microsoft.com/en-us/entra/identity-platform/scenario-desktop-app-configuration) explains public clients and loopback redirects. TwoDrive uses the account audience and fixed local port above; do not configure it as a confidential web application.

Run `twodrive status` to generate configuration, then edit the existing `[graph]` section in `~/.config/twodrive/config.toml`:

```toml
[graph]
client_id = "YOUR_AZURE_APP_CLIENT_ID"
tenant = "common"
redirect_uri = "http://localhost:53682"
scopes = ["Files.ReadWrite", "User.Read", "offline_access", "openid", "profile"]
```

Run `twodrive login` again after changing the application ID; existing tokens belong to the previous application. See [config.example.toml](../config.example.toml) for other settings.

## Upgrade and troubleshoot

Upgrade older packages to 0.2.8 or later for the built-in application. Missing, blank, and legacy placeholder client IDs are resolved to the built-in ID when configuration is loaded; the existing file is not rewritten. Custom client IDs and other Graph settings are preserved. A working custom registration does not need to be changed.

Work or school accounts may require administrator approval under their organization's consent policy. Ask the administrator to approve TwoDrive if prompted; registering another app is not the normal solution.

Close applications using the mount and inspect outstanding uploads before upgrading or restarting. After installing a new package:

```bash
systemctl --user restart twodrive-daemon.service
nautilus -q
```

**Previously installed from source?** A user service or `~/.local/bin` executable can take precedence over the package. Inspect:

```bash
systemctl --user cat twodrive-daemon.service
type -a twodrive twodrive-daemon
```

For a package installation, `systemctl --user edit twodrive-daemon.service` can explicitly select the packaged executable:

```ini
[Service]
ExecStart=
ExecStart=/usr/bin/twodrive-daemon
```

Run `systemctl --user daemon-reload`, then restart the service. Separately resolve stale CLI/helper binaries on your shell's `PATH`.

For login failures, check platform, account type, redirect URI, delegated permissions, consent policy, and port `53682`. For service failures:

```bash
journalctl --user -u twodrive-daemon.service -f
```

Do not delete the database/cache as a generic repair: it may contain the only copy of unsynced changes. See [paths and diagnostics](usage.md#paths-and-diagnostics).

## Uninstall

Close mounted files and inspect pending uploads before stopping the service:

```bash
systemctl --user disable --now twodrive-daemon.service
sudo apt remove twodrive
```

Quit any remaining tray process separately. Package removal leaves tokens, metadata, and cache intact; do not delete that data until unsynced content is recovered. Source installations use the [separate uninstall script](../scripts/uninstall-desktop-integration.sh).
