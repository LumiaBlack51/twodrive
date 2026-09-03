# twodrive 项目描述文档

## 1. 项目一句话说明

twodrive 是一个专为 Zorin OS 18.1 / GNOME / Wayland 环境设计的轻量级 OneDrive Files-On-Demand 客户端。它的目标不是做一个普通同步脚本，也不是简单的云端挂载工具，而是尽可能在 Linux 桌面上复现 Windows OneDrive 的核心体验：登录 OneDrive 后只同步元数据，文件默认只在云端显示，用户打开文件时才下载，用户可以对指定文件或文件夹执行“释放空间”或“始终保留在本地”，同时拥有一个可在文件管理器中正常浏览和使用的同步空间。

项目暂名为 twodrive，含义可以理解为 two-way drive，也可以理解为 Linux 上对 OneDrive 体验的再实现。它面向个人用户，尤其是使用 Zorin OS / Ubuntu GNOME 笔记本、需要在 OneDrive 中存放课程资料、论文、文档、代码项目的用户。

---

## 2. 项目背景

Linux 上没有微软官方 OneDrive 桌面客户端。现有方案大致有三类：

1. 命令行同步客户端，例如 onedrive client for Linux。
2. 云端挂载工具，例如 rclone mount。
3. 第三方商业同步工具，例如 Insync。

这些方案各有价值，但都不能完整满足本项目目标。

传统同步客户端会把选中的内容真实同步到本地，不适合大型云盘按需访问。rclone mount 可以把 OneDrive 挂载为远程文件系统，但它更像网络盘，不是真正的同步空间，不适合作为长期开发目录，也没有类似 OneDrive 的右键“释放空间”“始终保留在本地”体验。商业客户端则受功能、平台、体验和成本限制，不一定能提供理想的 Linux GNOME 集成。

twodrive 的目标是做一个专门为 Zorin/GNOME 场景服务的客户端，牺牲跨平台复杂性，换取更好的本地体验、低资源占用和可控的 Files-On-Demand 语义。

---

## 3. 目标用户

典型用户是：

- 使用 Zorin OS 18.1 或 Ubuntu 24.04 系 GNOME 桌面的用户。
- 使用 Wayland 会话。
- 使用 OneDrive 存放课件、论文、PDF、Office 文档、代码项目和个人资料。
- 希望节省本地 SSD 空间。
- 希望文件管理器里能看到云端文件，但不希望登录后自动下载整个 OneDrive。
- 希望像 Windows OneDrive 一样，对文件或文件夹右键选择释放空间或始终保留在本地。
- 希望应用常驻后台，并支持开机自启动。
- 希望应用美观、快速、不卡、低功耗，适合轻薄本和电池使用。

---

## 4. 核心产品原则

twodrive 的设计必须围绕以下原则：

1. 登录后只同步元数据，不自动下载文件内容。
2. 文件默认可以是 online-only，即只在云端存在，本地只显示占位视图。
3. 只有当用户打开文件时，才下载真实内容。
4. 下载后的文件进入本地缓存状态，可按策略保留一段时间。
5. “释放空间”只删除本地缓存，不删除云端文件。
6. “删除文件”才表示删除云端文件。
7. “始终保留在本地”表示该文件或文件夹进入 pinned 状态，必须完整保存在本地，并继续双向同步。
8. pinned 文件不参与自动释放。
9. 开发目录应默认建议 pinned，因为代码项目需要真实本地文件系统语义和稳定低延迟读写。
10. Nautilus 文件管理器扩展不能直接访问网络，只能查询本地 daemon 的状态。
11. GUI 和托盘只负责展示和控制，不能承担同步引擎逻辑。
12. 后台任务必须低唤醒、低优先级、可暂停，避免耗电和卡顿。
13. 所有修改操作都必须本地优先：先让缓存与 SQLite 在同一个本地提交点形成可靠状态，再由后台更新云端；“本地保存成功”和“云端同步成功”是两个不同事件。
14. 本地文件身份在生命周期内必须稳定。OneDrive item ID 是可后绑定的远端属性，不能在上传完成时替换 inode 和数据库主键。

