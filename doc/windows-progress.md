# Windows 双发行版进度（2026-09-16）

## 当前接续点

2026-10-07：按用户要求整理当前完整 Windows 预览、登录、浏览、持久索引与下载缓存改动提交 PR。
提交前重新运行 Linux workspace 测试（127 passed、6 ignored）和 Clippy/fmt/diff 检查通过；
此前 Windows/Flutter/真实账户验收为 2026-09-17 记录，本次未将其表述为重新执行。
真实大文件续传缺口仍保留。原 PR #6 已关闭，新 PR 地址待创建后补记。


2026-09-17 更新：**持久 delta 索引、真实可续传下载缓存、Full/Lite 共享状态已实现。**
真实账户初次/增量 delta、两次小文件下载、取消、重启保留、刷新取消和 Lite/Full
共享视图已验证；当前账户最大文件只有 1,140 字节，因此**真实较大文件中断续传验收受阻**。
该项仅有确定性 4 MiB 测试，不能称为真实 OneDrive 大文件验收完成。
最终运行包：`E:/software/twodrive/artifacts-metadata-cache-delivery-20260917`。
接续细节、证据、限制与 CFAPI 前边界见本文末尾“持久索引与下载缓存交付”。
原账户保持登录；没有真实云端写操作。以下旧接续点作为历史保留。

### 历史接续点：只读目录浏览

2026-09-17：本阶段“真实 OneDrive → Rust → IPC → Full/Lite”只读目录浏览已完成。
当前 Full 与引擎运行自 `E:/software/twodrive/artifacts-cloud-browse-final-20260917`，
保留原账户登录，Full 停留真实根目录；没有本地同步或云端内容变更。
实际 Git 工作树为 WSL `/root/workspaces/twodrive-windows`，分支
`codex/windows-dual-preview-20260916`，HEAD `77ef808`，所有开发增量未提交。
起始 9 个登录文件及其他修改已保留。Windows `native-build` 仍是镜像。
完整实现、最终验证、默认忽略/未执行项、产物和下一步见本文末尾
“2026-09-17 本阶段交付”；结构化结果见
[results.json](windows-evidence/cloud-browse/results.json)。

## 历史：P1 接续点（原记录保留）

本轮为 **P1 纵向链路工程预览**，不是 Windows 原生文件同步完成版。
源码在 WSL /root/workspaces/twodrive-windows，隔离分支
codex/windows-dual-preview-20260916；Windows 的 native-build 只是构建镜像。
没有推送 main、发布 Release、安装服务、注册真实同步根或接触真实令牌/文件。
继续工作前先读本记录、AGENTS.md 与 doc/incidents.md。

## 输入与基线核对

- 用户工作目录 E:/software/twodrive 初始只有 PROTOTYPE.html 与 preview-tray.png，
  不是 Git 仓库，没有可报告的当前分支或未提交 Git 修改。
- WSL 常见源码目录未找到已有 TwoDrive checkout。在 /root/workspaces/
  twodrive-audit-20260916 新克隆，再 git worktree add 创建上述隔离 worktree。
- origin/main = 41f817b；provider-boundaries = 10bab5f；两者源码树完全一致，
  main 比 provider 多一个合并提交（left/right 1/0）。
- peer-control = dd0686b；与 main 的共同祖先 5b79afe，left/right 2/14；
  peer 分支有 37 个文件差异、控制协议/发现/信任/更新等。未整体合并。
- 已读 AGENTS.md、CONTRIBUTING.md、incidents.md，并检查原型。
  三个远程分支均无 design/windows；本机 E:/software、Downloads/Desktop/
  Documents 及 WSL 常见目录没有找到 DESIGN.zh-CN.md、REFERENCES.md。
  用户表示文件在本地或附件，当前实际可用附件只有 HTML 和 PNG。
  **缺失文字设计待补齐，不能宣称已对原设计逐条验收。**
- 原附件复制到 design/windows（HTML 仅标准化换行与行尾空白）；只是视觉参考，未导入其样例对象或账户。

## 决策与实现边界

1. 从 main 开发。仅复用 peer 分支经逐行审阅的 private_file/credentials
   跨平台 DPAPI 适配与 upload.rs 的 Unix 条件编译；不导入 peer 协议、
   发布密钥、控制通道或历史发布授权。
