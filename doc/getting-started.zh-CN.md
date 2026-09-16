# 安装与登录

[返回首页](../README.zh-CN.md) · [English](getting-started.md) · [日常使用](usage.zh-CN.md)

> [!WARNING]
> 请先用可丢弃的测试文件试用，并保留独立备份。TwoDrive 是实验性软件；在挂载目录内删除文件也会删除云端项目。

## 1. 安装

已发布的 `.deb` 面向 **amd64 架构的 Ubuntu/Zorin OS GNOME 环境**，包含 CLI、后台服务、用户服务单元、Nautilus 扩展、状态图标、GTK 3 托盘和 GTK 4 Settings 辅助程序，需要 FUSE；扩展面向 Nautilus 4。其他发行版和桌面组合不属于该软件包的目标环境。

从 [Releases](https://github.com/LumiaBlack51/twodrive/releases/latest) 下载 `.deb` 及配套 `.deb.sha256`，在下载目录内先校验再安装。以下以 `0.2.10-1` 为例，请替换为实际文件名：

```bash
sha256sum -c twodrive_0.2.10-1_amd64.deb.sha256 && \
  sudo apt install ./twodrive_0.2.10-1_amd64.deb
```

托盘能否显示取决于桌面是否支持 AppIndicator/StatusNotifier；仅安装相应 Python 库不保证 GNOME Shell 一定显示托盘。源码安装见[贡献指南（英文）](../CONTRIBUTING.md#build-and-check)。

## 2. 登录

TwoDrive 从 0.2.8 起内置公共 Microsoft 应用 ID。**无需自行注册应用、编辑配置或创建 client secret。** 使用自己的 Microsoft 账户，通过 OAuth 2.0 Authorization Code + PKCE 登录：

```bash
twodrive login
```

在同一台桌面电脑的浏览器中授权，保留终端等待本地回调。CLI 也会打印登录地址。令牌保存在 `~/.config/twodrive/tokens.json`，权限为 `0600`，属于明文后备存储，尚未接入 Secret Service。不要公开令牌或本地配置。

## 3. 启动挂载

```bash
systemctl --user enable --now twodrive-daemon.service
```

关闭 Nautilus 窗口，重启以加载扩展，再打开挂载目录：

```bash
nautilus -q
twodrive open-folder
```

默认位置是 `~/TwoDrive/OneDrive`。服务先挂载本地元数据再执行网络刷新，因此首次浏览可能暂时为空。元数据刷新不会下载整个云盘。

托盘带有桌面自动启动项，在后续登录时启动；当前会话可在终端执行 `twodrive-tray`。用 `twodrive settings` 或托盘的 **Settings** 打开只读信息窗口。

```bash
twodrive status
systemctl --user status twodrive-daemon.service
mountpoint ~/TwoDrive/OneDrive
```

`status` 中显示令牌路径，只表示文件存在，不代表登录已验证或全部上传完成。[下一步：固定或释放文件 →](usage.zh-CN.md)

## 可选：使用自己的应用注册

仅在需要独立管理应用时使用此配置。普通安装使用 TwoDrive 内置应用即可。client ID 是公开标识，令牌和 client secret 则不是。

1. 在 Microsoft Entra 创建 **App registration（应用注册）**，选择“任何组织目录中的账户和个人 Microsoft 账户”，与下文的 `common` 对应。
2. 在 **Authentication** 中添加 **Mobile and desktop applications（移动和桌面应用程序）**，重定向 URI 为 `http://localhost:53682`，必须与本地配置一致。
3. 添加 Microsoft Graph **委托权限**，对应请求范围：`Files.ReadWrite`、`User.Read`、`offline_access`、`openid`、`profile`。
4. 记录 **Application (client) ID**。组织账户可能受管理员同意策略限制。

Microsoft 的[桌面应用配置说明](https://learn.microsoft.com/en-us/entra/identity-platform/scenario-desktop-app-configuration)介绍了公共客户端和回环重定向。TwoDrive 使用上述账户范围和固定本地端口，不要配置成需要密钥的机密 Web 客户端。

运行 `twodrive status` 生成配置，然后编辑 `~/.config/twodrive/config.toml` 中已有的 `[graph]` 节，不要追加重复节：

```toml
[graph]
client_id = "YOUR_AZURE_APP_CLIENT_ID"
tenant = "common"
redirect_uri = "http://localhost:53682"
scopes = ["Files.ReadWrite", "User.Read", "offline_access", "openid", "profile"]
```

修改应用 ID 后重新运行 `twodrive login`，已有令牌属于之前的应用。其他设置参考 [config.example.toml](../config.example.toml)。

## 升级与排查

旧版软件包需升级至 0.2.8 或更新版本才能使用内置应用。读取配置时，缺失、空白和旧版占位 client ID 会自动使用内置 ID，不会重写已有配置文件。自定义 client ID 和其他 Graph 设置保持原值；已经正常使用自定义注册的用户无需切换。

工作或学校账户可能受组织同意策略限制。如果提示需要管理员批准，请让管理员批准 TwoDrive；通常无需另行注册应用。

升级或重启前，请关闭使用挂载文件的应用，并检查待完成上传。安装新软件包后：

```bash
systemctl --user restart twodrive-daemon.service
nautilus -q
```

**之前从源码安装过？** 用户级服务或 `~/.local/bin` 程序可能优先于软件包生效。先检查：

```bash
systemctl --user cat twodrive-daemon.service
type -a twodrive twodrive-daemon
```

使用 `.deb` 时，可通过 `systemctl --user edit twodrive-daemon.service` 明确指定软件包中的程序：

```ini
[Service]
ExecStart=
ExecStart=/usr/bin/twodrive-daemon
```

执行 `systemctl --user daemon-reload` 后重启服务。也应单独检查 shell 的 `PATH` 是否仍优先选中旧版 CLI 或辅助程序。

登录失败时，检查平台、账户类型、重定向 URI、委托权限、组织同意策略及端口 `53682`。服务问题可查看：

```bash
journalctl --user -u twodrive-daemon.service -f
```

不要把删除数据库或缓存当作通用修复，其中可能存有尚未同步的唯一副本。详见[路径与诊断](usage.zh-CN.md#路径与诊断)。

## 卸载

先关闭挂载文件、检查待上传内容，再停止服务：

```bash
systemctl --user disable --now twodrive-daemon.service
sudo apt remove twodrive
```

托盘仍在运行时需单独退出。卸载会保留令牌、数据库和缓存，未同步内容安全恢复前不要删除这些数据。源码安装使用[单独的卸载脚本](../scripts/uninstall-desktop-integration.sh)。