---

## 5. 项目不是 rclone mount

理解 twodrive 时，一个关键点是：它不是 rclone mount 的套壳。

rclone mount 的本质是把云端存储挂载成一个远程文件系统。它适合临时访问云端内容，但不适合作为完整的 OneDrive Files-On-Demand 替代品。它没有稳定的逐文件释放空间体验，也没有同步状态图标、右键固定本地、冲突处理、本地同步策略、开发目录策略等完整桌面客户端功能。

twodrive 的定位是同步客户端，而不是单纯挂载工具。它需要有自己的状态数据库、本地缓存、文件状态机、上传队列、右键菜单、托盘、设置界面和低功耗策略。

---

## 6. 核心使用体验

用户安装并登录 twodrive 后，系统中会出现一个同步空间，例如：

```text
~/TwoDrive/OneDrive
```

用户可以像浏览普通文件夹一样浏览这个目录。目录中显示的是 OneDrive 云端文件和文件夹。初次登录后，twodrive 只拉取文件名、路径、大小、修改时间、文件 ID、etag 等元数据，不下载实际文件内容。

当用户打开一个 online-only 文件，例如：

```text
~/TwoDrive/OneDrive/Courses/Econometrics/Lecture_01.pdf
```

twodrive 会在后台下载该文件到本地缓存，然后把内容提供给打开它的应用。第二次打开时，如果缓存仍然有效，就直接从本地缓存读取。

用户可以右键文件或文件夹执行：

- 释放空间
- 始终保留在本地
- 取消始终保留
- 立即同步
- 查看状态
- 在 OneDrive 网页中打开

用户也可以从托盘菜单看到同步状态、暂停同步、立即同步、释放未固定缓存、打开设置。

---

## 7. 文件状态模型

twodrive 的核心是文件状态机。每个文件在数据库中都有状态。

主要状态包括：

```text
online_only
hydrating
cached
synced
pinned
dirty
uploading
conflict
error
```

各状态含义如下：

- online_only：文件只在云端，本地没有真实内容。文件在同步空间中可见，但打开时需要下载。
- hydrating：文件正在从云端下载到本地缓存。
- cached：文件已经下载到本地缓存，但不是永久固定文件，未来可能按策略释放。
- synced：本地缓存内容与云端一致。
- pinned：文件或文件夹被设置为始终保留在本地。它必须完整保存在本地，并继续双向同步，不参与自动释放。
- dirty：本地内容已修改，但尚未上传完成。
- uploading：本地修改正在上传到 OneDrive。
- conflict：本地和云端同时修改，无法安全自动合并。
- error：同步、下载、上传或认证出现错误。

状态之间的典型转换：

```text
online_only -> hydrating -> cached -> synced
cached -> online_only        // 释放空间
cached -> pinned             // 始终保留在本地
pinned -> cached             // 取消始终保留
cached -> dirty -> uploading -> synced
synced -> conflict           // 本地和云端冲突
any -> error                 // 操作失败
```

---

## 8. 释放空间的语义

“释放空间”是 twodrive 中非常重要的动作。它绝不等同于删除文件。

当用户对文件执行释放空间时，twodrive 应该：

1. 检查文件是否处于 dirty、uploading、hydrating 或 opened 状态。
2. 如果文件有未上传修改，不允许释放，或提示用户先完成同步。
3. 如果文件是 pinned，不应直接释放，除非用户先取消 pinned。
4. 删除本地缓存中的文件内容。
5. 保留数据库中的文件元数据。
6. 保留同步空间中可见的文件条目。
7. 将状态改为 online_only。
8. 不对 OneDrive 云端文件执行删除操作。