2. 原 twodrive-fs 中的 FUSE 模块及依赖仅在 Unix 编译；缓存、水化、上传恢复
   逻辑继续同源。Windows 的 twodrive-windows 使用同一个 core/backend/fs。
   不借助 WSL/FUSE 模拟 CFAPI。
3. Windows 命名管道 IPC v1；Unix socket 用于 Linux 回归。长度前缀 JSON、
   1 MiB 上限、协议版本及请求 ID 检查、超时、owner-only DACL、拒绝远程
   pipe 客户端。16 个并发连接。生产 DTO 无令牌。
4. GUI 只启动结构化 IPC bridge，不读写 SQLite、不解析 CLI 人类输出。
   命令成功/失败与后台快照一起返回；断连清除快照，绝不显示“已同步”。
5. 共享状态目录上的 OS 文件锁分别限定一个 engine 和一个 tray；
   Full/Lite 同引擎、同状态布局；关闭 UI/托盘不停止引擎。
   测试根需要显式指定，模式有标记，拒绝复用非本产品目录。
6. 单 worker 调度器：暂停持久化于后台后确认，不再出队新任务。
   已开始任务可完成，期间状态 draining；这不是瞬时取消。
7. MockImport 只在显式 --mock 模式可用，经过旧引擎本地写入/dirty/上传提交；
   下载同样复用水化路径；释放复用原数据库的缓存保护。文件图标只读后缀。
8. 正式入口为 unconfigured/signed_out，空文件/空账户；CFAPI、登录、
   账户切换及未接入设置明确禁用。不是隐藏 mock。
9. 发行包一律 preview、unsigned、native_sync_accepted=false；Full 包含 Flutter，
   Lite 仅 Rust EXE/说明/启动脚本。绝不使用测试密钥签名。

## 实际验证（持续补记）

- WSL cargo test --workspace --locked：101 个测试通过，6 个 FUSE 挂载测试默认
  ignored；不能将 ignored 计入通过。
- WSL cargo clippy --workspace --all-targets --locked -- -D warnings：通过。
- Nautilus Python 回归：19/19 通过。
- Windows cargo check / native test：通过；新增 5 个自动化测试在 Windows 通过。
- scripts/test-windows-ipc.py 已在 WSL 及原生 Windows 运行通过：
  读取列表不水化；第二引擎拒绝；版本不匹配不修改设置；暂停阻止实际队列；
  脏内容拒绝释放；上传确认；释放再下载字节一致；引擎断连；重启保留暂停。
- Windows 测试最初因 Python 默认 GBK 解码产生线程异常；改为显式 UTF-8，
  重跑无异常。旧次 PASS 不作为干净验收。
- Flutter 3.41.7 / Dart 3.11.5 / Rust 1.97.1 / VS 2026 Insiders。
  原生构建发现 fluent_ui ^4.12.0 自动选到 4.16.1，依赖调用了新版 Flutter API，
  虽 analyze lib 通过，release 编译失败；现固定 4.12.0。
  Windows Pub TLS 失败；通过 WSL 从官方 pub.dev 获取版本元数据/归档并核验
  archive_sha256 补齐构建缓存，不关闭 TLS 校验；已完成 offline 锁定解析和原生 Release 构建，结果见下方补记。
- CI 配置已编写；未推送，**CI 未执行**。本机原生结果不能替代整个 CI 矩阵。

## 验收缺口与下一步（不能省略）

- 找回 DESIGN.zh-CN.md / REFERENCES.md 并做正式逐项差距表。
- P2：将临时 scheduler 提升为可恢复的持久命令日志，epoch/事件订阅/分页、
  重连去重语义、并行总体进度/吞吐、后台错误分类、失败重试、排队恢复。
  当前 mutation replay 仅最近 256 项内存，近期记录 32，文件列表 200。
  Mock remote 内存数据在重启时丢失；不能当作云端耐久存储。
- P3：CFAPI 同步根注册、placeholder 身份及 generation、FETCH_DATA/取消、
  本地修改回传、rename/delete/冲突、占用与脏文件脱水保护；用隔离原生根
  验证图标/列表/搜索不触发水化。此项通过前所有包只能预览。
- P4：浏览器 OAuth PKCE、超时/取消、账户隔离、安全 refresh、账户切换与
  脏内容保留，Graph 真实账户端到端验证另需专用测试账户（本轮未使用）。
