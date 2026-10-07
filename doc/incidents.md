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



## TD-20261007-03：整合树 Windows 原生 peer 测试失败

- 日期：2026-10-07
- 状态：已验证 Windows 原生测试编译与执行修复；Linux/Windows peer 全流程通过。
- 影响版本与环境：dev 805ceba；GitHub windows-latest，Peer native builds 37593664900。
- 关联历史故障：与 TD-20260916-WIN-AUTH-TEST 同为测试缺少平台限定；本次是较新 main 新增的 known-folder 夹具，不是旧 OAuth 修复回归。TD-20261006-04 是生产依赖隔离，与本次编译目标不同。

### 症状与影响

Windows 的 cargo test -p twodrive-peer -p twodrive-core -p twodrive-backend --locked 在测试编译阶段失败；Linux 全工作区测试通过。该 job 未交付 Windows peer 资产；并行 Windows Full/Lite 构建仍在执行。未改动用户安装、配置或真实云端。

### 触发条件与复现

合并 Windows PR #9 和 peer-control 后首次原生 CI；运行包含 core 单元测试的 Windows 命令即可触发。Windows 预览历史流程只测试 Windows 引擎/后端，没有编译 core 的 cfg(test) 目标；peer-control 历史基线尚无该夹具。

### 根因与证据

known_folders.rs:229 的 tests 模块无条件导入 std::os::unix::fs::symlink；Windows 报 E0433 cannot find unix in os。该夹具由 main 的 6f3c40a 加入，检查 Unix/XDG 根目录和符号链接。生产 core 可编译；不是 P2P 握手或 QUIC 加密机制故障。

### 解决办法与恢复操作

仅为该 Unix 夹具及其 symlink 导入添加 cfg(unix)。保留不依赖 Unix 系统调用的 XDG 解析测试和所有其他 core/后端/peer Windows 测试，不绕过整套原生验证。生产路径未改动；没有执行用户环境恢复操作。

### 验证结果与边界

旧树 805ceba 在原生 CI 明确编译失败。修正后本地 backend/core/peer 94 项默认测试通过，原 Unix 夹具仍在 Linux 执行；fmt 和严格 Clippy 通过。c73fece 原生 Windows 93 项默认测试通过（仅该 Unix 夹具未编译），额外更新进程测试通过；Linux 151 项默认工作区测试和额外更新进程测试通过。两平台 peer release、自检和资产上传均通过；Windows DPAPI 往返、私有身份持久化及系统 DLL 检查通过。均使用合成状态，未验证真实 OneDrive 或异地设备握手。

### 防复发措施与后续

保留 Windows core 单元测试编译与执行在 peer CI 中；这是此次可复现构建缺陷的回归门槛。新增平台夹具必须使用对应 cfg，不能只验证生产 library 或上层引擎测试。

### 交付记录

