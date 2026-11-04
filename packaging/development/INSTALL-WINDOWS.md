# TwoDrive Development 0.1.6 — Windows x64

核对 Release 的 SHA256SUMS，解压完整目录后运行 `twodrive_dav.exe`。不要只复制一个 EXE；引擎、DLL 和 `data` 都必须保留。可以使用同目录 `install-desktop-windows.ps1` 安装到当前用户；自启动默认关闭。Development 不采用稳定客户端的登录状态。

连接页支持 WebDAV/NAS、已有或新邀请的 PC/NAS 共享、Google Drive 与 OneDrive。Google 登录需要用户自己的桌面应用凭据 JSON；本包不含账号令牌、用户 client secret 或设备身份。保存后点击“登录并使用”打开系统浏览器授权；“切换使用”选择其他连接，“当前使用”标明活动连接。PC/NAS 共享需对方运行 TwoDrive 分享服务，并通过可信渠道传递短时单次邀请；发现设备不等于已授权。

iCloud Drive 为只读预览入口：在现有连接表单点击“登录 iCloud / 更新授权”，在客户端内完成 Apple 账号、密码和双重认证，再保存并连接。请保留 `rclone.exe`、Rclone 文档和组件校验清单；`twodrive-provider-login.exe` 仅作可选终端诊断。更新保留原 Development state 和已有 OneDrive 连接，先退出旧进程再启动新包。上传、备份及原生挂载不在该适配器支持范围，真实账号兼容性以实际验证记录为准，详见 `docs/icloud-drive.md`。

分享此电脑：打开“共享”，选择目录，点击“启动共享 → 导出连接文件”。另一台电脑在“连接 → 添加连接 → 从 TwoDrive 连接文件添加”中选择该文件，点“保存并连接”。默认只读，邀请十分钟单次有效；配对后可直接重连。关闭窗口后共享留在托盘运行，退出应用停止共享；下次打开应用需重新点击启动。修改目录或读写权限后需重新导出。服务端可撤销已授权设备。更多范围见 `docs/desktop-sharing-validation.md`。

本版 Windows 支持文件浏览、读写传输、目录备份与托盘；Linux FUSE 功能不等于 Windows 挂载驱动支持。当前验证说明见 Release/verification.json；云端读写、组织盘、桌面缩放/多屏和网络打洞的范围分别说明。未来网盘适配须遵循 `docs/provider-integration-spec.md`；源码链接可通过同版本 GitHub 源码归档阅读。

English: Verify SHA256SUMS, extract every file, and run `twodrive_dav.exe`. The included installer is optional and operates on the current user. Keep the engine, DLLs and data directory together. Credentials are user-supplied; native Windows mounting is not implemented. The `docs/` directory includes the future-provider/login/upload design specification and setup guides.