- P5：窗口激活复用、工作区/多显示器/DPI/任务栏锚定/Explorer 重启，
  文件操作、可写设置、无障碍、签名与卸载/升级/降级完整生命周期。
- 所有能力先有引擎确认和测试，再打开 UI 控件。不得用 peer presence 替代同步。

## 产物与证据

构建脚本 scripts/build-windows.ps1；用同一调用产生 Full/Lite，输出各文件 SHA256
及包清单，Lite 内容检查、两包引擎哈希一致检查。说明 doc/windows-preview.md。
后续运行日志与原型对应实际截图放在 doc/windows-evidence/。
提交号、最终产物与实机验证补记在本文件末尾；未验证项保留为待执行。


## 本轮最终补记

- Windows 原生 Full/Lite 同版本 release 包完成，打包脚本 -Offline 全流程成功。
  最终目录：E:/software/twodrive/artifacts-preview-final。
  Full ZIP 15,268,018 字节；Lite ZIP 3,267,596 字节。
  包 SHA256 见 windows-evidence/SHA256SUMS.txt；逐文件清单见 full-manifest.json /
  lite-manifest.json。Lite 只有两个 Rust EXE、说明、许可证、启动脚本和清单。
- 最终包重新执行 native-ipc-final.log 和 native-editions-final.log，均通过。
  Full/Lite 的 engine/tray 字节相同；第二 tray 被拒绝；关闭 Full 后 Lite
  复用同一引擎 PID，保留已保存暂停；关闭两个 tray 均不停止引擎。
- Windows DPAPI synthetic secret roundtrip/replacement 测试通过，确认密文不是明文。
  不代表真实 OAuth 或账户切换已接入。
- Flutter analyze 通过，3 项状态测试通过，Full 原生 release 构建成功。
  断连反馈问题及回归记于 TD-20260916-W03。
- 实机使用 computer-use 技能观察实际 Flutter 窗口。点击 Full 暂停后从独立
  IPC 验证 paused=true；暂停期间从文件页提交下载，queued=1、文件 online_only；
  恢复后变为 cached。关界面后 IPC 仍返回同一 PID。所有过程是隔离 Mock。
- 最终 compact 活动弹层由 --tray 启动，显示后台真实确认的 sandbox-note.md
  26 字节上传；点“管理中心”成功切换窗口。最终文件页断连清旧列表、
  禁用操作并显示“无法确认同步状态”；重连恢复且错误条清除。
- **未实测**：系统通知区域的真实鼠标左/右点击、Lite 菜单具体点击、弹层失焦、
  多 DPI/多屏/任务栏锚点/Explorer 重启；代码路径存在不等于这些验收通过。
  Full 已有管理窗口时托盘左击的激活/切换尚待完善。
- 另跑 Linux 隔离 FUSE daemon smoke 成功（linux-mock-smoke.log）：
  hydration/write/upload/move/delete/pin/release。这与默认测试中 6 个 ignored
  测试不同，不能将那 6 项改写成通过。
- 本轮启动的隔离 engine/UI/tray 验收进程已结束，临时数据保留供追查；
  没有卸载/重启既有 Linux 服务、改动真实挂载、登录真实账号或同步真实文件。
- 阶段结果：P1 预览可运行；P2–P5 仍未完成，不能宣布总体目标完成。
  本地提交和源码归档信息见交付目录 DELIVERY.md。

截图（都是原生应用截图；不是 HTML 截图，不证明 CFAPI）：

| 文件 | 场景 |
| --- | --- |
| windows-evidence/final-tray-mock.png | 活动弹层，对应参考图布局；明确 Mock、真实后台已确认 |
| windows-evidence/final-management-mock.png | 管理中心概览，真实近期上传确认 |
| windows-evidence/final-files-mock.png | 文件类型图标、云端状态/缓存状态、能力禁用 |
| windows-evidence/final-disconnected.png | 断连清除旧文件状态、操作禁用 |
| windows-evidence/final-reconnected.png | 重连后恢复快照并清除错误 |

归档文本统一为 UTF-8/LF（日志去除终端行尾填充）；没有改变验证结果。

## 2026-09-16 登录增量（本地，未发布）

