# 日常使用与安全边界

[返回首页](../README.zh-CN.md) · [English](usage.md) · [安装指南](getting-started.zh-CN.md)

## 浏览与编辑

在 GNOME Files（Nautilus）中打开 `~/TwoDrive/OneDrive`。目录项来自 SQLite 元数据，普通浏览和 Nautilus/GIO 的文件类型探测不会预取内容；应用真正读取或复制仅云端文件时才会下载。未知格式在打开前可能显示通用图标。

可以通过挂载目录新建、编辑、重命名、移动和删除。修改先提交到本地，再由后台完成 Graph 操作。请观察[状态图标](../README.zh-CN.md#一眼看懂文件状态)，不要把编辑器提示保存成功当作云端同步完成。读取未缓存内容仍需要联网。

## 固定与释放内容

| Nautilus 操作 | 实际行为 |
| --- | --- |
| **Always keep on this device（始终保留）** | 设置显式固定策略，下载现有适用文件；文件夹递归处理，固定内容不受自动缓存清理影响。 |
| **Cancel always keep on this device（取消始终保留）** | 移除该项目的显式策略，保留缓存；继承的父目录策略仍可能生效。 |
| **Release space（释放空间）** | 取消所选项目的显式和继承固定策略（文件夹包括子项），然后安全释放本地缓存；不删除 OneDrive 项目，占位视图仍可见。 |
| **Sync now（立即同步）** | 刷新元数据，重试待处理操作和上传，下载待完成的固定文件；不是严格的“仅同步元数据”。 |
| **View status（查看状态）** | 查看所选项目的本地状态。 |
| **Copy path（复制路径）** | 复制绝对本地路径，多选时以换行分隔；TwoDrive 以外也可用，但不添加 shell 引号。 |

以下 CLI 命令使用**以 `/` 开头的云端相对路径**，不是用户主目录下的本地路径：

```bash
twodrive pin /Documents/report.pdf
twodrive pin /Courses
twodrive unpin /Documents/report.pdf
twodrive release /Documents/report.pdf
```

固定文件夹作用于现有子项。本地新建子项可以继承策略；但**新发现的云端文件仍保持仅云端**，即使它位于已固定目录之下。文件夹标记只是子项汇总，不保证所有文件均可离线使用。离线前应确认下载完成；`twodrive status-path /Documents/report.pdf` 可以检查有效固定状态。

**下载过程中：** 释放文件会取消下载并清理未完成片段，文件夹操作覆盖子项。旧句柄不会重启传输，主动重新打开文件可再次下载。阻塞中的网络请求需返回或超时后才能结束取消。

**写入或上传过程中：** 释放请求会持久化，显示“云朵＋同步”。上传成功且文件句柄关闭后才移除缓存；失败或冲突时保留数据。自动清理仍保护固定内容，选择“始终保留”会取消待执行的释放请求。

> [!WARNING]
> **删除与释放空间不同。** 在挂载目录内删除文件也会删除云端项目。仅为节省空间时应选择 Release space，而不是 Delete、`rm` 或手工删除缓存。

执行一轮同步/恢复，或按保留期限清理未固定缓存：

```bash
twodrive sync
twodrive cache prune
```

清理使用 `cache.retain_for`，默认 `30d`，保护固定或仍在处理的内容。托盘的 **Release unpinned cache** 调用按期限清理，并不立即清空全部未固定文件。

## 托盘与 Settings

[首页真实截图](../README.zh-CN.md)展示的是**空闲状态**，不是传输画面。有活动传输时，代码显示活动数量和最多五个上传/下载文件名，并在可用时显示字节进度。`Sync: idle` 只是活动快照，不保证所有排队操作都成功。

菜单提供打开目录、立即同步、缓存清理和 Settings 入口。**Quit tray** 只退出托盘，服务继续运行；**Quit tray and stop sync** 还会请求停止用户服务，停止前应关闭使用挂载文件的应用。

**Pause sync 目前只改变显示。** 它修改托盘文字和图标，不暂停服务或传输。真正停止需先关闭挂载文件，再执行 `systemctl --user stop twodrive-daemon.service`，用 `systemctl --user start twodrive-daemon.service` 恢复。停止服务也会使挂载不可用，不是保留挂载的离线暂停模式。

Settings 是**只读 GTK 4 信息窗口**，展示令牌文件状态、路径、实际缓存占用和配置值，不能编辑配置。“Pinned used”和“Recent errors”仍是占位项，不是实时统计或错误历史。显示令牌路径也不等于已验证登录有效。

## 当前限制与安全边界

TwoDrive 是实验性软件，不是备份系统。重要数据应另有独立副本。本地恢复和冲突处理只能降低特定风险，不构成不会丢失数据的保证。

发生 ETag 冲突时，TwoDrive 尝试让云端版本保留原名，将本地修改作为 `TwoDrive conflict` 副本上传，不会自动合并文档。已有云端同名目录需单独核对。不受 OneDrive 支持的文件名，例如含 `:` 或 `?`，可能阻塞上传但保留本地内容，应检查错误并适当重命名。

令牌是权限为 `0600` 的明文 JSON，尚未接入 Secret Service。不要公开令牌、OAuth 回调地址或上传会话 URL。`twodrive logout` 只删除本地令牌文件，应先停止服务，因为运行中的进程可能持有内存令牌。它不会撤销账户侧授权。

当前挂载在每轮恢复/元数据刷新间等待 60 秒，上传池使用 `ac_upload_concurrency`。展示的交流电/电池间隔字段**尚未实现按供电状态自适应调度**；`cache.max_size` 虽会保存和展示，但**不是实际执行的磁盘配额**。不要依赖这些字段限制流量、耗电或磁盘占用。

完整的 POSIX uid/gid/mode 和目录时间戳语义不会保留。当前客户端操作已登录用户的 `/me/drive`，不要据此假定支持多账户管理或任意 SharePoint 文档库。

## 可选的常用文件夹上传

常用文件夹处理**默认关闭**，独立于可读写挂载，支持 `upload_only`。配置的本地目录直接上传，不额外生成一份 TwoDrive 内容缓存。本地删除源文件不会删除云端副本，即使配置要求传播删除也会忽略。

启用前请阅读 [config.example.toml](../config.example.toml)。示例关闭启动扫描和重扫（`startup_scan = false`、`rescan_interval = "0s"`），自动生成的默认配置却分别为 `true` 和 `"15m"`。监视器先记录已有文件基线，再处理新增、移入/重命名和配置的扫描。它不是双向备份，也不保证立即上传全部旧文件。临时文件、隐藏文件和符号链接会跳过，上传成功前应保留源文件。

## 路径与诊断

| 用途 | 默认位置 |
| --- | --- |
| 挂载 | `~/TwoDrive/OneDrive` |
| 配置 / 令牌 | `~/.config/twodrive/config.toml` / 同目录的 `tokens.json` |
| 元数据与恢复记录 | `~/.local/share/twodrive/twodrive.sqlite3` |
| 内容缓存 | `~/.local/share/twodrive/cache` |
| 活动 / 上传会话 | `~/.local/share/twodrive` 下的 `activity.json` / `upload-sessions.json` |
| Nautilus 诊断日志 | `~/.local/state/twodrive/nautilus.log` |

Rust 组件支持 `XDG_CONFIG_HOME`、`XDG_DATA_HOME` 和 `TWODRIVE_MOUNT_DIR`，但 **Nautilus 扩展和软件包服务仍包含默认路径假设**。开发时覆盖这些变量，不等于完成整套桌面集成的自定义路径配置。

```bash
twodrive status
twodrive status-path /Documents/report.pdf
journalctl --user -u twodrive-daemon.service -f
```

重启或卸载挂载前先关闭其中的文件。需手工卸载残留挂载时，先停止服务：

```bash
systemctl --user stop twodrive-daemon.service
fusermount3 -u ~/TwoDrive/OneDrive
```

第二条仅在挂载仍存在时需要。不要靠删除数据库/缓存来重置卡住的上传。报告应提供脱敏版本、环境、复现步骤和相关日志，而非完整运行数据目录。移除用户名、个人路径、文件名、令牌和带授权信息的 URL。[问题报告与开发（英文）](../CONTRIBUTING.md)