对于文件夹释放空间，twodrive 应递归释放该文件夹下所有非 pinned、非 dirty、非 uploading、非 opened 的 cached 文件。

释放空间只应该操作 twodrive 的缓存目录，例如：

```text
~/.local/share/twodrive/cache/
```

不应该通过删除挂载目录中的路径来实现释放。

---

## 9. 始终保留在本地的语义

“始终保留在本地”对应 pinned 状态。

当用户对文件执行始终保留在本地时，twodrive 应该：

1. 将文件标记为 pinned。
2. 如果文件当前是 online_only，则加入后台下载队列。
3. 下载完成后将真实内容保存在本地缓存或本地镜像区域。
4. 后续云端修改需要自动同步到本地。
5. 后续本地修改需要自动上传到云端。
6. 自动释放策略不能删除 pinned 文件。
7. 缓存容量清理策略也不能删除 pinned 文件。
8. 断网时，pinned 文件仍应能访问。

当用户对文件夹执行始终保留在本地时，twodrive 应该：

1. 将该文件夹标记为 pinned scope。
2. 递归下载该文件夹下已有文件。
3. 未来云端新增到该文件夹的文件也应自动下载。
4. 本地在该文件夹中新建、修改、删除文件时，应继续双向同步到云端。
5. 该文件夹中的文件默认不参与自动释放。

取消始终保留时，只是取消 pinned 属性，不应立即删除本地内容。取消后文件可以进入 cached 状态，后续由用户手动释放或由自动释放策略释放。

---

## 10. 开发目录的特殊策略

用户希望在 twodrive 同步空间中做开发。这是一个高风险但重要的场景。

代码项目与普通文档不同。IDE、Git、语言服务器、编译器和包管理器会频繁执行大量文件系统操作，例如 stat、open、read、write、rename、fsync、文件监听、锁文件、临时文件生成等。如果代码项目处于 online-only 或频繁释放状态，性能和兼容性都会很差。

因此 twodrive 应将开发目录作为特殊场景处理。

建议行为：

- 当检测到目录中存在 `.git/`、`package.json`、`Cargo.toml`、`pyproject.toml`、`go.mod`、`CMakeLists.txt` 等项目特征时，提示用户将该目录设为始终保留在本地。
- 开发项目默认建议 pinned。
- 对 pinned 开发项目，本地读写应优先，后台负责同步到云端。
- 不建议对开发目录自动释放空间。
- 对大型依赖目录和构建产物应提供忽略规则建议。

典型忽略规则包括：

```text
node_modules/
.venv/
target/
build/
dist/
__pycache__/
*.tmp
*.swp
.~lock.*
```

是否忽略 `.git/` 需要谨慎。默认不应强制忽略源码和 Git 历史，但可以让用户自行配置。

---

## 11. 架构总览

twodrive 应该采用 daemon-centered 架构。

核心结构：

```text
Microsoft OneDrive
        │
        │ Microsoft Graph API
        │ delta / download / upload
        ▼
twodrive-daemon
        │
        ├── FUSE filesystem: ~/TwoDrive/OneDrive
        ├── SQLite index database
        ├── Local cache manager
        ├── Upload/download queue
        ├── OAuth token manager
        ├── Sync engine
        ├── Power policy manager
        └── Local IPC server
                │
                ├── twodrive-cli
                ├── twodrive-tray
                ├── twodrive-gtk
                └── Nautilus extension
```

重要原则：

- daemon 是唯一的同步状态权威。
- FUSE 层只负责把文件系统请求转给 daemon。
- CLI、GUI、托盘、Nautilus 扩展都只是 daemon 的客户端。
- Nautilus 扩展不能直接访问 OneDrive 网络 API。
- GUI 崩溃不应影响同步。
- 托盘退出不应默认停止 daemon，除非用户明确选择停止同步。

---

## 12. 模块说明

### twodrive-core