- 新包：E:/software/twodrive/artifacts-login-preview。保留此前交付包，使用原生产预览状态目录。
- 新增浏览器 PKCE 登录、取消/10 分钟超时、回调 state/方法/路径校验；令牌交换和 DPAPI 保存成功后才显示成功。
- IPC 仅传递授权状态和脱敏错误，不传令牌。Mock 模式拒绝登录，已保存凭据时拒绝覆盖为其他账户。
- UI 账户页面新增登录/取消，保存凭据后显示账户已授权；不宣称文件已同步。不包含账户切换、Graph 文件工作器和 CFAPI。
- Windows backend 24 + engine 6 项测试通过；Flutter 7 项通过；fmt/Clippy/analyze 和 release 构建通过。
- 本机已启动授权；真实 Microsoft 登录待用户完成浏览器验证，未宣称端到端同步验收。

实机补记：Microsoft 浏览器授权已完成；原生 IPC 返回 auth_status=signed_in、auth_error=null，令牌交换及 DPAPI 保存成功。真实文件同步仍未接入。

## 2026-09-16 真实目录浏览（进行中，可接续）

- 实际 Git 工作树在 WSL `/root/workspaces/twodrive-windows`，分支 `codex/windows-dual-preview-20260916`；native-build 是 Windows 构建镜像。
- 开始时确认 9 个登录增量文件未提交。完整原补丁与源码备份保存在 WSL `/root/workspaces/twodrive-preserved-20260916-browse`。两处源码规范化换行后仅 scripts/build-windows.ps1 不同，保留各自版本；没有 reset、clean 或远程覆盖。
- 已增加 GraphBackend 只读 me/drive、root/items/children 查询；可信 Graph v1.0 continuation 路径校验；元数据客户端禁用重定向。复用已有 PKCE/TokenStore/DPAPI/刷新，401 有限刷新、协调刷新、保留未轮换 refresh token；错误不带敏感响应正文。
- 引擎 IPC v1 增加异步 browse/browse_next/cancel_browse/logout 与目录状态；单页传输、取消标记、查询 ID 隔离、5 分钟过期标识；不会写入同步数据库或活动列表。Full 正在接入分页累积与面包屑；Lite 新增 list 查询。
- 首次真实只读探测成功：根目录 complete，4 项（3 目录、1 不支持条目）；进入首个普通子目录 complete，1 项；返回根与刷新成功。无云端内容下载/变更，recent 活动为空。未保存名称和 ID。
- 实测临时停止旧登录引擎（PID 14096），使用同一状态目录和既有 DPAPI 凭据启动新引擎。旧运行包未覆盖；后续改为独立 browse-development-runtime 副本避免构建文件占用。
- 首轮 Flutter 7 项通过；新增浏览测试及完整验证进行中。Rust 首轮测试因 target/debug/twodrive-engine.exe 被实测进程占用而失败（非通过），已迁移运行副本后重跑。Python 编辑辅助脚本曾因默认 GBK 解码失败，无文件写入，改用 UTF-8 后成功。
- 下一步：完成确定性 HTTP/状态测试，原生 Full 实测及脱敏截图，Rust/Flutter 静态检查、Linux 回归、独立 Full/Lite 构建及原生 IPC/单实例验收。
- 本节为中间记录，不代表阶段完成；未运行检查不能算通过。没有创建 PR、提交、推送、合并或发布。

## 2026-09-17 本阶段交付：真实 OneDrive 只读目录闭环

**已完成本阶段只读浏览闭环，未完成本地同步。** 实际 Git 工作树仍为
WSL `/root/workspaces/twodrive-windows`，分支 `codex/windows-dual-preview-20260916`，
基线 HEAD `77ef808`；本轮代码未提交、未创建 PR、未推送、未合并或发布。
起始的 9 个登录未提交文件均保留，其他已有修改也未 reset/clean/覆盖；Windows
`native-build` 镜像中的既有 build-windows.ps1 差异单独保留。源码备份与原始补丁位置见上节。

### 实现与约束

