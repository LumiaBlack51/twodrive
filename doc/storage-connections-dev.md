# Development 存储连接 / Storage connections

2026-10-15（Development 0.1.4）：`dev` 的 Development 桌面“连接 → 添加连接”可在已有连接在线时使用。可新增、修改、删除其他连接；正在使用的连接需先断开才能编辑或删除。每条已保存连接提供“登录并使用”或“切换使用”，活动连接明确标为“当前使用”；行点击保留为快捷操作，切换仍受待上传缓存保护。当前为单个活动连接，配置列表可保存多个账号。

## SMB2/3（本地只读候选）

2026-11-07 本地审查分支新增 **SMB2/3（只读）**，复用添加连接/登录/浏览/下载流程。手动填写服务器、端口、共享、共享内根目录和账号；需先准备独立 SMB Python 组件。默认 SMB3 加密，SMB2 仅签名需显式选择；无 SMB1、写入、备份、挂载、链接/DFS 或 SMB 自动发现。仅在受控回环 Samba 合成共享验证；默认 Samba 4.19/4.22 因缺少安全能力声明被拒绝，真实 Windows/NAS 未验证；尚未发布或安装。详情见 [SMB 指南](smb-readonly-dev.md) 和[后续阶段](lan-storage-roadmap.md)。

Local review candidate: SMB2/3 uses the existing connection form and file browser. A private Python component is required; encryption is mandatory by default, signed-only access needs explicit selection, and all writes/mounts/backups remain unavailable. Loopback Samba was tested; Windows and physical NAS were not. See the [SMB guide](smb-readonly-dev.md).

## WebDAV 网盘与 NAS

选择 **WebDAV / NAS**，填写服务端提供的根 URL、用户名、密码，按需启用“允许修改远端文件”，保存后点击“登录并使用”。默认使用 HTTPS 并验证证书。局域网 NAS 仅提供 HTTP 时，在高级设置中显式开启“允许局域网 HTTP”，地址须使用数字私有 IP，例如 `http://192.168.1.10:5005/`；该段连接不加密。公共地址或域名 HTTP 仍拒绝，URL 不得内嵌凭据。局域网 HTTP 使用操作系统到该 IP 的路由；不会据此宣称已绕过 VPN/透明网关。

根 URL 应指向启用了 WebDAV 的共享目录。读取、上传、条件覆盖、新建目录、移动、删除和文件夹备份依赖服务端权限和协议实现。S3、SMB、FTP 等需要各自适配器，不能只填进 WebDAV 地址。

## OneDrive 与 Google Drive

选择 **OneDrive**，填写连接名称并保存，点击“登录并使用”，尚未登录时在系统浏览器完成 Microsoft 登录。登录和缓存使用 Development 独立目录，不修改稳定版服务及其配置；OneDrive 当前使用系统路由/代理。普通文件支持读写、修改、移动、删除，Linux 可挂载，文件夹备份使用既有流程。新增对象使用冲突失败策略；覆盖必须具有现存对象的明确版本。可保存多个账号配置，浏览器若自动选中账号，可在 Microsoft 页面选择另一个账号。

Google 登录见 [Google Drive 指南](google-drive-dev.md)。私有登录目录和 Linux 挂载路径可留空，由引擎按连接 ID 自动选择独立目录；高级设置允许指定路径。不要把不同账号的私有登录目录指向同一个位置。

## PC / NAS 的 TwoDrive 设备共享

[Development 0.1.5](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.5) 桌面同时提供服务端 GUI：打开左侧 **共享**，选择目录，按需开启“允许对方修改文件”，点击 **启动共享 → 导出连接文件**。另一台电脑在 **连接 → 添加连接 → 从 TwoDrive 连接文件添加** 选择该文件，名称、设备身份和权限会自动填入，点击 **保存并连接**。无需手工填写 IP、端口或密钥。

连接文件是十分钟有效、仅能首次使用一次的配对邀请，包含访问秘密，请通过可信渠道传递。配对成功后客户端记住设备，后续重连无需再次导入；重新导出会替换未使用的旧邀请。服务端可在“已授权设备”撤销访问。默认只读；修改目录或读写权限后使用新的共享身份，需要重新导出。关闭窗口后共享继续在托盘运行，退出应用会停止；重启应用后需点击“启动共享”。已保存目录和选项会恢复。

