# TwoDrive 故障记录

## TD-20260917-DELTA-PENDING-DELETE：跨刷新删除非空目录可能遗留目录

- 日期：2026-09-17；状态：已修复并完成确定性回归；影响本轮未发布的索引实现。
- 关联：folder-move-sync-2026-09-11.md 涉及目录顺序，但不是本次只读索引根因。
### 症状与触发条件
目录删除标记先到，子项在后续 delta 才删除；首次刷新为保护子项保留目录。
### 根因与证据
审阅发现首次提交后清除了暂存删除标记，后续子项删除时无法识别待删目录。
没有证据表明真实账户发生过这种顺序。
### 解决办法与恢复操作
在持久条目中保留删除标记，每次提交所有移动/删除后清理无子项的已删除条目。
无真实云端修改，无本地账户恢复操作。
### 验证结果与边界
跨两次 delta 的目录/子项删除回归已验证旧代码失败（exit 101）、修复后通过（exit 0）。
游标写入失败事务回滚也通过；证据见 windows-evidence/metadata-cache。
真实 rename/move/delete 未执行，遵守只读验证约束。
### 防复发与交付
保留跨批次删除回归及事务回滚测试；未提交、未发布。

从 2026-09-15 起持续维护。每次故障新增条目，最近的条目放在前面；复发也单独记录并关联历史条目。维护流程见 [AGENTS.md](../AGENTS.md)。更早的调查保留在[文档索引](README.md#engineering-notes)，不追溯标记为已按本流程验证。

## 记录模板

复制以下模板填写。未知项明确写“未确认”或“未执行”，不要省略验证边界。

```markdown
## TD-YYYYMMDD-NN：故障标题

- 日期：
- 状态：调查中 / 已定位待修复 / 已修复待验证 / 已验证（注明范围）
- 影响版本与环境：
- 关联历史故障：无 / 条目编号或调查链接

### 症状与影响

用户可观察到的错误、影响的操作及数据影响；区分已确认影响与潜在风险。

### 触发条件与复现

最小操作步骤、必要状态、复现是否稳定。

### 根因与证据

出错的代码路径、因果链、必要的脱敏日志，以及排除或尚未排除的解释。
根因未确认时写明当前假设和缺少的证据。

### 解决办法与恢复操作

代码修改及其原理；如有本地恢复操作，另述操作范围和结果。

### 验证结果与边界

实际运行的测试、旧代码与修复后的结果、模拟/真实环境、未执行的验证。

### 防复发措施与后续

回归测试名称、需要保留的约束、未完成事项；复发时说明与历史修复的关系。

### 交付记录

已核实的提交、Release 和详细调查链接；尚未提交或发布时明确注明。
```

## TD-20260916-W03：文件页没有明确显示断连且重连遗留错误提示

- 日期：2026-09-16
- 状态：修复；3 项 Flutter 状态回归通过；最终原生界面断连/重连复验通过
- 影响版本与环境：本地 Windows Full 开发预览，未发布
- 关联历史故障：无

### 症状与影响

实机终止隔离测试后台后，文件列表被清空且按钮禁用，但页面只说没有文件，
没有明确说明断连。检查客户端发现成功重连的 snapshot 不会清除此前传输错误。
定时读取还会短暂显示“等待后台确认”，引起布局抖动。

### 触发条件与复现

Full 文件页连接隔离 Mock，终止本轮创建的 engine，再启动同一 state。
截图证据显示初版文件页的断连说明不足。未接触日常账户。

### 根因与证据

错误条仅放在概览 status()；files() 对 null snapshot 和空文件列表用同一个提示。
客户端仅在非 snapshot 成功时清 error。busy 同时用于轮询和用户命令。

### 解决办法与恢复操作

所有管理页显示连接状态/排队数与错误；断连使用独立说明并禁用操作；
成功重连清除 transport error；pendingMutation 区分读轮询与写请求。
仅重启一次性测试后台；没有修改实际同步状态。

### 验证结果与边界

Flutter 3 项回归验证断连清快照并禁操作、重连清错误、暂停等待后台确认、
不匹配响应 ID 拒绝应用。实机暂停与 UI->IPC->排队链路已验证；
最终修正版截图见本轮进度追加记录。CFAPI 和真实 Graph 未验证。

### 防复发措施与后续

apps/full/test/engine_client_test.dart 纳入打包脚本；界面不得乐观显示写操作成功。

### 交付记录

本地隔离分支，无 Release；见 [Windows 进度](windows-progress.md)。

## TD-20260916-W01：Windows Full 依赖解析成功但原生编译失败

- 日期：2026-09-16
- 状态：已定位并在本机原生 Release 构建验证修复；CI 未执行
- 影响版本与环境：Windows 新开发预览；Flutter 3.41.7、Dart 3.11.5
- 关联历史故障：无；不是既有 Linux 用户故障

### 症状与影响

flutter analyze lib 通过，但 flutter build windows --release 失败。
未安装、未发布该失败产物，无真实账户或文件受影响。

### 触发条件与复现

pubspec 使用 fluent_ui ^4.12.0，解析到 4.16.1；原生编译报
ScrollCacheExtent 不存在、ignorePointer/onReorderItem 参数不存在。

### 根因与证据

4.16.1 源码使用了本机 Flutter 没有的 API；仅分析应用 lib 不会完整编译
依赖。构建日志明确指向 pub 缓存中的 fluent_ui-4.16.1。
另发现 Windows Pub 联网 TLS 错误，阻碍获取兼容依赖；不是同一根因。

### 解决办法与恢复操作

固定 fluent_ui 4.12.0，提交 pubspec.lock；该版本官方元数据要求 Flutter >=3.32。
本机 Windows Pub TLS 失败时，用 WSL 官方 pub.dev HTTPS 下载归档，
对照官方 archive_sha256 核验后补充开发缓存，使用离线锁定解析。
未关闭 TLS、未升级 Flutter、未修改用户日常应用安装。
缓存是构建工具依赖，非 TwoDrive 用户状态。

### 验证结果与边界

同一 Flutter 上旧依赖原生 build 失败，固定后 release exe 构建成功。
增加双发行版脚本：必须检查 flutter build 和工具退出码，不能以 analyze
成功替代。CI 配置尚未运行，多架构未验证。

### 防复发措施与后续

锁定 Flutter 和依赖，CI 原生编译 Full；跟随未来 Flutter 升级时有意更新。
完整验证见 [Windows 进度](windows-progress.md)。

### 交付记录

本地隔离分支；未推送、未 Release；提交记录待本轮检查结束补记。

## TD-20260916-W02：原生 IPC 测试误用 Windows 默认文本编码

- 日期：2026-09-16
- 状态：修复并原生重跑通过
- 影响版本与环境：新测试脚本、中文 Windows Python 3.13，非引擎数据故障
- 关联历史故障：无

### 症状与影响

测试脚本输出 PASS，同时 stderr reader 线程出现 GBK UnicodeDecodeError。
该次输出不能作为干净验收证据。

### 触发条件与复现

subprocess.run(text=True) 未指定编码，读到 Rust UTF-8 本地化 OS 错误。
连接前/关闭后 IPC 失败属于本测试预期路径。

### 根因与证据

Python traceback 位于 subprocess readerthread；引擎 JSON/错误为 UTF-8，
Windows Python 默认编码为 GBK。并非管道消息截断或同步成功丢失。

### 解决办法与恢复操作

测试子进程显式 encoding="utf-8"；仅修改测试读取层，无本地账户恢复操作。

### 验证结果与边界

重跑原生 IPC 全流程无解码异常，上传/下载字节一致，断连与设置保留通过。
其它地区语言环境未全部测试；协议规范固定 UTF-8。

### 防复发措施与后续

全部 Windows IPC 自动化脚本显式编码；CI 保留原生测试。
没有将旧次带异常的 PASS 计为验收。

### 交付记录

本地隔离分支；未发布；证据见 [Windows 进度](windows-progress.md)。

## TD-20260916-01：C 编译产物在挂载中无法执行

- 日期：2026-09-16
- 状态：已验证本地权限机制修复（真实 FUSE 挂载 + MockBackend）；未验证真实 OneDrive 端到端。
- 影响版本与环境：修复前源码 `2f2607a`（基于 v0.2.9-1）；用户运行版本及具体产物未核实。修复版本 0.2.10-1。
- 关联历史故障：[早期可靠性修复](reliability-fix-2026-07-24.md)，其 setattr 兼容处理仅确认请求，没有保存 POSIX mode。本次补齐该处理遗漏的本地权限功能；与 TD-20260915-01 的本地创建记录丢失机制不同。

### 症状与影响

用户报告在 TwoDrive 创建 .c 文件后，编译结果无法执行。隔离复现中编译成功，但运行报 Permission denied；chmod 调用成功却不改变可见权限。

### 触发条件与复现

在隔离 MockBackend FUSE 挂载中创建 `hello.c`，内容为 `int main(void) { return 0; }`，执行 `cc hello.c -o hello`，再执行产物并尝试 `chmod 755 hello`。修复前产物及 chmod 后权限均为 0644，执行报 errno 13。将同一源文件编译输出到本地普通目录后可运行。

### 根因与证据

修复前 `crates/twodrive-fs/src/lib.rs` 的 `attr_for_record` 把普通文件权限固定为 0644；`create` 忽略 mode/umask；`setattr` 忽略 mode。因此无法保留编译器或 chmod 设置的执行位。初次使用现有 debug 二进制复现（其构建提交未核实）后，又将新增编译/挂载回归测试放到未修改的源码上重新构建：测试稳定失败，实际权限为 `0o100644`。修复后同一编译执行流程通过。本机原挂载选项未见 noexec。

### 解决办法与恢复操作

- 为 SQLite files 增加可空的 local_mode 字段，按稳定本地身份保存普通 rwx 位。迁移可重复执行；旧条目及新导入的云端条目仍默认文件 0644、目录 0755。
- 创建文件和目录时，把 `mode & !umask & 0777` 与本地创建记录一起持久化；chmod 更新数据库及 inode 视图，不产生内容上传任务。启用 FUSE default_permissions，让内核检查本地权限。
- 上传确认、单条/批量云端更新、释放缓存及重命名保留本地权限；原子替换使用来源文件权限，不能错误继承目标文件权限。重挂载从数据库恢复。
- 已有编译产物升级并重挂载后可用 `chmod +x 文件名` 恢复执行，也可重新编译。权限只保存在本机，不作为云端元数据同步。

本地恢复操作：仅创建并卸载隔离测试挂载；未安装到用户运行目录、重启用户服务或修改用户的云端文件。deb 更新需另行安装并重挂载后生效。

### 验证结果与边界

- 88 项默认 Rust 测试通过；19 项 Nautilus 测试通过；cargo fmt、Clippy（警告视为错误）及 git diff --check 通过。
- 6 项通常忽略的实际 FUSE 测试已显式执行并通过。首次批量运行中释放下载测试因未设置 TWODRIVE_TEST_CLI 而未执行到测试主体；构建新 CLI 并设置该变量后单独重跑通过，其余 5 项首次通过。
- 新增挂载测试验证直接创建权限、编译执行、umask、移除执行位后拒绝执行、恢复执行位、移除读位后拒绝读取、目录权限及文件重命名后重挂载执行。
- 数据库回归覆盖本地创建、chmod 不改变内容状态、上传确认、两种云端导入、释放缓存、重复初始化、原子替换权限以及删除后重新导入恢复默认策略。既有旧数据库迁移测试增加权限字段兼容及重复迁移保留设置的断言。
- 从生成的 0.2.10-1 deb 解包，CLI/daemon 与 release 构建字节一致，CLI 报告 0.2.10。使用解包 CLI 在隔离 mock 挂载中编译、执行、chmod 拒绝/恢复执行及重挂载执行均通过。
- **验证边界**：实际挂载测试使用 MockBackend，不代表真实 OneDrive 上传下载端到端验证；重挂载执行使用本地缓存。没有核对用户具体编译产物；未安装系统 deb 或验证用户原会话。完整 uid/gid、特殊模式位、已解除链接句柄的 chmod 及目录时间戳不属于此次实现；挂载根目录 mode 固定，chmod 返回不支持。

### 防复发措施与后续

新增 `compiled_program_and_chmod_survive_remount`（需 FUSE、fusermount3、python3、cc，默认忽略）及 `local_permissions_survive_cloud_updates_release_and_replacement`。修改创建、setattr、数据库导入、替换或迁移时须保留“本地权限不被云端元数据覆盖”约束，发布时显式运行挂载测试。实际云端验证仍待执行，不承诺所有编译或执行问题均已消除。

### 交付记录

- 修复提交：[b9fa018](https://github.com/LumiaBlack51/twodrive/commit/b9fa01888c91a6a6696c76aa74d52819985e40a6)。
- 已合并 [PR #3](https://github.com/LumiaBlack51/twodrive/pull/3)，合并提交：[37234e4](https://github.com/LumiaBlack51/twodrive/commit/37234e49a982190dbe01f72608c69c783ee8e864)。合并树与已测试构建输入一致。
- Release：[TwoDrive 0.2.10-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.10-1)，已发布 amd64 deb 及 sha256 文件；GitHub 报告的两项资产 SHA-256 均与本地一致。
- 安装包中的故障记录为发布前验证快照；本次补记仅更新交付链接，不修改发布二进制。

## TD-20260915-01：新建目录中解压文件报 I/O 错误

- 日期：2026-09-15
- 状态：已验证本次故障机制修复；真实 OneDrive 完整上传、下载比对尚未执行。
- 影响版本与环境：修复前源码包含于 0.2.8；故障发生在 Linux/Nautilus + TwoDrive FUSE 挂载。本机旧程序早于本次发布，具体包版本未核实；不据此断言最早受影响版本。
- 关联历史故障：[父目录移动与子项同步顺序](folder-move-sync-2026-09-11.md)。关联在于保护待处理目录的逻辑；没有证据证明此前所有 I/O 错误都是本次根因。

### 症状与影响

向 OneDrive 挂载中新建的目录解压 ZIP，创建 `__MACOSX/._…` 文件时出现输入/输出错误。守护进程记录：`twodrive create upload error: created upload record disappeared`。

失败会中断解压并留下部分结果。普通文件也受影响，`._` 文件名只是本次最先暴露问题的位置。原 ZIP 的 48 个条目通过 CRC 检查；未发现原压缩包损坏的证据。

### 触发条件与复现

1. 在本地建立目录，使其云端创建操作仍处于待处理状态。
2. 在其中建立嵌套目录并创建文件，例如解压 ZIP。
3. 在父目录同步完成前执行本地文件创建。

隔离测试中保留父目录待处理状态，可稳定触发旧代码错误。待移动父目录使用同一过滤机制；新增解压测试直接覆盖的是待创建目录。

### 根因与证据

`TwoDriveFs::create_upload` 将本地新建文件交给 `Database::upsert_metadata`，随后再分别登记缓存和 Writing 状态。

`upsert_metadata` 用于云端元数据导入。为防止过时云端数据破坏尚未完成的本地目录操作，它会跳过待创建或待移动目录下的条目。此次本地写入也被跳过，函数却正常返回；后续查询找不到记录，最终向 FUSE 返回 EIO。

将新增回归测试放回旧 `create_upload`，稳定得到与生产日志完全相同的错误。修复后同一测试通过。原来的分步写入还暴露了中间状态；本次一起消除了这个窗口，但没有把它当成已证实的另一条生产故障原因。

### 解决办法与恢复操作

新增 `Database::create_local_file`：通过单条 SQLite INSERT 同时登记本地身份、路径、缓存路径、访问时间和 Writing 状态。本地创建不再经过云端导入过滤；唯一约束防止覆盖同名记录。

保留云端旧元数据过滤和父目录操作完成前暂停上传的规则。新建内容由写入句柄持有缓存锁，后续按原有恢复流程上传。

本地恢复：备份数据库和旧程序后更新用户目录下的 CLI、daemon，并重启服务；挂载恢复，真实 OneDrive 元数据同步成功。没有自动合并或删除历史部分解压结果和无关云端冲突。

### 验证结果与边界

- 87 项默认 Rust 测试、19 项 Nautilus 测试通过；格式检查及 Clippy（警告视为错误）通过。
- 5 项通常忽略的实际 FUSE 挂载测试全部显式执行并通过，包含待处理目录中的 ZIP 解压。
- 普通文件和 AppleDouble 文件在父目录待处理时保留正确字节，父目录恢复后向模拟云端正确上传；过时元数据未替换本地记录。
- 验证了待删除路径重用及同名创建冲突时保护现有记录。
- 安装包内容和构建输入一致，GitHub 安装包摘要与本地 SHA-256 一致。
- **验证边界**：挂载测试的云端为 MockBackend。真实 OneDrive 只验证了元数据同步与原 ZIP 读取校验，未完成原 ZIP 的“解压 → 上传 → 重新下载逐文件比对”。不能据此保证任何来源的 I/O 错误都不会再发生。

### 防复发措施与后续

已提交的回归测试：

- `archive_files_in_pending_nested_folders_survive_until_upload`
- `zip_extraction_into_pending_folder_on_mount`（需要 FUSE，默认忽略，相关发布验证应显式运行）
- `local_create_is_atomic_and_can_reuse_a_pending_delete_path`

后续修改本地创建、云端导入过滤或父子同步顺序时，应运行上述相关测试，保留“本地创建不能被云端过滤跳过”和“子项上传等待父目录完成”两项约束。

未完成的验证：真实 OneDrive 原 ZIP 端到端比对。若再次出现相同报错，应先核实运行程序版本、触发状态和日志，再判断是否属于本次机制的回归。

### 交付记录

- 根因修复：[c84714f](https://github.com/LumiaBlack51/twodrive/commit/c84714f)
- 同批发布的既有 Nautilus 修复：[030132f](https://github.com/LumiaBlack51/twodrive/commit/030132f)，与解压故障根因不同。
- Release：[TwoDrive 0.2.9-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.9-1)
- 详细调查：[Archive extraction I/O error](archive-extraction-2026-09-15.md)

## TD-20260916-UI：Flutter 默认测试仍引用已移除的 MyApp

- 状态：已修复；完整 Flutter 6 项测试通过，静态分析无问题。
- 触发条件：前端重设计时检查完整 Flutter 测试入口。
- 根因与证据：`apps/full/test/widget_test.dart` 仍为默认计数器测试，构造不存在的 `MyApp`；实际入口为 `TwoDrive`。与此前 FUSE 故障无关。
- 解决办法：替换为真实界面的托盘/管理中心布局、导航、暂停确认与断连禁用测试，测试使用注入的隔离数据。
- 本地恢复：无。验证边界：不涉及真实 OneDrive、原生托盘窗口行为或 CFAPI。
- 防复发：后续前端修改运行完整 `flutter test` 与 `flutter analyze`。

## TD-20260916-WIN-AUTH-TEST：后端测试无条件引用 Unix 权限 API

- 日期：2026-09-16
- 状态：已定位，修复验证中。
- 影响版本与环境：0.2.10 Windows 原生后端测试编译；不影响已有运行程序。
- 关联历史故障：与 Flutter 默认测试故障无相同根因。
- 症状与复现：执行 cargo test -p twodrive-windows -p twodrive-backend --locked，报 std::os::unix 不存在和 Permissions.mode 不存在。
- 根因与证据：onedrive/tests.rs 无条件导入 PermissionsExt 并检查 Unix mode。
- 解决办法：仅在 Unix 上编译 mode 检查，Windows 继续执行实际加密存储和读取往返测试。
- 本地恢复：无；未删除账户或数据。
- 验证边界：测试结果待补；模拟令牌不代表真实 Microsoft 登录成功。
- 防复发：Windows 后端测试加入本次验证命令。

验证补记：Windows 后端 24 项与 Windows 引擎 6 项测试均通过；Clippy 无警告。Unix 权限断言仅在 Unix 编译，Windows 加密存储往返通过。真实 Microsoft 登录与云端同步不属于本条测试结论。

## TD-20260916-BROWSE-DEV：浏览开发验证受文件占用和编码影响

- 状态：原因确认，验证重跑中。
- 触发：真实目录探测从 target/debug 启动引擎后运行 cargo test；另一个编辑辅助脚本使用 Python 默认文本编码。
- 症状/证据：Windows 无法删除正在运行的 twodrive-engine.exe（os error 5）；Python read_text 报 GBK UnicodeDecodeError。
- 根因：验证程序占用重建目标；中文源码与 Windows 默认编码不匹配。编码机制与 TD-20260916-W02 相邻，但此次是编辑辅助脚本，不是 IPC 输出。
- 处理：运行引擎移到独立 browse-development-runtime 副本；编辑脚本显式 UTF-8。保留既有产物/登录凭据，不修改云端。
- 验证边界：失败运行不计通过；随后结果见 windows-progress.md。新增构建验证都从独立产物运行，不再占用 target。

## TD-20260916-BROWSE-CANCEL：空查询身份仍被客户端接收

- 状态：确定性回归发现并修复，完整重跑中。
- 触发：取消查询/注销立即清除 queryId 后，传输返回 query_id=null 的目录对象。
- 根因/证据：原客户端仅判断 incoming.query_id != queryId，null == null 时仍接收；新增 cloud_browse_test 的取消/注销测试首次失败，出现不该恢复的条目。
- 修复：没有非空活动 queryId 时不接收任何目录数据。引擎另以取消标记、登录状态、查询 ID 三重检查阻止迟到 worker 发布。
- 验证：新增快速切换、取消、注销、分页去重测试；结果补入 windows-progress.md。测试用合成目录，未退出当前真实账户。
- 恢复操作：无。防复发：保留该回归；完整 Flutter 测试与 Rust 迟到完成测试纳入本阶段检查。

验证补记（2026-09-17）：取消空查询修复后客户端回归通过。新增 widget 测试首次因测试 fixture 的 Map.addAll 泛型不匹配失败，改为显式 Map<String,dynamic> 后完整 11 项通过；这不是生产 JSON 解析故障。Dart format 暴露三个缺花括号的 lint（两处新代码、一处已有测试），首轮构建因此被静态检查拦截；已加花括号，重新打包，首次失败日志独立保留。

## TD-20260917-RETRY-AFTER：共用 HTTP 重试器提前重试长限流

- 状态：代码审阅确认机制，修复后验证中。
- 触发：Graph/OAuth 返回 Retry-After 大于 30 秒；既有辅助函数将其截为 30 秒。
- 证据：原 `retry_after_seconds_are_honored_with_a_small_cap` 明确断言 120 秒变为 30 秒。没有声称当前真实账户发生过限流。
- 影响：现有刷新复用该重试器，可能早于服务端允许的时间重试；与登录增量无凭据丢失关系。
- 修复：保留完整秒数及 HTTP 日期；超出单次重试预算时停止自动重试而非缩短等待。保留结构化 HTTP 状态/延迟，不包含响应正文；浏览端持有共享等待截止时间，后续查询也不能绕过刷新限流。授权网络故障保持网络/超时分类。
- 测试：合成 HTTP 刷新返回 429/120 秒，两次查询只发出一次刷新请求，保存的凭据仍在；HTTP 日期解析、原 120 秒断言已更新。完整平台结果见 windows-progress.md。
- 本地恢复：无；未改真实权限、云端文件或既有登录。

最终补记：TD-20260916-BROWSE-DEV、TD-20260916-BROWSE-CANCEL、TD-20260917-RETRY-AFTER 已完成本轮修复后重跑；Windows Rust 42、Linux Rust 115、Flutter 11 通过，静态检查通过，6 项 FUSE ignored 未执行。当前真实账户只读根/子目录实测通过；真实异常注入及真实多页未执行。构建与证据见 windows-progress.md 2026-09-17 交付节，不将 mock HTTP 结果称为真实异常恢复验证。


## TD-20260917-INDEX-DOWNLOAD-DEV: Native milestone compilation and verification

- Date: 2026-09-17.
- Status: development checks being rerun; no released or running account data affected.
- Environment: uncommitted Windows 0.2.10 metadata/cache milestone.
- Related history: TD-20260916-BROWSE-DEV (verification workflow only).

### Symptoms and reproduction

Initial cargo check rejected four methods private to the new engine child module.
The first warnings-denied Clippy run rejected two cloned single-element slices in new tests.

### Root cause and evidence

Rust child-module method visibility does not expose private methods to the parent;
compiler E0624 identified the four calls. Clippy identified cloned_ref_to_slice_refs.

### Fix and recovery

Use pub(super) for the four engine entry points and std::slice::from_ref in tests.
No local account recovery, cloud mutation, or existing preview replacement occurred.

### Verification and limits

Native cargo check passed after visibility fixes; deterministic readonly HTTP tests passed.
Full final verification remains in progress and will be recorded in windows-progress.md.
These are development failures, not evidence of real Graph failure or recovery.

### Prevention and delivery

Keep native check, all-target Clippy and deterministic tests in milestone validation.
Uncommitted; no release or remote publication.


## TD-20260917-INDEX-UI-ENCODING: New UI strings lost in PowerShell stdin

- Date: 2026-09-17; status: fixed, widget verification passed.
- Affected environment: uncommitted new index UI only; no cloud data impact.
- Related incident: TD-20260916-W02; a different encoding boundary (PowerShell to Python stdin).

### Symptoms and reproduction

Reading newly generated Dart source showed question marks replacing Chinese labels;
Flutter analyze could not detect this semantic text loss.

### Root cause and evidence

PowerShell's default pipe encoding replaced non-ASCII Python source before execution.
The Dart source itself contained literal question marks.

### Fix and local recovery

Replaced only the new affected strings with UTF-8 patches. Added a widget test asserting
real labels and IPC refresh/download/open commands. No account recovery was needed.

### Verification and limits

The index widget and full Flutter suite passed after correction. Native UI verification
is tracked in windows-progress.md. No real Graph fault is inferred from this issue.

### Prevention and delivery

Use direct UTF-8 patches or ASCII-only helper scripts; inspect visible UI labels.
Uncommitted, no release. Development Clippy also found collapsible_if and test-module
ordering; both were corrected and warnings-denied native Clippy passed.


## TD-20260917-PACKAGING-PS51: Release packaging requires unavailable Path API

- Date: 2026-09-17; status: fixed, new-directory package rerun pending.
- Environment: Windows PowerShell 5.1, native Windows build mirror script.
- Related: TD-20260916-W01; different root cause, packaging after successful compilation.

### Symptoms and reproduction

Full Rust/Flutter release compilation succeeded; manifest generation failed because
System.IO.Path.GetRelativePath does not exist in this PowerShell/.NET Framework.
The failed artifact directory is retained and is not a delivered package.

### Root cause and evidence

build-windows.ps1 called a newer .NET API under the system PowerShell runtime.

### Fix and recovery

Derive each enumerated child file path relative to the known package prefix using
Substring; no external paths are enumerated. Expand packaging verification to full
Flutter analyze/test. Build again in a new output directory, preserving known-good previews.

### Verification, prevention and delivery

Rerun status and per-file manifest/hash verification are recorded in windows-progress.md.
No installer, cloud mutation or publication. Keep native packaging in release validation.
Uncommitted; release link not applicable.


## TD-20260917-INDEX-SIZE: Invalid file size could be treated as zero

- Date: 2026-09-17; status: corrected and deterministic regression passed.
- Environment: new uncommitted read-only metadata parser.
- Related history: folder-move-sync-2026-09-11.md negative size handling; same input
  class in a newly introduced parser, not a regression in the Linux parser.

### Symptoms, trigger and root cause

Code review found as_u64().unwrap_or(0) accepted missing/negative file sizes as zero.
This could create an empty cache candidate. No real account occurrence was observed.

### Fix and recovery

Require a nonnegative integer size for supported live file items; folder aggregate sizes
remain non-authoritative. No local recovery or real cloud operation was needed.

### Verification and prevention

invalid_file_size_is_never_an_empty_cache_candidate passes; final native/Linux suites
pass. Real invalid Graph responses were not induced. Preserve strict file-size checks.

### Delivery

Uncommitted; evidence and limits in windows-progress.md.

## TD-20260917-INDEX-SUMMARY: Full overview used obsolete cache counters

- Date: 2026-09-17; status: fixed and native UI verified.
- Environment: initial new index/cache preview; previous browsing architecture retained.
- Related history: TD-20260916-W03 concerned disconnected state, not these counters.

### Symptoms and trigger

After a real small file reached cached in the index, Full overview still showed 0 B
because it read the legacy mock files collection rather than the new persisted cloud view.

### Root cause and fix

The new file page used cloud state, while overview retained its previous data source.
Add engine-computed file_count/cached_bytes to CloudView and use these for real-mode
summary. Account status explicitly describes read-only indexing/download cache.

### Verification and boundary

Native Full first showed 46 files and 1.1 KiB, then 2.2 KiB after the second 1,132-byte
real download; Lite/IPC returned 46 files and 2,264 cached bytes. Final Full UI bytes match
the inspected build; full Flutter suite/analyze and final release build pass.
No cloud metadata/content mutation, no local account repair, no CFAPI claim.

### Prevention and delivery

Keep summary and file-page state sourced from CloudView; maintain native shared-state
acceptance and widget tests. Uncommitted; no release.

### 2026-09-17 final development verification addendum

INDEX-DOWNLOAD-DEV, INDEX-UI-ENCODING, PACKAGING-PS51 and DELTA-PENDING-DELETE
fixes passed final verification: Windows Rust 85, Linux Rust 127, Flutter 12, Nautilus 19;
Clippy warnings denied, fmt, analyze, Full/Lite release and artifact hash verification pass.
Six ignored FUSE tests remain unexecuted. CRLF copied into WSL initially failed diff
whitespace checks; changed text was normalized to LF and checks rerun successfully.
A helper UNC path inspection failed (WinError 64); no files were changed by that failed
inspection, and subsequent comparison/copy used WSL-local Python with explicit paths.
See windows-progress.md and windows-evidence/metadata-cache/results.json for real-account
results and remaining large-file acceptance blockage. No failures/skips count as passes.


## TD-20261007-PR-WSL：提交前 fetch 的宿主调用超时

- 状态：重试成功；一次 WSL 调用返回 Wsl/Service/WSAETIMEDOUT，根因未确认。
- 影响：首次远端基线检查未完成，不计通过；无应用/凭据/云端数据变化。
- 处理：重新通过 WSL 执行有界 git fetch，仅更新远端跟踪引用，成功取得 main bdacfb9。
- 验证：随后 Git 查询、workspace 测试 127 passed/6 ignored、Clippy 均成功。
- 边界/后续：不归因为 TwoDrive 缺陷；保留超时事实，网络操作继续使用有界等待。

提交检查补记：git diff --cached --check 发现此前未跟踪证据中的 CRLF、终端尾部空格及 EOF 空行；仅规范文本空白后重跑，保留日志内容与所有成功/失败结论。旧 git diff --check 未包含未跟踪文件，不能代表这些文件此前已通过暂存检查。