- GraphBackend 按需 GET 当前 drive、目录及 children，Microsoft Graph v1.0，默认每页请求 100 项。没有递归扫描、delta 索引、云端写操作或内容下载。
- 目录身份以 account_id/drive_id/item_id 表达；文件以该目录身份和 item.id 定位，不以路径/名称作为主键。Graph nextLink 仅留在后台，验证 HTTPS、精确 Graph 主机/端口、v1.0 和同一 children 路径；检测重复游标。元数据 HTTP 禁止重定向。
- IPC v1 增加 browse、browse_next、cancel_browse、logout；查询异步执行，最多 4 个未结束 worker，单页最多 256 KiB。查询 ID、取消标记与登录状态阻止迟到响应发布；每次 IPC 仅传本页，Flutter 按 ID 合并去重。
- Full 文件页真实根目录/子目录、返回、多级面包屑、刷新、取消、分页、类型图标、字节大小和修改时间已接通。区分 loading、partial、complete、failed、stale；空目录必须由成功完成确认。真实模式概览不把目录数当全盘总数。
- 明示“云端浏览，尚未启用本地同步”；不开放上传、删除、固定、释放、内容下载。共享快捷方式、package、未知类型显式不支持，不伪装成空目录。元数据不写同步数据库、不产生活动记录。
- 复用原 PKCE、TokenStore/DPAPI 与令牌刷新；401 最多一次刷新，互斥协调，保存轮换凭据并保留服务端未重发的 refresh token。注销等待刷新锁后删除凭据，防止刷新重建已注销凭据；当前真实账户未注销。
- Graph/OAuth 的 429 保留 Retry-After 秒数或 HTTP 日期；等待超过本次预算时失败返回，绝不提前重试。浏览端共享等待截止时间，手动刷新也不能绕过。其他错误有界重试，网络/超时/权限/重新登录分类；日志不含令牌、授权码或敏感响应正文。
- 目录是内存快照，5 分钟后过期，分页需刷新。当前每个引擎只有一个活动浏览会话；另一 CLI/Full 查询会替换它，旧客户端明确提示过期并可刷新恢复。未实现多客户端独立游标会话或持久目录缓存。
- Lite 提供 `twodrive-engine.exe list --state ABSOLUTE_PATH [--drive ID --item ID | --next QUERY_ID]`，输出结构化单页 JSON；用 has_more 判断是否还有页。用法及 IPC 行为见 windows-preview.md。

### 实际验证及证据

证据集中在 [windows-evidence/cloud-browse](windows-evidence/cloud-browse/results.json)，只含脱敏日志、合成测试数据和遮盖私人名称的原生截图，不含真实目录清单、令牌或原始私人截图。

| 检查 | 本轮最终结果 | 证据 |
| --- | --- | --- |
| Windows Rust backend + windows | 42 passed，0 ignored | browse-rust-tests.log |
| Windows Clippy all-targets，警告视为错误 | 通过 | browse-clippy.log |
| Linux cargo test --workspace --locked | 115 passed，6 ignored（另列下方） | browse-linux-tests.log |
| Linux workspace Clippy all-targets | 通过 | browse-linux-clippy.log |
| Nautilus Python | 19 passed | browse-nautilus.log |
| Flutter 完整测试 | 11 passed | browse-flutter-tests.log |
| Flutter analyze（含测试） | No issues | browse-flutter-analyze.log |
| Rust fmt / git diff --check | 通过 | 最终工作树实际运行 |
| Windows 原生命名管道 / Linux Unix socket | 最终引擎通过 | browse-native-ipc.log / browse-linux-ipc.log |
| Full/Lite 单实例/交接 | 最终包通过 | browse-native-editions.log |
| Full/Lite release、同字节引擎和托盘、Lite 无 Flutter | 通过 | browse-final-build.log、各包 manifest.json |
| 当前账户只读 Graph → Rust → IPC → Lite list | 通过 | browse-live.log |

HTTP 确定性测试覆盖：分页/空页、中文与特殊名称、未知/共享类型、403/404、拒绝重定向/非可信目标、限流等待/取消、401 后一次刷新和再次 401、并发过期刷新、凭据轮换/保存/删除，以及 OAuth 120 秒限流跨查询不提前重试。引擎完成路径测试覆盖快速切换/取消/注销迟到结果、部分页失败保留和循环游标；Flutter 测试覆盖异步切换、分页去重、注销/取消迟到响应、加载/真实空/错误/不支持条目。

真实只读实测（最终引擎重新确认）：根目录 complete、4 项（3 个目录，1 个不支持条目）；普通子目录 complete、1 项；Lite list 同样成功。Full 原生界面按需进入多级目录，看到 2 个真实文件的类型图标、大小和修改时间；进入一个真实空目录后才显示为空；返回、根面包屑、刷新和 CLI 替换会话后的刷新恢复成功。没有读取任何文件内容或修改真实云端对象，IPC recent=0、同步数据库文件快照为空。更换最终引擎前后 Full EXE 与 Dart app.so 哈希一致；最终根目录截图重新拍摄。