负责项目核心类型和状态机。包括文件状态、配置结构、路径映射、错误类型、缓存策略、同步策略等。

### twodrive-backend

定义云端接口 trait，并实现 mock backend 和 OneDrive Graph backend。

mock backend 用于开发和测试，不访问真实网络。

Graph backend 负责 OAuth、delta、下载、上传、删除、重命名等 OneDrive 操作。

### twodrive-fs

负责 FUSE 文件系统。它将 `~/TwoDrive/OneDrive` 暴露为用户可见的同步空间。

FUSE 层处理 lookup、readdir、getattr、open、read、write、create、rename、unlink 等文件系统操作。早期阶段可以只支持只读，后续支持写入和上传。

### twodrive-daemon

后台常驻服务，是整个项目的核心。它负责启动 FUSE、维护数据库、调度同步、管理缓存、处理上传下载队列、执行自动释放、响应 CLI/GUI/右键菜单请求。

### twodrive-cli

命令行工具，用于登录、登出、查看状态、立即同步、pin、unpin、release、缓存清理等。

### twodrive-gtk

GTK4/libadwaita 设置界面。它只负责展示状态和修改配置，不直接执行同步逻辑。

### twodrive-tray

后台区域图标。显示当前同步状态，提供常用操作入口，例如打开同步空间、暂停同步、立即同步、释放缓存、打开设置。

### twodrive-nautilus

Nautilus 扩展。负责右键菜单和状态标志。它通过本地 socket 或 D-Bus 查询 daemon，不直接访问网络。

---

## 13. 本地文件和目录布局

推荐遵循 XDG 目录规范。

配置目录：

```text
~/.config/twodrive/
```

数据目录：

```text
~/.local/share/twodrive/
```

缓存目录：

```text
~/.local/share/twodrive/cache/
```

数据库：

```text
~/.local/share/twodrive/index.db
```

日志：

```text
~/.local/share/twodrive/logs/
```

运行时 socket：

```text
$XDG_RUNTIME_DIR/twodrive/daemon.sock
```

同步空间：

```text
~/TwoDrive/OneDrive
```

---

## 14. 数据库角色

SQLite 是 twodrive 的本地状态权威。

它需要保存：

- OneDrive 文件 ID。
- 本地虚拟路径。
- 文件名。
- 父目录 ID。
- 是否目录。
- 文件大小。
- 云端修改时间。
- 本地修改时间。
- etag / cTag。
- 当前状态。
- 是否 pinned。
- 是否 dirty。
- 缓存路径。
- 最近访问时间。
- 最近同步时间。
- 错误信息。
- delta link。
- 上传队列。
- 缓存条目。

数据库不能只是缓存，它决定文件状态和同步语义。

---

## 15. OneDrive 同步模型

OneDrive 接入应基于 Microsoft Graph。

核心流程：

1. OAuth2 授权码 + PKCE 登录。
2. 保存 access token 和 refresh token。
3. 初次登录后使用 delta API 拉取目录树和元数据。
4. 将 delta link 保存到数据库。
5. 后续同步使用 delta link 获取云端变化。
6. 对 online-only 文件，只保存元数据，不下载内容。
7. 用户打开文件时，通过 Graph 下载文件内容。
8. 大文件上传应使用 upload session。
9. 本地与云端冲突时，不应静默覆盖。

桌面客户端不应依赖 webhook 作为第一版核心机制，因为普通笔记本没有稳定公网回调地址。第一版使用 delta 轮询更现实。

---

## 16. FUSE 行为

FUSE 是 twodrive 实现按需下载的关键。

当用户浏览目录时，FUSE 根据 SQLite 中的元数据显示文件和文件夹。此时不下载文件内容。

当用户打开文件时，FUSE 调用 daemon：

1. 检查文件状态。
2. 如果是 online_only，则进入 hydrating。
3. 调用 backend 下载文件到 cache。
4. 下载完成后状态变为 cached 或 synced。
5. 将文件内容返回给应用。

