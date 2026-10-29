# TwoDrive Development 0.1.6 — Linux x64

先核对 Release 的 SHA256SUMS，解压完整目录，在 `bundle` 内执行：

```bash
python3 install-dav-desktop.py --bundle . --engine-dir . --gnome-anchor ./gnome-tray-anchor
~/.local/bin/twodrive-dav
```

要求 GTK 3、会话 D-Bus 与托盘宿主；挂载需 FUSE 3。安装到当前用户，不替换稳定 OneDrive 服务，不自动启用自启动。运行中的更新可加 `--preserve-running-runtime` 保留旧进程路径；安装器不会重启客户端。

GNOME/Zorin 的 TwoDrive 专用定位组件支持 X11 和原生 Wayland。新组件首次安装可能需下一次登录桌面才加载；不要为了更新强制退出正在进行的工作。组件已加载时启动器在 Wayland 会话使用原生 Wayland，显式 GDK_BACKEND 优先。其他 Wayland compositor 尚未验收。

连接页添加 WebDAV/NAS、PC/NAS（需分享端运行 TwoDrive）、Google Drive 或 OneDrive。Google 需用户自备桌面应用凭据 JSON；本包不含任何账号/令牌/用户 client secret。首次 Microsoft/Google 连接打开系统浏览器授权。普通 NAS 可使用它的 WebDAV 服务。详细规范/使用说明在 `docs/`，其中源码链接与历史验证链接请通过 GitHub 同版本源码阅读。

iCloud Drive 为只读预览入口：在现有连接表单内完成 Apple 账号、密码和双重认证，再保存并连接，日常登录不需要终端。可选诊断命令为 `twodrive-provider-login --state <该连接的私有登录目录>`。保留 `rclone`、Rclone 文档及组件校验清单；Linux 桌面和真实 iCloud 的运行结果需单独验收，参见 `docs/icloud-drive.md`。

分享此电脑：打开“共享”，选择目录，点击“启动共享 → 导出连接文件”。另一台电脑在“连接 → 添加连接 → 从 TwoDrive 连接文件添加”中导入并“保存并连接”。默认只读，邀请十分钟单次有效；配对成功后无需重新导入。关闭窗口后共享继续在托盘运行，退出应用停止；重启应用后需再次点击启动。修改目录或读写权限后需重新导出，服务端可撤销设备。验证范围见 `docs/desktop-sharing-validation.md`。

English: Verify SHA256SUMS and keep the whole bundle. Run the command above from `bundle` to install for this user. GNOME's new positioning component may load only on the next desktop login. Explicit GDK_BACKEND is preserved; autostart remains unchanged. Credentials are supplied by the user, never bundled. Consult `docs/` and the matching source archive for the provider/auth/upload contract and validation boundaries.