截图：full-loading.png、full-root-redacted.png、full-child-redacted.png、full-files-redacted.png、full-empty-redacted.png。均为 computer-use 技能捕获的原生窗口，私人名称用确定性不透明矩形遮盖；未以生成图或 mock 替换界面。

### 未执行/默认忽略（不计为通过）

- 本轮未在真实账户制造 401/403/429、断网或强制令牌过期；这些异常恢复以 mock HTTP/合成状态测试验证。未注销真实账户，未新增授权范围。
- 真实已访问目录没有超过 100 项，**真实 nextLink 多页未触发**；多页处理及去重以确定性 HTTP/Flutter 测试验证。
- Linux 6 项默认 ignored 未执行：compiled_program_and_chmod_survive_remount、deferred_reader_rechecks_indexer_before_downloading、gio_directory_metadata_does_not_download_unknown_large_files、mounted_large_listing_and_copy_remain_responsive_during_download、mounted_release_cancels_download_and_old_handles、zip_extraction_into_pending_folder_on_mount。也未把历史 FUSE smoke 当作本轮通过。
- 远端 CI 未运行；多 DPI、多屏、Explorer 重启、通知区鼠标菜单完整矩阵未重测。单实例证据限定 engine/tray 文件锁与 Full/Lite 交接；未新增全局 Flutter 窗口互斥机制。
- CFAPI、内容传输、全盘 delta、双向同步、安装/签名均未实施，不属于此次只读阶段。

### 最终产物与接续

最终目录：`E:/software/twodrive/artifacts-cloud-browse-final-20260917`。
Full ZIP 16,937,725 字节；Lite ZIP 4,910,023 字节。哈希见目录及证据中的 SHA256SUMS.txt；逐文件清单见各包 manifest.json。旧登录包与其他预览包均保留，没有直接覆盖运行包。
最终引擎与 Full 管理窗口使用原 `windows-preview-state` 中的登录状态，当前停留真实根目录供继续查看；源码测试不从运行目录重建。

接续优先项：有实际需要时扩展多客户端独立浏览会话、对大目录做真实分页验收；任何未来内容同步另立阶段。继续保持真实数据只读、异常恢复可取消、凭据安全边界，不把本次元数据浏览标为文件已同步。

本轮失败已逐项记录：构建目标被运行程序占用、辅助脚本 GBK 解码失败、取消空身份回归失败、widget 测试 fixture 类型错误、Dart lint 阻止首轮打包，以及共用 Retry-After 截断机制。失败/忽略不计通过；最终通过的是修复后重跑结果。

最终校验补记：两个 ZIP 的 SHA256 及 Full 22 个文件、Lite 6 个文件的 manifest 大小/哈希全部逐项核对通过。本轮 5 张未脱敏临时截图已删除，只保留带不透明遮盖的交付截图；真实目录名/ID 未写入仓库。最终留运行的进程为引擎 PID 40024、Full PID 51916（供本次接续定位，PID 会变化）。

## 2026-09-17 持久索引与下载缓存交付

### 架构与已实现机制

- 保留既有 OAuth PKCE、DPAPI TokenStore、Graph 刷新/限流、Rust 引擎、IPC v1、
  Full/Lite 发行版和只读目录浏览。旧目录浏览从 Full 文件页“按目录浏览”进入。
  Flutter 只调用版本化 IPC bridge；不读数据库、不访问 Graph 或凭据。
- 复用 `engine.sqlite3`，在 core Database 增加 cloud_items/cloud_stage/cloud_delta/
  cloud_tasks 表，不改 Linux files/本地写入模型。元数据主键为 account/drive/item ID；
  父子关系保存 parent ID，不用路径作身份。分页逐页写暂存表，同一 ID 最后出现值生效。
- 完整分页成功后在单一 SQLite 事务中应用新增/修改/重命名/移动/删除及 delta 游标。
  失败或进程退出保留前一已提交索引；下次从已提交游标重新开始，清理未完成暂存页。
  410 失效游标重新全量枚举，成功前不清空旧索引。continuation 验证 Graph HTTPS
  主机、端口和同 drive delta 路径，检测循环；下载 URL 不进入索引。