当用户读取 cached/pinned 文件时，应直接从本地缓存读取。

当用户写入文件时，内容先写入本地缓存；`flush`、`fsync` 和路径级 `truncate` 只保证本地持久化，关闭写句柄后记录进入 dirty 队列。上传完成后再绑定或更新 OneDrive item ID 与 etag。

`create`、`mkdir`、`rename` 和 `unlink` 不得在 FUSE 请求线程中调用 Graph。它们先修改本地命名空间，并在同一 SQLite 事务中写入持久化操作：

- 文件内容修改进入 dirty/uploading 恢复队列。
- 建目录和移动进入 `pending_metadata_operations`。
- 删除进入 `pending_deletes`，并立即从本地命名空间隐藏。
- 同一文件的远端操作严格串行；不同文件可以有限并发。
- delta 同步不得覆盖仍有本地写入或元数据操作的记录。

应用收到保存成功，表示数据已经安全落到本地缓存；等待网络、限流、冲突和云端失败通过独立同步状态反馈，不能让已经完成的本地保存返回失败。

---

## 17. 删除、释放、取消固定的差异

这三个动作必须严格区分。

删除：

- 意味着删除 OneDrive 云端文件。
- 本地元数据和缓存也应移除。
- 可以进入云端回收站。

释放空间：

- 只删除本地缓存。
- 不删除云端文件。
- 文件仍在同步空间中可见。
- 状态变为 online_only。

取消始终保留：

- 只取消 pinned 属性。
- 不立即删除本地缓存。
- 文件可继续处于 cached 状态。
- 后续可能被自动释放策略清理。

---

## 18. 缓存策略

缓存策略应兼顾节省空间和体验。

可配置项包括：

- 非固定文件保留时间：7 天、30 天、90 天、180 天、永不。
- 缓存最大容量：5GB、20GB、50GB、自定义。
- 磁盘剩余空间低于某阈值时主动释放。
- 电池模式下是否延后缓存整理。

自动释放只作用于 cached 文件，不能作用于：

- pinned 文件。
- dirty 文件。
- uploading 文件。
- hydrating 文件。
- opened 文件。
- conflict 文件。

默认策略可以较保守，避免误释放用户近期需要的文件。

---

## 19. 冲突处理

冲突是同步客户端必须处理的问题。

典型冲突场景：

1. 文件在本地被修改，尚未上传。
2. 同一文件在云端也被修改。
3. etag 不匹配，无法确认覆盖是否安全。

此时 twodrive 不应静默覆盖任何一方。

推荐行为：

- 保留云端版本为原文件名。
- 将本地版本另存为冲突副本。
- 冲突副本命名包含设备名和时间。

例如：

```text
paper.docx
paper (conflict from user 2026-06-30).docx
```

状态标记为 conflict，并通过 Nautilus emblem 和托盘错误提示告知用户。

---

## 20. Nautilus 集成

Zorin/GNOME 的文件管理器集成是 twodrive 的重要体验。

Nautilus 扩展应提供两类功能：

1. 右键菜单。
2. 文件状态标志。

右键菜单包括：

- 释放空间。
- 始终保留在本地。
- 取消始终保留。
- 立即同步。
- 查看状态。
- 在 OneDrive 网页中打开。

状态标志包括：

- online_only：云端图标。
- hydrating / uploading：同步中图标。
- cached / synced：已同步图标。
- pinned：始终保留图标。
- conflict / error：错误图标。

Nautilus 扩展必须轻量。它不能访问 OneDrive API，不能做网络请求，不能扫描整个云盘。它只能通过本地 socket 或 D-Bus 向 daemon 查询某个路径的状态。

如果 daemon 不可用，扩展应快速失败，不应卡住文件管理器。

---

## 21. 托盘和后台应用区域

twodrive 应常驻后台区域。

托盘图标负责显示整体状态，而不是负责同步。