[整合提交 805ceba](https://github.com/LumiaBlack51/twodrive/commit/805ceba6616efd6d77014736881ad1e9d74a6528)，[失败 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37593664900/job/112700935062)；[修复 c73fece](https://github.com/LumiaBlack51/twodrive/commit/c73fece6b8e9215b562ea88317a31f9f5723cf7e)，[成功原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37594656006)。peer 资产 [Windows](https://github.com/LumiaBlack51/twodrive/actions/runs/37594656006/artifacts/11469772936) / [Linux](https://github.com/LumiaBlack51/twodrive/actions/runs/37594656006/artifacts/11470636670)。未发布 Release 或替换用户安装。


## TD-20261007-02：dev 合并后的隔离 FUSE 上传冒烟超时

- 日期：2026-10-07
- 状态：已定位并验证冒烟等待边界；保留既有 60 秒重试语义。
- 影响版本与环境：未提交的 Windows PR #9 + peer-control + dev 整合树，Linux 临时目录与真实 FUSE。
- 关联历史故障：TD-20261006-02/03 是 WebDAV 上传/ETag 适配；本次仅确认挂载上传超时，不据症状判定同源。

### 症状与影响

双 CLI LAN 直连的 GET/PUT/MOVE/DELETE 阶段已完成，挂载读取也已完成；创建 new-folder/written.txt 并 fsync 后，等待服务器字节一致超过 45 秒。测试数据是临时合成内容；未操作用户安装、账户、云端或现有挂载。

### 触发条件与复现

cargo build 实验 --features mount 后，scripts/smoke.py --network lan --mount 首次运行超时（exit 1）。原脚本退出清理临时目录，正常终止进程时未输出失败日志，尚无后台错误证据。

### 根因与证据

不是已确认的合并回归。恢复留档后再次以 45 秒截止运行仍失败：父目录已在服务器创建且本地 metadata 操作已清空；子文件保留在本地 dirty 缓存，没有后台传输错误。`recover_dirty_record` 在父目录操作未完成时返回 false，worker 本轮结束；`mount_backend` 的周期恢复每 60 秒重新排队。两文件与原 dev 71c31e0 完全相同。将该测试等待范围覆盖完整重试周期后，真实挂载在 59.8 秒确认服务器内容，符合该机制。

### 解决办法与恢复操作

修正测试截止时间：挂载保存及依赖操作允许 90 秒覆盖已有 60 秒恢复周期；增加失败日志和显式 --keep-state 保留一次性证据。没有改变引擎调度，也没有把后台尚未完成的本地保存显示为成功上传。仅正常退出并卸载该测试自己的挂载，未改动真实服务。

### 验证结果与边界

本轮 151 项主工作区默认 Rust 测试、10 项 dev 测试、严格 Clippy/格式、19 Nautilus 和 4 Settings 测试通过；额外 peer 更新子进程和引擎 IPC 通过。原 45 秒冒烟两次失败；修正测试边界后真实 FUSE + QUIC LAN 冒烟通过读取、完整字节保存/后台上传、目录移动与删除（保存耗时实测 59.8 秒）。独立只读冒烟通过，内核写入返回 EROFS，服务器 PUT 被拒绝。不能声称立即上传，不能将本机实验当作异地 NAT 或真实 OneDrive 端到端。

### 防复发措施与后续

保留真实挂载字节比对、失败日志/状态选项，以及包含实际恢复周期的截止时间。该验证修正不改善现有子文件等待延迟；若后续优化父目录完成后的子项唤醒，须另做调度机制回归。

### 交付记录

[整合提交 805ceba](https://github.com/LumiaBlack51/twodrive/commit/805ceba6616efd6d77014736881ad1e9d74a6528)，最终运行代码 [c73fece](https://github.com/LumiaBlack51/twodrive/commit/c73fece6b8e9215b562ea88317a31f9f5723cf7e)。[WebDAV/QUIC 原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37594655979) Linux/Windows 全部成功：Linux 10、Windows 8 项测试及 2,097,105 字节双进程 LAN 比对通过；CI 不运行 FUSE，实际挂载结果为本条所述本地验证。未发布 Release 或替换用户安装。


## TD-20261007-01：dev 多分支合并工具截断工作区 manifest

- 日期：2026-10-07
- 状态：已验证 Linux/Windows 合并 manifest、编译、协议与组合包回归。
- 影响版本与环境：未提交的 dev + Windows PR #9 + peer-control 合并树；仅隔离 worktree。
- 关联历史故障：TD-20261006-04 是 Unix 平台依赖问题；本次是合并工具机制，不属同源。

### 症状与影响

合并冲突消解后的 cargo metadata 报 Cargo.toml 第 8 行 unclosed array。尚未编译或交付该中间树；原 main 工作目录、安装、服务和用户数据未改动。

### 触发条件与复现

合并 codex/peer-control 时，临时 Python helper 对多行 Git 冲突块使用 DOTALL 正则，起止标记中的 .* 也可跨行匹配。工作区 members 尾部及后续字段被误吞；manifest 检查立即失败（退出 101）。

### 根因与证据

此问题属于本轮合并操作错误。起止标记应限制到单行，不能贪婪匹配后续完整内容。不是源 Windows 或 peer 实现本身的 Cargo 语法错误。

### 解决办法与恢复操作

从已核实的 Windows 合并父提交恢复完整 Cargo.toml、核心 manifest 和较新的 OAuth 实现；明确加入 peer member。Cargo.lock 按父提交的完整 package 段合并，并保留既有锁定版本，再由 Cargo 重新核对实际依赖。Gitignore 同时保留 Windows 产物与 peer 秘钥排除规则。恢复操作仅作用于本轮合并文件，无用户配置/文件恢复。

### 验证结果与边界

恢复后 cargo metadata --no-deps --offline 成功解析；离线更新合并依赖图后 --locked 编译通过。主工作区 151 项默认 Rust 测试通过（6 项 FUSE 和 1 项更新进程默认忽略），另显式执行更新进程测试通过；dev 的 1 项兼容性 + 9 项协议测试、两工作区 fmt/严格 Clippy、19 Nautilus、4 Settings 通过。Linux 独立引擎进程的 IPC/单实例/重启/字节比对通过。

最终 c73fece 原生 Linux/Windows 的三条整合 CI 全部成功：Windows core/backend/peer 93 项、额外更新进程、Windows 引擎 11 项、Flutter 12 项、analyze/release、命名管道 IPC、Full/Lite 中同一工具哈希及实际打包 QUIC 双进程传输通过。Windows dev 8 项协议测试通过。下载最终组合资产后，独立验证 Full 25 / Lite 9 文件的 manifest 长度/哈希、两 ZIP 校验值与内容、四个工具跨版一致。真实云端、异地 NAT、用户设备和 CFAPI 未验证，6 项既有 FUSE ignored 未在本轮执行。

### 防复发措施与后续

保留源父提交恢复边界，manifest 使用标准 TOML 解析校验；冲突标记按单行限制，不通过文本截取拼接 package 半段。提交前运行格式、严格 Clippy、两工作区测试和原生 CI，并保留所有已有故障条目。

### 交付记录

Windows 来源 5120cf7（[PR #9，已合并到 dev](https://github.com/LumiaBlack51/twodrive/pull/9)），peer 来源 dd0686b，原 dev 71c31e0；Windows 合并 d3c9e32，peer/包整合 [805ceba](https://github.com/LumiaBlack51/twodrive/commit/805ceba6616efd6d77014736881ad1e9d74a6528)，平台测试限定 [c73fece](https://github.com/LumiaBlack51/twodrive/commit/c73fece6b8e9215b562ea88317a31f9f5723cf7e)。[Windows 组合包 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37594661999)、[peer CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37594656006)、[WebDAV/QUIC CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37594655979) 均成功。已核验[组合资产](https://github.com/LumiaBlack51/twodrive/actions/runs/37594661999/artifacts/11469933545)来源为 c73fece；Full ZIP SHA-256 `49c2ab6540228984745233bbed51b9235f38a1148c880c12566318d0c68b6552`，Lite ZIP `0b4663b87ab01e72c23836350e11df4263c170139cb44d3ad541ded7c992835e`。仅记录更新在构建后提交，不改变可执行文件的源码/锁文件/构建脚本；资产内文档是该构建的快照。未合并 main、发布 Release 或替换用户安装。


## TD-20261007-PR-WSL：提交前 fetch 的宿主调用超时

- 状态：重试成功；一次 WSL 调用返回 Wsl/Service/WSAETIMEDOUT，根因未确认。
- 影响：首次远端基线检查未完成，不计通过；无应用/凭据/云端数据变化。
- 处理：重新通过 WSL 执行有界 git fetch，仅更新远端跟踪引用，成功取得 main bdacfb9。
- 验证：随后 Git 查询、workspace 测试 127 passed/6 ignored、Clippy 均成功。
- 边界/后续：不归因为 TwoDrive 缺陷；保留超时事实，网络操作继续使用有界等待。

提交检查补记：git diff --cached --check 发现此前未跟踪证据中的 CRLF、终端尾部空格及 EOF 空行；仅规范文本空白后重跑，保留日志内容与所有成功/失败结论。旧 git diff --check 未包含未跟踪文件，不能代表这些文件此前已通过暂存检查。

TD-20261007-PR-WSL 补记：创建 PR 后一次新 WSL 调用再次在启动阶段超时；后续核对工作树干净、HEAD 174a12b，确认该次没有文档写入或提交。重试读取 PR 状态成功；宿主间歇启动超时根因仍未定位，不影响已创建 PR。
## TD-20261006-04：dev Windows 原生编译检查失败

- 日期：2026-10-06
- 状态：已验证（原生 Linux/Windows 编译、协议测试与双进程 CLI）。
- 影响版本与环境：dev 提交 `0222663`，GitHub windows-latest，静态 CRT 编译参数。
- 关联历史故障：TD-20261006-02/03 属于 Linux 协议/文件适配问题；当前未认定同源。

### 症状与影响

Windows fmt 通过，Clippy/编译步骤失败，后续测试与产物构建被跳过；尚未交付 Windows 可用性。

### 触发条件与复现

[原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37469772524/job/112289915114) 首次编译。同次 Linux CI fmt/Clippy/测试、release 构建、双 CLI 进程传输与资产上传全部通过。

### 根因与证据

`twodrive-core/src/credentials.rs:2` 无条件导入 `std::os::unix::fs::PermissionsExt`，Windows 原生编译报 E0433；第 34 行 `Permissions::from_mode` 报 E0599。实验模块无条件依赖稳定核心/后端，导致本来仅用于 Linux 的凭据代码也参与 Windows 编译。错误发生在实验协议代码编译前，与 QUIC/NAT/加密不属同一机制。

### 解决办法与恢复操作

稳定核心、后端及 FUSE 依赖仅在 Linux 启用；实验内新增可移植元数据和最小存储接口，WebDAV 客户端与协议测试通过统一 `model` 引用。Linux 仍重导出原接口，Windows 不引入稳定 OAuth/数据库实现。未改动稳定源码、用户安装或真实目录，无额外本地恢复。

### 验证结果与边界

旧 Windows CI 明确失败，未运行协议测试或构建产物。修正后本机 Linux 新增元数据兼容性测试与 9 项协议集成测试全部通过，fmt/Clippy 警告视错误通过；`cargo tree --target x86_64-pc-windows-msvc --locked` 核对无稳定核心/后端/FUSE/SQLite 依赖。

[a15be05 原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37471312984) 已全部成功：Linux 1 项元数据兼容性测试 + 9 项协议测试，Windows 8 项协议测试；两平台 fmt、Clippy 警告视错误、release 构建、双 CLI 进程 GET/PUT/MOVE/DELETE 与 2,097,105 字节比对、产物上传通过。Windows 使用静态 CRT。原失败分支不能编译，修正后同一原生流程成功，验证了依赖隔离机制。

**边界**：Windows 未执行 Unix 权限、软链接与 FIFO 的专用测试，也未验证用户特定 ACL、Explorer WebDAV 客户端或 Windows FUSE 挂载。CI 冒烟各自在单个 runner 内启动两进程，不能据此声称两台异地 NAT 验证。Linux/公共中继与稳定基线范围见 TD-20261006-03。

### 防复发措施与后续

保留原生 Windows 编译、协议测试、release 构建与双 CLI 进程 GET/PUT/MOVE/DELETE 冒烟；保留 `portable_metadata_matches_the_linux_contract`，不通过修改稳定代码绕过隔离边界。保留平台矩阵，源码改动后实际核对 CI 结果。

### 交付记录

源码 [0222663](https://github.com/LumiaBlack51/twodrive/commit/022266391500e1293f722bccb9d333479c7c3cbf)，草稿 [PR #8](https://github.com/LumiaBlack51/twodrive/pull/8)；隔离修复 [a15be05](https://github.com/LumiaBlack51/twodrive/commit/a15be05fe8c038f4ac4556a72a2b0a649c5ec38f)；[成功 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/37471312984)，资产 [Linux](https://github.com/LumiaBlack51/twodrive/actions/runs/37471312984/artifacts/11416947772) / [Windows](https://github.com/LumiaBlack51/twodrive/actions/runs/37471312984/artifacts/11417581314)。资产对应已验证源码，不作为稳定 Release。

## TD-20261006-03：dev 条件覆盖与根目录文件 PUT 被错误拒绝

- 日期：2026-10-06
- 状态：已验证（本机 WebDAV、QUIC 和隔离 FUSE）；真实异地 NAT 与外部厂商服务未验证。
- 影响版本与环境：未提交 `dev` 实验 `twodrive-dev 0.1.0`，Linux 临时目录；稳定 TwoDrive 不受影响。
- 关联历史故障：TD-20261006-02 首轮问题修正后暴露；新的 ETag/根目录同步问题属于新增适配器的不同机制。

### 症状与影响

有效 ETag 覆盖返回 HTTP 412；位于共享根目录的文件 PUT 返回 HTTP 404。后者在原子替换后失败，可能已提交内容；不能将错误响应等同于远端未写入。测试未操作用户文件。

### 触发条件与复现

`webdav_contract_unicode_versions_moves_and_snapshot_deletions` 在条件覆盖时失败；`streaming_download_cancellation_and_full_file_upload` 创建 `/large.bin` 时失败。嵌套路径的字节数组 PUT 与本机 QUIC 读写已经通过，当时 8 项测试为 6 通过、2 失败。

### 根因与证据

- dav-server 0.11 的 PROPFIND `getetag` 直接输出元数据裸值，HTTP 条件检查使用带引号 ETag；客户端直接复用 XML 值造成不匹配。保留已带引号/弱标记形式，只给裸值补 HTTP 引号，并要求覆盖使用单个带引号的强 ETag，防止弱/星号/无效条件被服务忽略。
- `Path::parent()` 对根目录相对文件名返回空路径。提交后调用 capability `open_with("")` 返回 ENOENT，映射成 HTTP 404；不是 HTTP File body framing 或原文件缺失。改为对空父路径使用 `.`。

### 解决办法与恢复操作

修正 ETag 语法与父目录句柄选择。未修改稳定引擎、真实配置或安装服务；仅使用测试目录，无额外本地恢复。

### 验证结果与边界

- 两项测试在修正前分别失败（412/404），修正后通过。5 MiB 文件流上传/下载字节一致，下载取消有实际回调；旧 ETag/未带版本的覆盖被拒绝，正确版本可覆盖，后续 MOVE/DELETE 与快照删除检测通过。
- `invalid_conditional_tokens_fail_before_network_io` 覆盖弱标记、星号、裸值、多个 token 和换行；拒绝发生在网络操作前。
- 最终 9 项实验集成测试全部通过，fmt、Clippy 警告视错误通过。两独立 CLI 进程的 LAN、公网基础设施默认模式和强制公共中继模式均通过 GET/PUT/MOVE/DELETE，2,097,105 字节比对一致；强制中继输出已核实为 relay。
- 隔离真实 FUSE + QUIC 测试通过读取、持久保存/后台上传、目录移动和删除；另验证默认只读挂载返回 EROFS、服务器拒绝 PUT。
- 稳定工作区 101 项 Rust 默认测试、19 项 Nautilus 和 4 项 Settings 测试通过；原有 6 项环境相关 FUSE 测试仍默认忽略，本次新实验挂载冒烟与它们是不同验证。
- **边界**：两端进程均在本机，公共中继是实际外部基础设施，但未证明两台异地 NAT 机器打洞；未测试全部 WebDAV 厂商、真实 OneDrive 端到端、完整桌面 GUI、断点上传或双向目录镜像。Windows 原生编译、协议与产物结果另见 TD-20261006-04，未将 CI 配置当作验证成功。

### 防复发措施与后续

保留真实协议与磁盘内容测试、条件写语法检查、独立缓存绑定和完整快照失败时不生成删除结果的约束。路径 ID 随移动变化，WebDAV 刷新语义及外部服务的原子性边界见 [设计](dev-webdav-peer-design.md) 与 [实验指南](../experiments/twodrive-dev/README.zh-CN.md)。

### 交付记录

实现与修复已提交 [0222663](https://github.com/LumiaBlack51/twodrive/commit/022266391500e1293f722bccb9d333479c7c3cbf)，草稿 [PR #8](https://github.com/LumiaBlack51/twodrive/pull/8)。Windows 原生结果见 TD-20261006-04；不合并或发布到稳定 main。

## TD-20261006-02：dev WebDAV 适配器首轮端到端读写失败

- 日期：2026-10-06
- 状态：已验证（Linux 临时目录）；后续独立机制见 TD-20261006-03。
- 影响版本与环境：`dev` 分支新增 `twodrive-dev 0.1.0` 实验；未影响稳定程序。
- 关联历史故障：无相同实现；[provider 边界记录](refactor-2026-09-16.md) 涉及共享接口，失败在新增适配器。

### 症状与影响

首轮 PUT 返回 HTTP 500，上传上限较小时单资源 PROPFIND 返回 HTTP 502，4 项读写测试失败。已确认 PUT 错误可发生在原子替换之后；原有数据均为临时测试文件。

### 触发条件与复现

初始 8 项集成测试：4 通过、4 失败。普通、Unicode、流式 PUT 和真实本机 QUIC 转发均失败；设置 64 字节上传上限后，174 字节 PROPFIND XML 也被拒绝。

### 根因与证据

- cap-std 的 Linux 目录能力可能使用 O_PATH。复制能力句柄后直接 fsync 返回 EBADF（脱敏诊断为 errno 9），此时文件已同步、已 rename；HTTP 500 并不说明写入未发生。
- 把上传上限应用到全部方法的请求 body，误拒绝 PROPFIND XML；该错误被 DAV 库转为 HTTP 502。
- 本轮源码检查另发现普通文件检查原位于 open 之后，根目录中的 FIFO 可阻塞读取。直接命名管道回归覆盖该路径，增加 open 前类型检查及 Unix O_NONBLOCK，之后再检查句柄类型，防止本机替换竞态使网络读取阻塞。

### 解决办法与恢复操作

以目录能力重新打开可读父目录句柄并同步，保持访问范围约束；文件体上限只用于 PUT，XML 采用独立 64 KiB 上限。Content-Length 和实际流长度超限返回 413，中断/超限写入删除临时文件，保留原内容；命名管道与设备不作为普通文件打开。未操作真实账户、用户目录、系统服务或安装包。

### 验证结果与边界

- 修正前 PUT/小上限元数据测试失败（500/502），修正后通过。`interrupted_and_oversized_put_preserve_original_content` 核对原内容与临时文件清理；元数据读取不受 64 字节上传上限影响。
- `capability_scope_rejects_symlink_escapes_and_special_files` 覆盖外部目录/文件软链接、设备软链接、命名管道、`..` 和保留临时文件名。临时移除普通文件检查和非阻塞打开后，旧机制的单项回归在编译完成并进入测试后超过 12 秒，被隔离测试超时终止（退出 124）；恢复保护后该项通过。
- 权限、过期/单次/并发邀请、真实 QUIC 未授权拒绝和现有连接撤销测试通过。验证为临时磁盘与本机设备，未覆盖所有文件系统的断电恢复、磁盘故障或 Windows ACL 部署。

### 防复发措施与后续

保留文件提交后响应、上传错误不破坏原内容、独立 XML 限制与 capability 范围回归；无法声称所有 500/502 错误永久消失。其他新机制和完整测试范围见 TD-20261006-03。

### 交付记录

实现与修复已提交 [0222663](https://github.com/LumiaBlack51/twodrive/commit/022266391500e1293f722bccb9d333479c7c3cbf)，草稿 [PR #8](https://github.com/LumiaBlack51/twodrive/pull/8)；[dev 设计](dev-webdav-peer-design.md)。

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

## TD-20260916-08：peer 升版暴露更新测试固定候选版本

- 日期：2026-09-16
- 状态：已验证本地及 Windows 原生完整测试和更新进程夹具。
- 影响版本与环境：0.1.1 构建期间的更新单测/健康夹具；不是生产反回滚逻辑故障。
- 关联历史故障：TD-20260916-07 升版验证中发现；与 Graph 故障机制不同。

### 症状与影响

完整 Rust 测试有两项失败：update must be a newer stable version。

### 触发条件与复现

把 peer 版本从 0.1.0 升至 0.1.1 后执行 cargo test --workspace --locked。

### 根因与证据

测试固定以 0.1.1 作为新版本，已不高于当前版本；生产反回滚检查正确拒绝。原生健康夹具同样写死版本。

### 解决办法与恢复操作

测试候选及原生夹具按编译版本 patch + 1 构造，第二次升级为 patch + 2。生产更新检查不改。无本地恢复操作。

### 验证结果与边界

旧测试在 0.1.1 实际失败；修复后完整 Rust 测试通过；本地及 Windows 原生夹具验证通过。此项不属于真实 GitHub 自动升级验证。

### 防复发措施与后续

候选版本应相对编译版本生成；保留不递增版本拒绝及回滚水位断言。

### 交付记录

随本次控制通道修复提交 c2776f1；见 TD-20260916-07 的交付链接。

## TD-20260916-07：真实 Graph permanentDelete 拒绝导致 peer 轮询失败

- 日期：2026-09-16
- 状态：已验证 Linux 真实 Graph 控制对象读写删除修复；Windows/Linux 双机握手待重新验证。
- 影响版本与环境：codex/peer-control cc472ea，peer 0.1.0，用户 Windows/Linux 同账号真实联调；修复版本 0.1.1。
- 关联历史故障：TD-20260916-06 为分页编码问题。本次已观测失败在删除阶段，为另一根因；之前 mock 和原生 CI 没有覆盖真实 Graph 删除能力。

### 症状与影响

用户 Windows 登录成功后持续 poll failed，Linux ping 仅入本地队列，未完成真实握手。Linux 同账号真实探针复现删除失败；原程序隐藏错误阶段。

### 触发条件与复现

使用既有隔离 peer 登录状态访问 AppFolder。真实 approot、namespace、devices/list 均 200；新建 diagnostics bucket 201，探针 PUT 201，GET 200 且内容一致；旧空 POST permanentDelete 返回 411。显式 Content-Length: 0 后返回 400 invalidRequest，脱敏消息为固定文本 API not found。轮询实际收件箱也复现 400。没有读取或删除用户普通文件。

### 根因与证据

1. reqwest 空 POST 不自动发送 Content-Length；真实 Graph 前端拒绝为 411。新增回环 HTTP 测试在旧构造上失败（header 为 None），修复后为 0，通过。
2. 本账号使用文档 drive-ID 路由仍返回 API not found。不能把微软文档列出接口当成当前账号支持证据；具体服务端原因/Windows 原始失败阶段不能仅由 Linux 结果推断。
3. 原 runtime 把读取失败当成无效信封并继续删除，网络故障时可能丢消息。新增回归在旧逻辑上失败（delete 被调用），修复后保留对象重试。

### 解决办法与恢复操作

- 控制通道空 POST 显式携带 Content-Length: 0。仅 permanentDelete 的 400 + invalidRequest + 精确 API not found 回退到普通 DELETE；本进程记忆不可用能力并提示进入回收站。401/403、其它 400、限流及 5xx 不回退，不清空回收站。
- 控制 HTTP 错误使用类型化诊断，操作名、数字状态、固定允许列表错误码和固定 hint；不输出 URL、ID、响应正文、token 或任意服务端文本。可开启逐操作成功日志。doctor 报配置 AppFolder/过期状态/refresh 是否存在（不是已授权 scope 的证明），只校验自建唯一探针。
- 发现阶段记录读取错误；收件箱读取失败保留消息，下次重试，不计为协议拒绝。

本地恢复：暂停下载目录中的 Linux peer PID 344856/344857；稳定 daemon PID 204605 及挂载未改动。使用原隔离 peer auth 做真实探针，未重新登录或更换用户设备身份。最初两次失败探针遗留的两项对象已校验测试内容后普通删除；未清空回收站。

### 验证结果与边界

- 真实账号 doctor 修复后：PUT 201、GET 200 内容一致、permanentDelete 400 触发明确能力回退、DELETE 204、随后 LIST 200 且本次对象不存在。
- 空 POST 和读取失败保留消息两项已证明旧代码失败、修复后通过。固定错误码脱敏及严格回退条件单测通过；本地 114 项默认 Rust 测试、19 项 Python 测试通过。格式与 Clippy 通过；Windows 原生 72 项默认测试及额外更新进程测试通过，Linux CI 114 项默认及额外更新进程测试通过。
- Python 初次读取重定向内容曾发生网络 timeout；Rust doctor 读回成功。不据此推断 Windows 网络状况。
- 本机两个临时隔离身份通过真实 Graph 完成互相 Authenticated、双向 PING/PONG round trip verified；临时云端 presence/收件箱及本地测试身份/token 已清理。它们运行于同一 Linux 主机，不是 Windows/Linux 双机实测。
- 下载目录 0.1.1 发布版再次运行 doctor 全部通过，其中 permanentDelete 直接返回 204。**更正边界**：400 API not found 是当时的真实响应，不能推断该账号永久不支持此 API；程序仍优先永久删除，仅对精确能力响应作本进程回退。
- **边界**：以上是 Linux 对真实 Microsoft Graph，不是 mock；仍不等于 Windows/Linux 真机握手与 ping/pong。Windows 需替换新 exe 运行 doctor、互信后验证。普通删除进入回收站，不保证永久清除。

### 防复发措施与后续

保留 empty_permanent_delete_post_has_explicit_zero_content_length、only_explicit_api_not_found_allows_recycle_fallback、diagnostic_never_echoes_untrusted_error_fields、failed_mailbox_read_is_not_deleted_or_counted_as_rejected。新云端功能须增加真实账号探针，不能仅以 mock/CI 宣称支持。说明和重测命令见 [双机指南](peer-quickstart.zh-CN.md)。

### 交付记录

修复提交：[c2776f1](https://github.com/LumiaBlack51/twodrive/commit/c2776f10148ee6b90dd65b13023281e04537ece6)。[原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/35074280918) Windows/Linux 均成功；Windows release exe 原生版本、健康、自身份持久化及系统 DLL 依赖检查通过。Linux 发布版 SHA-256：`8f03fba31b94a3a59f7f58e7e3466b4a07d88f6e91be49775319b21057786d2e`，已替换本地下载目录 peer，并保留 0.1.0 备份。未发布 GitHub Release；稳定 TwoDrive 不发布、不安装、不重启。


- Windows exe：7662080 bytes，SHA-256 `84b33e056bc417c574b6016ec3d72376f264e38680ce9f36c8170fe5b14187b0`；与下载 CI SHA256SUMS 一致，PE AMD64 已核对。CI artifact ID 10436929068。
- 本地手动替换包：`dist/peer-0.1.1/twodrive-peer-0.1.1-windows-x86_64.zip`，亦已复制到本地下载目录。ZIP SHA-256 `c50ec246e014018964182a75546fbe9bf42b8c06363554596fa4bc6829b3ec30`。包内 exe 是上述未修改的 CI 原件；没有自动更新签名 manifest、令牌或私钥。
- 原用户 Linux 身份短时运行 40 秒：poll/read 错误均 0，6 次 permanentDelete 204；未观察到原 Windows 对端认证。已停止测试 peer 进程，等待两端替换/重启；稳定 daemon PID 204605、启动时间 14:18:36 CST 不变。
- 后续记录提交仅补充验证/交付，不改变已构建可执行文件输入。尚未执行 Windows 用户设备真实 doctor/双机 ping；不得把原生 CI 或本机双进程当作这项完成。

## TD-20260916-06：控制通道分页校验拒绝等价的 OneDrive ID 编码

- 日期：2026-09-16
- 状态：已验证单元机制：旧代码失败、新代码通过，Linux/Windows 最终 CI 通过；真实 Graph 分页未验证。
- 影响版本与环境：peer 控制 provider，至 fb05bcb；既有文件系统分页未改动。
- 关联历史故障：TD-20260916-02 的不可信 URL 约束新增时边界遗漏，不是同一信任根因。

### 症状与影响

构造合法等价 nextLink 时，包含 `!` 的 ID 与 `%21` 编码的预期路径不相等，严格字符串校验会拒绝后续页。实际账号返回形式未核实。

### 触发条件与复现

预期路径包含 `ABC%21123`，nextLink 同一位置为 `ABC!123`。新增 pagination_accepts_equivalent_onedrive_id_encoding 在修复前失败。

### 根因与证据

校验要求正确的 HTTPS Graph 来源和相同资源，但直接比较转义后的路径字符串，混淆编码差异与跨资源访问。

### 解决办法与恢复操作

已逐路径段解码后比较，同时保留段边界、HTTPS、主机、端口、凭据、fragment 限制；不放宽到任意 Graph URL。无需用户数据恢复。

### 验证结果与边界

旧代码测试失败；修复后等价编码、错误来源及资源逃逸测试通过，Clippy 和格式检查通过。模拟 URL，不是实际 Graph 分页抓包。

### 防复发措施与后续

保留等价编码和不同资源拒绝用例，不能只验证恶意 nextLink。

### 交付记录

[修复提交 31ca30e](https://github.com/LumiaBlack51/twodrive/commit/31ca30e0433fbbf733930e14a88315058d472ec1)；[最终 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/35068850290)。

## TD-20260916-05：peer 篡改测试可能没有实际改变随机密文

- 日期：2026-09-16
- 状态：已修复测试反例构造；这是审计发现的测试缺陷，不是已确认的协议绕过。
- 影响版本与环境：peer 实验分支 0.1.0 测试代码，至 389bed8。
- 关联历史故障：审查 TD-20260916-04 的 CI 失败期间发现；其实际失败来自 daemon 时间窗，两者根因不同。

### 症状与影响

原篡改测试把密文首字节替换为 ff；随机首字节本来为 ff 时没有篡改，约 1/256 概率错误要求合法消息被拒绝。

### 触发条件与复现

静态反例：首字节为 ff 时 replace_range(..2, "ff") 保持输入不变。没有将此前任何 CI 失败归因于这一机制。

### 根因与证据

替换为固定值不能保证改变随机输入。协议正常接收未改动的有效消息并非安全漏洞。

### 解决办法与恢复操作

把首个十六进制字符改成与原值不同的字符，并断言新旧密文确实不相等。没有生产代码或本地恢复操作。

### 验证结果与边界

本地篡改、会话、目标和过期测试重跑通过，Clippy/格式检查通过；Windows 原生同项测试也已通过。没有真实云端攻击测试。

### 防复发措施与后续

保留 assert_ne!，篡改类测试必须先确保构造的输入不同于有效原件。

### 交付记录

[修复提交 fb05bcb](https://github.com/LumiaBlack51/twodrive/commit/fb05bcb943b086fc2413ab049fc5cbd17c2e94c3)；[最终 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/35068850290)。

## TD-20260916-04：CI 中既有 known-folder 持久化测试的短观察窗失败

- 日期：2026-09-16
- 状态：已验证测试调度依赖修复：本地与最终 Linux CI 通过；生产回归尚无证据。
- 影响版本与环境：重构前继承的 daemon 测试；peer CI 35068052022 的 Linux runner。
- 关联历史故障：[响应与恢复](responsiveness-recovery-2026-09-08.md)描述需要保留的逐任务持久化行为；[历史调查](background-download-investigation-2026-09-08.md)曾记录另一项并发测试波动，不能据此认定相同根因。

### 症状与影响

completed_known_folder_job_is_persisted_while_slower_job_runs 在 CI 断言失败，本地完整套件此前通过。失败发生在旧 daemon 测试，不是 Windows peer 编译或握手失败。

### 触发条件与复现

测试只轮询 25 次、每次 20ms，模拟快/慢上传分别休眠 100/800ms。注入额外 600ms worker 启动延迟后，旧测试稳定在 0.51 秒失败，尚未进入生产上传处理。

### 根因与证据

原 CI 日志只证明 500ms 内未观察到目标状态，不能分辨启动调度慢与中间状态被漏采样。生产上传实现本阶段没有修改；具体 CI 调度轨迹无法还原。应使用可控的慢任务完成屏障，而非依赖两次定时休眠之间恰好采样。

### 解决办法与恢复操作

仅改测试：慢上传等待释放信号，主线程观察快结果持久化后再允许慢任务结束，并始终释放/回收工作线程。生产 daemon 不变，无本地恢复操作。

### 验证结果与边界

同样的 600ms 模拟启动延迟下，旧测试失败；新屏障版本在 1.55 秒内通过，且断言仍要求慢任务未完成时快结果已经持久化。最终原生 CI 通过。属于测试确定性验证，不代表真实 OneDrive 上传性能。

### 防复发措施与后续

保留“慢任务仍未结束时快结果已持久化”的断言，避免简单放宽断言或只重跑掩盖问题。

### 交付记录

[CI 35068052022](https://github.com/LumiaBlack51/twodrive/actions/runs/35068052022)，[修复提交 fb05bcb](https://github.com/LumiaBlack51/twodrive/commit/fb05bcb943b086fc2413ab049fc5cbd17c2e94c3)；[最终 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/35068850290)。

## TD-20260916-03：peer 健康检查无法写入相对输出文件

- 日期：2026-09-16
- 状态：已验证：本地与 Windows 原生 CLI 回归及 release 自检通过。
- 影响版本与环境：实验 peer 0.1.0，截至 7f357bd；不影响现用 TwoDrive 实例。
- 关联历史故障：无；不是 TD-20260916-02 的协议信任问题。

### 症状与影响

原生 CI 在运行 release exe 的 `health-check --output health.json` 时退出失败；构建本身和绝对路径的原生更新子进程测试通过。阻断 artifact 交付检查，未造成已有文件丢失。

### 触发条件与复现

在空的隔离当前目录执行上述命令，输出使用不带父目录的相对文件名。本地新增 CLI 回归测试在旧函数上稳定失败，退出码 1。

### 根因与证据

Path::parent 对单文件名返回空路径；原子写入尝试在空路径创建临时文件，而不是当前目录。core 新增的私密文件写入辅助函数存在相同边界，虽然 peer 的实际 token 路径使用绝对父目录。

### 解决办法与恢复操作

已将空父目录规范为 `.`，保持同目录原子替换和 Unix 目录 fsync。没有用户数据恢复操作。

### 验证结果与边界

`release_health_command_writes_relative_output_without_state_side_effects` 在修复前失败，修复后通过；格式、Clippy 和 diff 检查通过。Windows 原生同一 CLI 回归及 release 自检通过。测试不访问 OneDrive，不代表真实登录通过。

### 防复发措施与后续

保留真实 CLI 子进程测试，验证生成预期健康结果且不创建 peer 运行状态。原生 CI 在上传产物前执行 release 健康检查。

### 交付记录

触发 CI：[35067546462](https://github.com/LumiaBlack51/twodrive/actions/runs/35067546462)。[修复提交 69fb3d9](https://github.com/LumiaBlack51/twodrive/commit/69fb3d94c797867f8d702daf7801d6f73e04000f)；[最终 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/35068850290)。artifact 摘要见 [交付记录](peer-implementation.md#verified-delivery)。

## TD-20260916-02：relay-lab 原型的云端信任与控制重放边界不足

- 日期：2026-09-16
- 状态：已验证新 peer 的本地及原生 CI 安全机制；真实 OneDrive 端到端未验证。
- 影响版本与环境：用户提供的独立 twodrive-relay-lab 0.1.0 原型；不据此认定稳定 TwoDrive 存在相同攻击面。
- 关联历史故障：无；与 FUSE 权限和本地创建记录故障无关。

### 症状与影响

原型把同一 OneDrive 中自签名 manifest 列表作为 trusted_senders；可写云端的主体能添加自有密钥，再发送可被接受的加密消息。加密不等于验证物理设备归属。原型没有设备控制握手与会话重放状态，不能直接当作控制协议使用。

### 触发条件与复现

审计 identity.rs 的 manifest 自签名验证、graph.rs::list_manifests 和 crypto.rs::verify_payload 调用链，确认信任列表完全来源于云端。新协议用未知自签名设备、重复信封、过期信封、错误会话/目标作为合成回归输入。旧原型复制到隔离临时目录后，新增两项安全要求测试；均可重复失败（云端注入身份被接收、同一信封两次解密均被接收）。原始工作区原型未修改，也未在真实云端进行攻击测试。

### 根因与证据

云端发现与本地授权未区分；消息签名未覆盖目标、信封 ID 和有效期等外层路由字段。详细分析见 [peer audit](peer-implementation.md)。Windows 兼容性检查还发现已有 core credential 和 provider upload persistence 导入 Unix 专用 API，原样依赖无法编译 Windows peer；这属于此前 Linux-only 代码的移植边界。

### 解决办法与恢复操作

复用 provider/OAuth，不导入原型的重复认证实现。新协议增加本地指纹信任、完整上下文签名、随机会话、挑战握手、重放窗和输入上限。Windows 使用 DPAPI，Unix 保持 token 格式兼容并原子写入。更新发布密钥独立于设备身份和云端。

本地恢复操作：无。仅在独立 worktree 和测试临时目录构建运行，未操作稳定服务和已有用户数据。

### 验证结果与边界

初轮 peer 7 项、core 31 项 Linux 测试通过，包括未知设备拒绝、双向握手与 ping/pong、重放/过期/会话拒绝，以及正常更新、错误签名、损坏文件、健康检查失败回退。最终 110 项默认 Rust 测试、19 项 Python 测试、6 项实际隔离 FUSE 测试、独立 daemon smoke 通过。Windows 原生 CI 的 68 项默认测试及额外原生更新进程测试通过。测试为模拟，非真实 OneDrive 端到端。已在原型副本运行 audited_cloud_manifest_is_not_local_authorization 和 audited_control_replay_must_be_rejected，两项均因预期的安全边界缺失而失败；新 peer 的对应未知身份和重放拒绝测试通过。两套实现协议不同，这不是将同一测试直接移植到旧生产版本。

### 防复发措施与后续

协议层测试固定 cloud discovery != local trust；后续传输仅提供不可信字节。保持独立发布根、反回滚检查和不覆盖当前 exe 的更新事务。更多故障及最终验证将在交付前补记。

### 交付记录

实验分支 codex/peer-control；[构建提交 31ca30e](https://github.com/LumiaBlack51/twodrive/commit/31ca30e0433fbbf733930e14a88315058d472ec1)；[最终 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/35068850290)。产物摘要见 [交付记录](peer-implementation.md#verified-delivery)。未发布或替换稳定 TwoDrive。

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