服务端和客户端可在同一个应用中同时运行，断开客户端连接不会停止本机共享。底层仍使用 QUIC/TLS；TwoDrive 客户端自动管理自己的本机 WebDAV 网关。第三方 WebDAV 客户端不能仅凭远端主机的 IP 直接连接 QUIC 服务，需先在客户端电脑运行 TwoDrive 网关，再连接它输出的本机 URL 和凭据。普通 NAS 无法运行此程序时，优先使用它的现有 WebDAV 服务。

保留命令行服务，适用于无桌面主机：

Linux 分享端示例（状态目录须独立；`--write` 显式允许修改）：

```bash
twodrive-dev --state "$HOME/.local/share/twodrive-share" serve \
  --root /absolute/shared-directory --write \
  --discoverable --device-name "My PC" --invite-file /absolute/private/invite.json
```

Windows 分享端示例：

```powershell
.\twodrive-dev.exe --state "$env:LOCALAPPDATA\twodrive-share" serve `
  --root D:\Share --write --discoverable --device-name "My PC" `
  --invite-file C:\Private\invite.json
```

在接收端选择 **PC / NAS（设备共享）**，点击“发现局域网设备”，选择对应设备，再通过文件选择器导入其可信配对邀请并保存。也可直接导入邀请。邀请十分钟有效、单次使用；过期后分享端使用相同 `--state` 执行 `invite --out NEW_ABSOLUTE_PRIVATE_FILE` 重新签发。邀请包含访问授权秘密，须通过可信渠道传递。

发现使用 IPv4 UDP 49183，需要分享端 `--discoverable` 且防火墙允许局域网探测。响应签名绑定设备公钥与本次随机探测，但设备名称不是可信身份。选择设备后邀请必须与所选公钥匹配；发现本身不会授予文件访问权。客户端复制邀请到私有配对目录，首次连接后删除私有副本，随后使用已授权身份重连。原邀请文件由用户保存或移除。

跨网络可直接导入邀请；发现广播不会跨公网。数据传输沿用既有 iroh QUIC/TLS、UDP 打洞和加密中继回退。直连能否成功取决于 NAT、防火墙及网络策略，不能保证每个网络都打洞成功。自定义中继、网络策略、设备撤销见 [设备共享指南](../experiments/twodrive-dev/README.zh-CN.md)。未开启 `--discoverable` 的原服务器仍可通过邀请连接，但不出现在发现列表。

## 实际验证与范围

- 在线管理限制：Rust 与 Flutter 回归均证明旧逻辑失败、修复通过；实际隔离 desktop IPC 验证添加/编辑/删除其他配置不改变活动连接及其 revision，活动配置仍不可编辑/删除，未知连接 ID 不会断开当前账号。
- 实际本机 UDP + QUIC：发现、选择身份、导入邀请、私有副本权限、配对、条件写入、下载字节比对、改名、删除、重连及切回已有 WebDAV 通过。由 `scripts/test-storage-connections.py` 复现；不是独立公网 NAT 测试。
- WebDAV：局域网 HTTP 显式选择与地址范围测试、真实本机 HTTP/文件系统/FUSE/备份测试；未测试实体 NAS 的 WebDAV 实现。
- OneDrive：分页/不可信 continuation 边界沿用 Graph 引擎；新浅层目录和创建失败/条件覆盖请求有本地 HTTP 回归。真实个人账号通过隔离登录副本完成 8 MiB+123 字节上传/下载比对、条件覆盖/旧版本拒绝、改名、删除与自有目录清理，见[实际摘要](onedrive-desktop-validation-2026-10-14.json)；新浏览器登录、组织账号、真实 OneDrive FUSE/备份及原生 Windows 本轮未验证。
- Google：既有真实账号普通文件、FUSE 和备份验证见 Google 指南。本轮新增连接操作不要求重新登录 Google。