- 删除目录在子项移动/删除完成前保留持久删除标记；后续批次也会清理已空的待删目录。
  TD-20260917-DELTA-PENDING-DELETE 回归已证明旧实现失败、修复后通过。
- 真实下载只走 Rust read-only provider：先验证 Graph 元数据 eTag/大小，再获取临时
  downloadUrl。独立无 Authorization 客户端向该 URL 发 Range，禁止自动重定向，
  HTTP 错误/正文/URL 不进入任务错误。最多三次传输尝试；重试重新获取临时 URL。
  206 严格校验 Content-Range，200 明确截断 partial 从零重写，超长/短内容不发布。
- 缓存键由 account/drive/item/eTag/size 哈希组成；临时 partial 与完成文件同目录。
  每次尝试及完成时落盘；完成前再次检查云版本、取消/账户 epoch 和索引版本，
  原子 rename 后才登记 cached。崩溃在 rename 与数据库登记之间留下的文件不自动
  视为有效缓存。重启将运行中任务标为 error，可显式续传；恢复偏移从实际 partial 读取。
- 注销取消 worker 并递增 epoch，迟到刷新/下载不能发布当前账户结果。不同账户索引
  和缓存隔离；保留旧账户本地数据，但登出后不向客户端显示。真实账户未执行注销。
- Full 文件页提供分页持久列表、文件类型图标、刷新/取消刷新、下载/续传/取消、
  字节/速度、cached/error、打开/显示缓存位置；概览使用同一索引文件数和缓存字节。
  Lite 的 index/refresh/download/cancel-download/tasks 调用同一 worker 和 IPC。
  命令接受不代表完成；完成状态来自后续快照。真实文件不标为 synced。

官方语义参考：[Graph delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)、
[下载与 Range](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)。

### 实际测试与构建

结构化结果：[metadata-cache/results.json](windows-evidence/metadata-cache/results.json)。
日志均为合成数据或真实账户的聚合结果，不含真实名称/ID/凭据/游标/下载 URL。

| 检查 | 本轮实际结果 |
| --- | --- |
| Windows Rust core/backend/windows | 85 passed，0 failed，0 ignored |
| Linux cargo test --workspace --locked | 127 passed，0 failed，6 ignored 未执行 |
| Windows relevant all-target Clippy / Linux workspace all-target Clippy | `-D warnings` 通过 |
| Rust fmt / Git diff whitespace | 通过 |
| Flutter 完整测试 / analyze | 12 passed / No issues |
| Flutter Windows release / Rust Full+Lite release | 通过；打包脚本同时执行完整 Flutter 检查 |
| Nautilus Python | 19 passed |
| Windows 命名管道 / Linux Unix IPC | 最终包/最终源码通过 |
| Full/Lite 单实例与交接 | 最终包通过，两版共用同一引擎 |
| Linux 隔离 FUSE daemon smoke | hydration/write/upload/move/delete/pin/release 通过；仅 Mock |
| ZIP / manifest / Full-Lite engine-tray 字节 | 两个 ZIP、Full 22 文件、Lite 6 文件逐项核验通过 |

新增确定性覆盖：多页初次和增量 delta，ID 稳定的 rename/move/delete，重复条目最后值，
跨批次目录删除，410 安全重建，刷新中断，游标写入失败事务回滚；HTTP body 中断后
Range 续传，4 MiB 取消后新 worker 续传，Range 返回 200，错误 Content-Range，
云版本变化，签名下载请求无 bearer，负文件大小拒绝，注销/账户 epoch/取消迟到保护，
重启 partial/缺失/旧版本缓存不报 cached，Full IPC 操作和中文文案。
旧实现失败证据为 delete-old-failure.log；修复通过为 delete-fixed.log。

### 真实 OneDrive 只读验证

- 同一已授权账户初次 delta 得到 56 条索引（46 个文件），后续增量成功；最终交付
  引擎也重跑增量成功。真实枚举仅一页，真实多页未触发；分页由确定性 HTTP 验证。
- 两个不同小文件各下载 1,132 字节，最终 cached 共 2,264 字节；没有输出内容或名称。
  一项从 Lite download 提交，Full 原生概览显示 46 文件和约 2.2 KiB 缓存，文件页
  显示相同持久状态、下载/续传、打开/显示位置、刷新及保留的目录浏览入口。