托盘菜单可包含：

- 打开 TwoDrive 文件夹。
- 当前同步状态。
- 暂停同步。
- 立即同步。
- 释放未固定缓存。
- 设置。
- 退出托盘。
- 退出并停止同步。

在 GNOME/Zorin 上，托盘支持可通过 AppIndicator / StatusNotifierItem 实现。由于 GNOME 对传统托盘图标支持并不统一，应用应在设置或首次启动时提示用户启用对应的 GNOME 扩展。

---

## 22. 开机自启动

twodrive 应支持用户登录桌面后自动启动。

推荐分为两层：

- systemd --user 启动 twodrive-daemon。
- XDG Autostart 启动 twodrive-tray。

daemon 崩溃后应由 systemd 自动重启。托盘退出不应默认停止 daemon。

这样可以保证同步核心稳定，GUI 和托盘只是控制层。

---

## 23. GUI 设计

设置界面应使用 GTK4 + libadwaita，尽量贴近 GNOME 原生体验。

界面风格应简洁、现代、轻量，不使用 Electron，不使用大型 WebView，不做花哨动画。

推荐页面：

- 状态。
- 账户。
- 同步空间。
- 缓存。
- 性能与电池。
- 忽略规则。
- 错误与日志。
- 关于。

状态页显示：

- 登录状态。
- 同步状态。
- 云端文件数量。
- 本地缓存占用。
- pinned 文件占用。
- 待上传数量。
- 待下载数量。
- 最近错误。

缓存页提供：

- 缓存保留时间。
- 缓存最大容量。
- 立即释放未固定缓存。
- 查看最大缓存文件。

性能与电池页提供：

- 电池模式策略。
- 上传下载并发限制。
- 低功耗模式。
- 接入电源时是否补全 pinned 下载。

---

## 24. 低功耗和 Lunar Lake 优化思路

twodrive 面向轻薄本，必须重视低功耗。

项目不应尝试强行控制 CPU 核心，而应通过低唤醒、低优先级、合理并发、事件驱动来让系统调度器自然把后台任务放到更合适的核心上。

后台策略：

- 空闲时不频繁轮询。
- delta 同步使用较低频率。
- 电池模式下进一步降低轮询频率。
- 上传和下载并发受限。
- 缓存整理在电池模式下延后。
- 大文件上传下载在电池低电量时暂停或提示。
- 使用 nice、ionice、systemd CPUWeight / IOWeight 降低后台任务优先级。
- 错误重试使用指数退避。
- 不要每秒刷新托盘状态。
- 不要让 GUI 或 Nautilus 扩展持续扫描。

建议电池模式行为：

- 下载并发：1。
- 上传并发：1。
- delta 间隔：15–30 分钟。
- 暂停非必要预下载。
- pinned 下载可延后到接入电源。

接入电源行为：

- 下载并发：2–4。
- 上传并发：1–2。
- delta 间隔：2–5 分钟。
- 允许补全 pinned 文件夹。
- 允许缓存整理。

---

## 25. 性能目标

推荐性能目标：

- 空闲 CPU 接近 0%。
- 空闲内存尽量控制在 80–150MB 以内。
- GUI 打开快速，不阻塞同步。
- Nautilus 打开普通目录时不卡顿。
- 右键菜单快速出现。
- 状态 emblem 查询只访问本地 daemon。
- 登录后首次索引只拉元数据，不下载文件内容。
- 电池模式下不主动下载非 pinned 文件。
- 对大目录使用分页、懒加载和缓存，避免一次性处理过多文件。

---

## 26. 安全和隐私

OAuth token 应安全保存。

优先使用 Linux Secret Service，例如 GNOME Keyring。若早期版本暂时 fallback 到本地文件，必须：

- 明确标注为 fallback。
- 权限限制为 0600。
- 不在日志中打印 token。
- 不在错误信息中泄露认证信息。