故障与交付记录：[TD-20261014-05](incidents.md#td-20261014-05连接任意账号后禁止添加或管理其他连接)。[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2) 已发布；见[最终构建与范围](development-0.1.2.md)。

## English

The Development desktop supports adding and managing inactive connections while another account is connected. Each saved connection has an explicit Sign in and use / Switch action and a Current marker; switching preserves pending-write protection. The active signed-in account is shown separately from the custom connection name, including the tray panel and native tooltip. Configure WebDAV/NAS using its service URL and credentials; HTTPS is the default. Numeric private-IP HTTP requires explicit opt-in and uses OS routing without claiming VPN bypass. OneDrive uses the existing Microsoft OAuth/Graph engine with independent Development account state; Google setup is described in its guide.

The latest `dev` desktop includes a **Sharing** page: choose a directory, start sharing, then export a connection file. On the other PC, use **Connections → Add → Import TwoDrive connection file → Save and connect**. The name, public device identity and write permission are filled automatically. This is newer than the published Development 0.1.4. Invitations expire after ten minutes and authorize one new device; subsequent connections reuse that identity. Exporting again replaces an unused invitation. The host can revoke devices. Sharing is read-only by default, continues in the tray, and stops when the app quits. Restarting the app restores settings but requires Start. Changing the directory or write permission creates a new host identity. Client disconnect does not stop hosting.

Headless servers can still use `twodrive-dev serve`. The GUI scans IPv4 LAN UDP port 49183; discovery alone never authorizes access. QUIC/TLS attempts UDP NAT traversal and can fall back to encrypted relay. A third-party WebDAV client requires the local TwoDrive gateway; it cannot connect directly to the host's QUIC port. Native Windows local IPC/QUIC transfer and widget interaction were tested; Linux CI, physical NAS and independent-network hole punching are separately recorded scopes. The regression scripts isolate state and touch disposable data. See [sharing validation](desktop-sharing-validation.md).

Protocol references: [Microsoft upload session](https://learn.microsoft.com/graph/api/driveitem-createuploadsession?view=graph-rest-1.0), [driveItem conflict policy](https://learn.microsoft.com/graph/api/resources/driveitem?view=graph-rest-1.0).

## 云端删除受阻（Development 0.1.3）

Linux 可写挂载先移除本地路径，再排队云端删除。状态页显示云端待确认、被拒绝和等待重试数量；拒绝不会让任务/必要缓存消失。修复权限或登录后可点击“重新尝试”，让任务在下一轮扫描重新评估。权限/认证退避 5 分钟至 6 小时，临时错误 1 分钟至 1 小时，更长 Retry-After 优先，重启保留。桌面文件页直接删除仍同步返回错误，不会加入挂载待办。Windows 当前没有 FUSE 挂载，因此这项队列状态仅用于已有 Linux 挂载状态。

English: Linux mount deletion status distinguishes local removal from unconfirmed cloud cleanup. Retained tasks survive restart, back off and can be made eligible through Retry. Direct desktop file deletion reports synchronously; permissions and deletion targets are unchanged.

## 自动刷新与账号名称（Development 0.1.4）

状态轮询每 3 秒进行，整个刷新周期只允许一项在途；长操作期间不会不断排入重复 status。引擎主动报告操作开始/完成和最新状态，完成后自动更新活动连接及账号。当前目录每 15 秒浅层检查，窗口重新获得焦点时也请求刷新；网络阻塞时等待当前请求返回，不积累后续轮询。状态轮询的暂时错误在下一次成功后消除，失败上传等明确操作错误仍保留供处理。这不是云端实时推送，也不保证网络请求能立即取消。

Google 使用服务返回的账号名称（现有探测优先邮箱）；OneDrive 使用 Microsoft `/me` 返回的显示名称，缺少时回退邮箱/UPN；WebDAV 显示已成功连接的用户名或“匿名访问”，设备共享显示设备连接名称。活动账号出现在连接页、管理页标题、托盘面板和系统托盘提示中。离线配置只显示“上次登录”，不冒充仍在线。账号文字仅用于展示，稳定身份 ID、profile revision 和 binding 仍控制文件操作/恢复。旧 session 没有名称时重新连接即可刷新；显示探测不可用时明确提示名称暂不可用，不把配置名当作真实账号名。

队列保留 32 项上限，满时拒绝新增工作而不阻塞取消通道；已经取消但尚未执行的任务不会再运行。文件传输和 FUSE 持久待办仍使用原有恢复机制，不能将 desktop IPC 队列与云端持久同步待办混为一谈。

English: Status refreshes are single-flight, completion events synchronize account/connection state, and shallow directory polling runs every 15 seconds or on focus. Transient polling errors recover automatically; action failures remain visible. Native tray tooltips and connection screens show adapter-provided account names. The bounded IPC queue keeps cancellation available and skips queued cancelled work; it is separate from durable mount synchronization.
