# TwoDrive 故障记录

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

## TD-20261006-01：系统语言切换后特殊目录与上传来源错位

- 日期：2026-10-06
- 状态：已验证来源诊断与重选机制；系统目录变化的完整根因未确认，真实云端端到端未验证。
- 影响版本与环境：本机 Linux/GNOME，本次运行 daemon SHA-256 与此前本地调查已核实的 0.2.10-2 发布产物相同（`2500d0922ede9ffa736ccaa4611c3d670859d4cced59a14bf2533b8de1312332`）；修复版本 0.2.11-1。
- 关联历史故障：无相同已记录故障；[重构记录](refactor-2026-09-16.md) 涉及 known-folder 模块。本次提示缺口在重构前实现中也存在，未认定为重构回归。

### 症状与影响

切换中文到英文后，`~/Pictures` 仍为指向失效旧 OneDrive 路径的软链接，XDG 图片入口指向主目录，Documents、Videos、Templates 英文入口与旧中文内容目录分离。本机只读检查确认上述状态。人工将“下载”改为 Downloads 后，TwoDrive 仍配置 `~/下载 -> /Downloads`，未覆盖新目录；`~/图片 -> /Pictures` 来源仍存在。未确认图片丢失。

### 触发条件与复现

已失效旧软链接、桌面语言切换和显式上传路径与本地新名称不同。隔离测试创建“下载”并改名为 Downloads 后，旧来源缺失；旧版 status 没有错误提示，设置界面“Recent errors”固定显示 None recorded locally。另用临时目录模拟挂载来源，旧扫描允许把该目录内容上传到 MockBackend。

### 根因与证据

- **已确认的 TwoDrive 缺陷**：`known_folders/scan.rs` 对缺失来源仅在扫描时写后台日志，未启动监视；CLI 只显示映射，GTK Settings 没有来源健康状态或重选入口，却固定显示“没有错误”。因此用户难以发现失效来源或修复映射。扫描原先也未拒绝主目录/根目录/TwoDrive 挂载来源，本次补充一致校验以避免错误重选范围。
- **独立的配置变化**：人工改名造成 Downloads 映射失效，不能归因于 TwoDrive。TwoDrive 的 local 是显式路径，不跟随 XDG 或系统语言自动更名。
- **旧路径迁移检查**：当前安装/卸载脚本及仓库已检索历史没有创建该旧 OneDrive 软链接或改写 `user-dirs.dirs` 的实现；没有其他客户端特殊目录迁移功能。旧链接创建者、最初配置和迁移过程仍未确认，不能证明所有旧路径迁移已完成。
- [xdg-user-dirs-update 文档](https://manpages.debian.org/testing/xdg-user-dirs/xdg-user-dirs-update.1.en.html) 确认不存在的特殊目录可被重置到主目录，GUI 工具可处理语言更名；缺少当时执行日志，故仅作为与现场相符的机制，不能确认完整因果链。

### 解决办法与恢复操作

- 共享来源诊断，区分缺失目录、失效/被跳过的软链接、非目录、不可读取和不适合作为上传来源的路径。CLI status、JSON `known-folders status`、daemon 和 Settings 使用相同检查；识别 XDG 主目录回退与映射差异并解释自定义映射也可能合理。
- Settings 增加 Choose source…；`known-folders set-source <index> <directory>` 验证选中目录后原子保存单项 local，保留其他配置、未知选项和远端目标。界面保存校验原映射，避免使用过时行号覆盖已变化来源；失败保留配置。
- 拒绝主目录、文件系统根目录、挂载内部和包含挂载的来源，不把云端目录改作上传源。配置保存后需关闭挂载文件并重启 TwoDrive；运行中的 daemon 不热加载配置，启动时缺失来源恢复后也需重启。

**本地恢复操作**：本次未改动用户 XDG 设置、特殊目录、软链接、上传映射，未安装新包或重启真实服务。仅创建并卸载隔离测试挂载。Pictures 的系统入口和 Downloads 的实际映射仍需按用户期望选择；新代码不会自动迁移私人内容。

### 验证结果与边界

- `status_exposes_missing_upload_source_after_local_rename` 在未修改 CLI/诊断的旧代码上失败（status 无缺失提示），修复后通过。
- `mount_source_is_not_uploaded_into_itself` 在恢复旧扫描 guard 的代码上失败（MockBackend 收到模拟挂载目录内容），修复后通过；不是实际云端循环上传复现。
- 101 项 Rust 默认测试通过，6 项环境相关 FUSE 测试仍默认忽略；19 项 Nautilus、4 项 Settings 回归通过；cargo fmt、Clippy 警告视错误、git diff --check 通过。
- 回归覆盖 XDG 主目录回退与差异、中文显式路径不被猜测改名、失效/有效软链接、文件、相对路径、主目录与挂载别名、只读诊断不改配置、错误选择/过时来源保持配置、成功选择保留远端目标/未知配置/0600 权限、Settings 按钮映射及子进程错误显示。Settings 测试使用控件替身，不代表完整 GUI 点击验证。
- 使用本机 GTK4 创建了隐藏的真实 Settings 控件树和目录选择器（合成状态），构造通过；未执行完整 GUI 点击、选择与重启。
- 隔离真实 FUSE + MockBackend daemon 冒烟通过：挂载、读取、持久写入、模拟上传、移动/删除、pin/release；只操作临时数据，未使用真实令牌。
- `0.2.11-1` amd64 deb 已从干净的提交导出构建；解包 CLI/daemon 与 release 二进制、Settings 与源码字节一致，CLI 版本为 0.2.11。核对包中无运行令牌、用户配置或数据库。解包 CLI 在隔离配置中将缺失“下载”改为 Downloads 后状态由 missing 变为 ready，远端目标/0600 权限保留，XDG 文件与旧来源不被移动；包内 CLI/daemon 的真实 FUSE + MockBackend 冒烟通过。
- **边界**：未执行 GNOME 登录/语言切换全链路、真实 OneDrive 上传再下载比对、图片清单或完整性核对、用户原会话升级与 GUI 重选全流程。未证明该旧软链接由 TwoDrive 产生，未确认图片丢失，也不保证所有来源变化会自动恢复。

### 防复发措施与后续

保留显式来源语义；检测异常并由用户重选，不基于中文/英文名称猜测目录，不把 XDG 主目录回退当作上传范围。维护来源检查、配置保存与 GTK 回归；语言变更/改名后的用户操作与重启要求已同步写入中英文指南。

### 交付记录

- 修复源码：[6f3c40a](https://github.com/LumiaBlack51/twodrive/commit/6f3c40ada47bda2ad46811115b77ac1a748b6392)；打包验证记录：[9f3ade3](https://github.com/LumiaBlack51/twodrive/commit/9f3ade3aa7058a2c0b10a90c0a9b3a17c20bb466)。
- 已合并 [PR #7](https://github.com/LumiaBlack51/twodrive/pull/7)，合并提交/发布标签指向 [ae50648](https://github.com/LumiaBlack51/twodrive/commit/ae50648a4d08ae14f7a2041d3ab5210ff220430f)。合并树与已验证源代码/文档树一致。
- [Release v0.2.11-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.11-1) 已发布 amd64 deb 和 .deb.sha256；GitHub 两项资产 SHA-256 均与本地一致。deb SHA-256：`2904803b76277bb4d4305b3f2435b6a8419bdfcb91369da2c9bdf0fba234b7cf`。
- 包内故障记录为构建时验证快照；本次交付补记只更新已核实的发布链接与证据表述，不更改发布二进制。新包未安装到本机，真实服务保持原进程运行。

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