- 一次真实下载在 payload 前取消（0 字节），重启后保留取消任务，Lite 重试成功。
  **这不是已传输部分真实文件的 Range 续传证据。** 最终包重启保留已有索引和缓存。
- 一次真实刷新取消后仍保留原 56 条已提交索引，随后增量恢复成功。Windows 接受
  缓存 reveal IPC，并实际打开缓存目录。默认应用打开按钮有实现和 widget 命令验证，
  未打开真实内容阅读或执行。
- 账户最大文件 1,140 字节，无较大文件。因此真实较大文件中断/重启/Range 恢复无法
  在当前数据条件下执行；未上传测试文件，也未请求改变云内容。
- 真实 rename/move/delete、内容版本变化、410 和注销/切换账户均未人为制造。
  这些验收限定为确定性测试，不能写成真实账户异常恢复全部完成。

### 风险、未执行项与 CFAPI 前边界

- 真实较大文件 Range 恢复是剩余验收阻塞；获得已有、经授权可读取的大文件后再验。
  当前只支持每次一个下载；重启不自动重下，用户显式恢复。阻塞 HTTP 读期间取消
  最长可能等待本次内容请求 30 秒超时（元数据请求 20 秒）。
- 旧版本和孤立完成文件不会变为有效缓存，但尚无自动垃圾回收/缓存预算。缓存验证
  使用云 eTag、大小与完成记录；不提供用户在本地同长度篡改内容的哈希审计。
- 索引每页 100 项，任务快照最多 100 个运行/错误记录；界面分页位置由共享引擎保存。
  不承诺任意规模驱动器性能、多客户端独立分页会话或自动后台定时刷新。
- 六项默认 ignored FUSE 挂载测试本轮未执行，保持上一节列出的名称；独立 Mock smoke
  不替代它们。远程 CI、多屏/DPI/Explorer 重启矩阵未执行。
- **边界仍在 CFAPI 之前**：没有 sync-root 注册、placeholder、hydration 回调、上传、
  云端写操作、冲突解决、安装或签名。后续可基于持久 ID/索引/下载缓存开展独立 CFAPI
  设计，但本阶段不等于原生同步就绪；真实较大文件传输验收也尚待补齐。

### 源码、进程与产物

- Git：WSL `/root/workspaces/twodrive-windows`，分支
  `codex/windows-dual-preview-20260916`，HEAD `77ef808`；本轮及此前登录/浏览改动
  均未提交。无 push/main merge/PR/release/install；无无关 Linux 服务变更。
- Windows `E:/software/twodrive/native-build` 是镜像。只同步审阅过的本轮文件，统一
  Git 文本为 LF；既有 design/windows/frontend-redesign-20260916.md 镜像差异保留。
  build-windows.ps1 的 GetRelativePath 在 PowerShell 5.1 上失败，按故障证据修复；
  WSL 脚本保留原本兼容的路径实现，仅扩展完整 Flutter 测试。
- 起点源码/补丁另存 WSL `/root/workspaces/twodrive-before-index-download-20260917.tar.gz`
  和同名 `.patch`；未 reset/clean、未覆盖已知良好 cloud-browse-final 预览。
- 最终交付：`E:/software/twodrive/artifacts-metadata-cache-delivery-20260917`；
  SHA256SUMS 和两包 manifest 已核验，亦在 evidence 目录存有包哈希。
  早期 metadata-cache、final、verified 目录是保留的开发/验证产物，不是本次指定交付。
- 最终留运行：交付 Full engine PID 48872、Full PID 46688（PID 会变化），仍使用
  `E:/software/twodrive/windows-preview-state`，保留原登录和本次只读下载缓存。
  Full 最终停在持久文件页；没有保留含私人目录列表的截图到仓库。
- 开发失败与修复见 [incidents.md](incidents.md)：模块可见性/Clippy、PowerShell 中文
  管道编码、PowerShell 5.1 打包、跨批次删除。首次 WSL diff 检查因复制文本 CRLF
  失败，规范为 LF 后重跑通过；不把失败运行计为通过。原生 UI 自动刷新使旧 accessibility
  索引失效，重新截图定位后验证成功；一次导航即时检查尚未渲染，随后重读确认。
