# Windows 双发行版进度（2026-09-16）

## 当前接续点

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
