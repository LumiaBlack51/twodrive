# TwoDrive

TwoDrive is an experimental Linux OneDrive Files-On-Demand client for GNOME, written in Rust.
It provides a read/write FUSE mount, on-demand downloads, local caching, Microsoft Graph sync,
Nautilus status emblems and actions, a tray status helper, and power-aware background operation.

> [中文指南](#中文指南) | [English guide](#english-guide)

## 中文指南

### 功能与安全边界

- 默认挂载到 `~/TwoDrive/OneDrive`，目录项先显示，文件在打开时才下载。
- 支持本地新建、修改、移动和删除；默认并发上传 4 个文件，失败任务持久化等待重试。
- 支持“始终保留在此设备上”和“释放空间”。本地新增文件可继承固定目录策略；云端新发现的
  文件默认保持仅云端，不会自动下载。
- 大于 10 MiB 的文件使用 Microsoft Graph upload session 分片上传，并持久化会话以便重启后
  从服务端确认的偏移继续。
- FUSE 挂载会报告缓存磁盘的真实可用空间，支持文件管理器和解压工具的容量预检。
- ETag 冲突时保留两份：云端新版本保留原名，本地修改保存为 `TwoDrive conflict` 副本。
- TwoDrive 是 public desktop client，使用 OAuth2 Authorization Code + PKCE，不需要也不应创建 `client_secret`。
- OAuth token 存放在 `~/.config/twodrive/tokens.json`，权限为 `0600`。当前尚未接入 Secret Service。

TwoDrive 仍是实验性软件。重要文件应另有备份；OneDrive 不保存完整的 POSIX uid/gid/
mode 和目录时间戳语义。

### 1. 安装 Release 中的 Deb

从 [Releases](../../releases/latest) 下载 `twodrive_0.2.2-2_amd64.deb`，然后执行：

```bash
cd ~/Downloads
sudo apt install ./twodrive_0.2.2-2_amd64.deb
```

该包适用于 amd64 的 Ubuntu/Zorin OS GNOME 环境，并会安装 CLI、daemon、托盘辅助程序、设置程序、
systemd 用户服务和 Nautilus 扩展。

### 2. 创建 Azure App Registration

1. 在 Microsoft Entra 管理中心创建 App Registration。
2. 账户类型选择“任何组织目录中的账户和个人 Microsoft 账户”。
3. 在 Authentication 中添加“移动和桌面应用程序”重定向 URI：`http://localhost:53682`。
4. 添加 Microsoft Graph 委托权限：`Files.ReadWrite`、`User.Read`、`offline_access`、`openid`、`profile`。
5. 记录 Application (client) ID。不要创建 client secret。

### 3. 配置与登录

先生成本地配置：

```bash
twodrive status
```

编辑 `~/.config/twodrive/config.toml`，至少替换 `client_id`：

```toml
[graph]
client_id = "YOUR_AZURE_APP_CLIENT_ID"
tenant = "common"
redirect_uri = "http://localhost:53682"
scopes = ["Files.ReadWrite", "User.Read", "offline_access", "openid", "profile"]
```

这是本地配置，不要将其或 `tokens.json` 提交到仓库。然后登录：

```bash
twodrive login
```

浏览器授权成功后，启用后台挂载：

```bash
systemctl --user enable --now twodrive-daemon.service
nautilus -q
```

验证：

```bash
twodrive status
systemctl --user status twodrive-daemon.service
mountpoint ~/TwoDrive/OneDrive
```

### 4. 日常使用

```bash
# 只同步云端元数据，不主动下载文件内容
twodrive sync

# 始终保留文件或目录
twodrive pin /Documents/report.pdf
twodrive pin /Courses

# 取消该项目的显式固定策略
twodrive unpin /Documents/report.pdf

# 仅删除可释放的本地缓存，不删除云端文件
twodrive release /Documents/report.pdf

# 按保留期限清理未固定缓存
twodrive cache prune
```

在 Nautilus 中也可使用右键菜单“Release space”和“Always keep on this device”。直接删除
`~/TwoDrive/OneDrive` 中的文件会同时删除云端文件；“释放空间”只删除本地缓存，两者语义不同。

### 5. 路径和故障排查

- 挂载：`~/TwoDrive/OneDrive`
- 配置：`~/.config/twodrive/config.toml`
- Token：`~/.config/twodrive/tokens.json`
- 数据库：`~/.local/share/twodrive/twodrive.sqlite3`
- 缓存：`~/.local/share/twodrive/cache`

```bash
journalctl --user -u twodrive-daemon.service -f
systemctl --user restart twodrive-daemon.service
fusermount3 -u ~/TwoDrive/OneDrive
```

登录失败时，优先检查 Azure 中的平台类型、redirect URI、账户类型和 Graph 委托权限是否与本地
配置完全一致。

### 6. 卸载

```bash
systemctl --user disable --now twodrive-daemon.service
sudo apt remove twodrive
```

卸载软件包不会自动删除你的 token、数据库和缓存。

## English Guide

### Features and safety boundaries

- Mounts at `~/TwoDrive/OneDrive` by default. Metadata is shown first; file content is downloaded
  when an application opens the file.
- Supports local create, edit, move, and delete with four concurrent uploads by default. Failed jobs
  remain in durable retry queues.
- Supports Always Keep and Release Space. Locally created descendants can inherit a pinned directory
  policy; newly discovered cloud files stay online-only until opened or explicitly pinned.
- Files larger than 10 MiB use Microsoft Graph upload sessions. Session URLs and source identity are
  persisted so restart recovery resumes from the offset confirmed by the service.
- The FUSE mount reports real backing-store capacity for file-manager and archive-tool preflight checks.
- ETag conflicts preserve both versions: the cloud winner keeps the original name and the local edit
  is uploaded as a stable `TwoDrive conflict` copy.
- TwoDrive is a public desktop client using OAuth2 Authorization Code + PKCE. Do not create a
  `client_secret`.
- OAuth tokens are stored in `~/.config/twodrive/tokens.json` with mode `0600`. Secret Service is not
  integrated yet.

TwoDrive is still experimental. Keep independent backups of important data. OneDrive does not
preserve full POSIX uid/gid/mode or directory timestamp semantics.

### 1. Install the Deb release

Download `twodrive_0.2.2-2_amd64.deb` from [Releases](../../releases/latest), then run:

```bash
cd ~/Downloads
sudo apt install ./twodrive_0.2.2-2_amd64.deb
```

The package targets amd64 Ubuntu/Zorin OS GNOME systems and includes the CLI, daemon, tray helper,
settings app, systemd user unit, Nautilus extension, and emblems.

### 2. Create an Azure App Registration

1. Create an App Registration in the Microsoft Entra admin center.
2. Select accounts in any organizational directory and personal Microsoft accounts.
3. Under Authentication, add the Mobile and desktop applications redirect URI
   `http://localhost:53682`.
4. Add delegated Microsoft Graph permissions: `Files.ReadWrite`, `User.Read`, `offline_access`,
   `openid`, and `profile`.
5. Record the Application (client) ID. Do not create a client secret.

### 3. Configure and sign in

Generate the local configuration:

```bash
twodrive status
```

Edit `~/.config/twodrive/config.toml` and replace at least the client ID:

```toml
[graph]
client_id = "YOUR_AZURE_APP_CLIENT_ID"
tenant = "common"
redirect_uri = "http://localhost:53682"
scopes = ["Files.ReadWrite", "User.Read", "offline_access", "openid", "profile"]
```

This is local configuration. Never commit it or `tokens.json`. Sign in and start the daemon:

```bash
twodrive login
systemctl --user enable --now twodrive-daemon.service
nautilus -q
```

Verify the installation:

```bash
twodrive status
systemctl --user status twodrive-daemon.service
mountpoint ~/TwoDrive/OneDrive
```

### 4. Daily use

```bash
# Metadata-only sync; does not proactively download file contents
twodrive sync

# Always keep a file or directory locally
twodrive pin /Documents/report.pdf
twodrive pin /Courses

# Remove this item's explicit pin policy
twodrive unpin /Documents/report.pdf

# Delete releasable local cache only; does not delete the cloud file
twodrive release /Documents/report.pdf

# Prune old unpinned cache according to config retention
twodrive cache prune
```

Nautilus also provides Release space and Always keep on this device actions. Deleting a file inside
`~/TwoDrive/OneDrive` deletes both its local view and cloud item. Release space only removes eligible
local cache; these actions intentionally have different semantics.

### 5. Paths and troubleshooting

- Mount: `~/TwoDrive/OneDrive`
- Config: `~/.config/twodrive/config.toml`
- Token: `~/.config/twodrive/tokens.json`
- Database: `~/.local/share/twodrive/twodrive.sqlite3`
- Cache: `~/.local/share/twodrive/cache`

```bash
journalctl --user -u twodrive-daemon.service -f
systemctl --user restart twodrive-daemon.service
fusermount3 -u ~/TwoDrive/OneDrive
```

For login failures, first verify that the Azure platform type, redirect URI, supported account type,
and delegated Graph permissions exactly match the local configuration.

### 6. Uninstall

```bash
systemctl --user disable --now twodrive-daemon.service
sudo apt remove twodrive
```

Removing the package does not automatically delete your token, database, or cache.

## Build From Source

Install build/runtime prerequisites and build the workspace:

```bash
sudo apt install cargo rustc fuse3 sqlite3 python3-gi gir1.2-gtk-3.0 \
  gir1.2-gtk-4.0 gir1.2-ayatanaappindicator3-0.1 python3-nautilus
cargo build --workspace
```

Run the full validation gates:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Build the Deb package with `scripts/build-deb.sh`. For isolated local testing, use the mock backend
with temporary `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `TWODRIVE_MOUNT_DIR` values so the real token,
database, cache, and mount remain untouched.

## Architecture and recovery notes

- `twodrive-core`: configuration, SQLite metadata/state, pin policy, and durable recovery records.
- `twodrive-backend`: Microsoft Graph, OAuth2 PKCE, delta, download, upload sessions, and retry logic.
- `twodrive-fs`: FUSE operations, hydration, durable writes, upload recovery, and POSIX compatibility.
- `twodrive-cli`: user commands and desktop integration helpers.
- `twodrive-daemon`: background mount, known-folder upload-only watcher, and power policy.

`write`, `flush`, and `fsync` persist data to the local cache before success. A separate `writing`
state prevents a crash from uploading a half-written generation. Closed `dirty` or `uploading`
records replay concurrently after restart, and large uploads resume their persisted Graph session.
Known-folder uploads use a durable per-file queue, startup scan, and periodic rescan. Failed remote
deletes remain in a pending-delete queue. Delta metadata cannot overwrite a changing local generation
at the same path.

## License

[MIT](LICENSE)