日志应避免记录敏感内容。可以记录文件路径和错误摘要，但不要记录 access token、refresh token、完整授权 URL 等敏感数据。

---

## 27. 错误处理

常见错误包括：

- 登录过期。
- 网络不可用。
- OneDrive API 限流。
- 下载中断。
- 上传失败。
- 磁盘空间不足。
- 文件被占用。
- 云端文件已删除。
- 权限不足。
- 本地和云端冲突。

错误处理原则：

- 不静默丢数据。
- 不自动覆盖冲突。
- 不因单个文件错误中断整个同步。
- 错误应可在 GUI 和 CLI 中查看。
- 网络错误应指数退避重试。
- 上传失败的 dirty 文件必须保留在本地。
- 元数据操作和删除失败必须保留在持久化队列，重启后继续执行。
- 后台上传完成时只能绑定远端身份，不能替换稳定的本地身份。

---

## 28. 打包和平台范围

第一阶段平台范围应非常克制。

只支持：

- Zorin OS 18.1。
- Ubuntu 24.04 系。
- GNOME。
- Wayland。
- Nautilus。
- x86_64。
- OneDrive Personal 优先。

暂不支持：

- KDE。
- Nemo。
- Caja。
- Flatpak。
- Snap。
- Windows。
- macOS。
- 多云盘。
- SharePoint 复杂团队库。
- Android / iOS。

打包形式优先使用 `.deb`，因为 FUSE、systemd --user、Nautilus 扩展、AppIndicator、桌面自启动等能力在传统 deb 包中更容易处理。

---

## 29. 推荐技术栈

核心语言：Rust。

推荐原因：

- 性能好。
- 内存安全。
- 适合后台服务。
- 适合并发下载上传。
- 适合文件系统和状态机。
- 比 Python/Electron 更轻。
- 比 C/C++ 更安全。

推荐组件：

- FUSE：fuser 或 fuse3。
- HTTP：reqwest。
- 异步运行时：tokio。
- SQLite：rusqlite 或 sqlx sqlite。
- OAuth：oauth2 crate 或自实现 PKCE 流程。
- GUI：GTK4 + libadwaita。
- 托盘：AppIndicator / StatusNotifierItem。
- Nautilus 扩展：Python Nautilus extension。
- 本地 IPC：Unix socket 或 D-Bus。
- 日志：tracing + journald / 本地日志文件。

---

## 30. 项目气质

twodrive 应该是一个轻量、克制、可靠的桌面工具。

它不应该像 Electron 应用那样常驻几百 MB 内存，也不应该像脚本工具那样缺少桌面体验。它应该更接近一个 GNOME 原生后台服务：平时安静地待在后台，用户需要时才出现；没有必要时不唤醒 CPU，不扫盘，不乱下载；出错时清楚告诉用户；涉及删除和释放空间时语义非常明确。

项目的美观不应来自复杂动画，而应来自清晰的信息架构、GNOME 原生控件、状态明确、操作安全、反馈及时。

---

## 31. 最终理解

twodrive 的本质是：

```text
一个面向 Zorin/GNOME 的 OneDrive Files-On-Demand 同步客户端。
```

它的技术核心是：

```text
FUSE 虚拟同步空间 + Microsoft Graph 元数据同步 + 本地缓存 + SQLite 状态机 + Nautilus 集成 + systemd 后台服务。
```

它的产品核心是：

```text
登录不下载，打开才下载，释放只删本地缓存，始终保留则完整本地同步。
```

它的工程核心是：

```text
同步状态必须可靠，文件语义必须安全，后台行为必须低功耗，桌面体验必须像一个真正的 GNOME 应用。
```

如果后续 AI 或开发者继续参与这个项目，应首先理解：twodrive 不是“另一个 rclone GUI”，也不是“普通 OneDrive 同步脚本”，而是试图在 Linux 上复现 OneDrive Files-On-Demand 体验的专用客户端。
