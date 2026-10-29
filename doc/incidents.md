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





## TD-20261029-02：超限补丁回归把流式早拒绝固定断言为 HTTP 413

- 日期：2026-10-29（Asia/Shanghai）
- 状态：已验证测试边界修正及内容保护；未声称修复 HTTP-over-QUIC 的所有早拒绝响应差异
- 影响版本与环境：本轮未提交 delta_upload.rs，本机 QUIC/HTTP 转发；未发布或安装。
- 关联历史故障：TD-20261006-02 有上传限额与 HTTP framing 调查，本次尚不认定相同根因。

### 症状与影响

相同 6 MiB 超限补丁前次收到 413，本轮收到网关 502；回归要求固定 413 因而失败。需区分拒绝写入与错误响应传递；后续逐次比对确认原文件未更改。

### 触发条件与复现

服务器限额 4 MiB、补丁输入额外允许 1 MiB，客户端仍在发送 6 MiB body 时服务端提前返回错误并关闭 HTTP-over-QUIC 流；重跑新增组合回归出现不同 HTTP 状态。

### 根因与证据

重复 8 次合成超限流式发送观测到 413 和 502；502 对应错误链的 stopped by peer（仅记录布尔分类，不保存正文）。锁定 noq 的 RecvStream::drop 在接收尚未结束时调用 stop(0)，Hyper 写入错误可先于读取早拒绝响应结束请求；与实际错误链和状态差异相符。回归误把应用的拒绝判定等同于活跃 QUIC 发送端必然取得的 HTTP 状态。与旧 XML/PUT 限额串用机制不同。

### 解决办法与恢复操作

拆分验证：只发送超限 Content-Length、没有活动 body 写者时，必须通过真实网关收到 413；另在仍发送超限 body 时只接受明确的 413 或因 STOP_SENDING 导致的 502，逐次独立核对原文件和后续临时文件清理。没有修改限额、放宽写入权限或自动重放变更；一次性诊断已删除。没有用户数据恢复操作。

### 验证结果与边界

原固定状态断言实际失败（502 != 413）。拆分后的真实本机 QUIC 回归通过：头部超限仍为 413；8 次活动 body 发送中拒绝结果含 413/502，所有原内容保持不变，正常 PATCH 和后续锁/取消回归通过。另一个刻意截断 body 的用例产生 Hyper body-write-aborted，明确与超限 STOP_SENDING 区分。未执行抓包、所有 QUIC/HTTP 早拒绝组合或真实异地链路验证；Windows 原生组合回归也已通过（见 TD-20261029-01 的 CI 链接）。

### 防复发措施与后续

分别验证超限头部判定、活跃发送时拒绝、原内容与临时文件保护；不以收到某一状态作为唯一未写入证明。后续同一用例又观测到回环 HTTP 写端 BrokenPipe（OS 32），在取得响应头前结束；属于同一早拒绝跨两跳传播边界。仅对该刻意超限用例接受 BrokenPipe/ConnectionReset/ConnectionAborted 错误链，连接失败/超时不算通过，正常补丁仍必须成功。

### 交付记录

初始实现：[fff7c59](https://github.com/LumiaBlack51/twodrive/commit/fff7c59)；验证修正：[4fb5b25](https://github.com/LumiaBlack51/twodrive/commit/4fb5b25)，随 [PR #48](https://github.com/LumiaBlack51/twodrive/pull/48) 交付。该运行时代码的原生 Linux/Windows 结果见下一条；没有发布 Release。


## TD-20261029-01：增量扩展把 Unicode URL 当作未解码磁盘路径

- 日期：2026-10-29（Asia/Shanghai）
- 状态：已验证 Linux/Windows 原生分块、真实本机 QUIC 与合成文档；异地 NAT/真实编辑器保存未验证
- 影响版本与环境：本轮未提交的 twodrive-dev 0.1.7 增量扩展；Linux 临时目录和本机真实 QUIC。未发布或安装。
- 关联历史故障：TD-20261006-03 是裸 ETag 引号适配，本次是另一种根因，保留旧结论。

### 症状与影响

新增 conditional_delta_full_fallback_corruption_locks_and_interruption_over_quic 回归首次使用 PROPFIND 取得的当前版本上传时，TD-SIGNATURE 返回 412，未进入 PATCH，也未修改原文件。

### 触发条件与复现

在临时共享根创建合成二进制文件，通过真实本机 QUIC 的 WebDAV 网关查询 ETag，然后进行 Auto 条件上传；首轮稳定拒绝。

### 根因与证据

dav-server 0.11 的 DavPath::from_uri 只复制 URI 字节，不执行 URL 解码；扩展误以为其与普通 DAV 处理路径一致。合成中文路径被当作含 %XX 的磁盘文件名，delta_base 返回 NotFound，映射为 412。普通 DAV 使用规范化/解码路径构造；不是文件 ETag 本身过期。已查阅本机锁定依赖源码确认这一差异。

### 解决办法与恢复操作

扩展改用 DavPath::new(request.uri().path())，与 DAV 的 URL 路径规范化保持一致；继续使用相同 capability 与普通文件检查。没有用户数据恢复操作。

### 验证结果与边界

Linux 根工作区默认测试、严格 Clippy/格式、独立工作区 84 项默认测试与严格 Clippy/格式通过；7 项根 FUSE 和 1 项 peer 更新进程测试仍默认忽略。另显式通过独立 dev 的真实隔离 FUSE 冒烟，包含已有大文件修改、持久上传、新目录与移动/删除。

同一中文路径 QUIC 回归在修复前失败（TD-SIGNATURE 412），修复后通过。分块编码/校验回归、真实本机加密 QUIC 的增量/强制全量/冲突/损坏/超限/中断/锁/取消检查通过，旧 DAV 的条件全量回退通过。两进程 smoke 的合成 PDF/DOCX/PPTX 均逐字节校验并验证强制全量；自动差分的补丁加签名约为原文件 2.4%/3.5%/3.6%，仅代表这些合成样例。同一运行时代码 4fb5b25 的 Linux 84 项、Windows 81 项原生默认测试、严格 Clippy/格式、release 构建、文档 delta/full 两进程冒烟和签名控制冒烟全部通过，见下方真实 CI。真实异地 NAT 和真实编辑器保存未验证。

### 防复发措施与后续

保留中文路径、当前/旧版本、完整回退、损坏补丁、中断与 DAV 锁的组合回归；测试与文档明确字节增量和元数据轮询的区别。详见[增量协议与验证边界](peer-delta-upload.md)。

### 交付记录

初始实现：[fff7c59](https://github.com/LumiaBlack51/twodrive/commit/fff7c59)，验证边界修正及本轮已验证的最终运行时代码：[4fb5b25](https://github.com/LumiaBlack51/twodrive/commit/4fb5b25ffeb0406b05fc1161b7f85bfb90e595a3)。[原生 Linux CI](https://github.com/LumiaBlack51/twodrive/actions/runs/40807054924/job/123228401817) 和[原生 Windows CI](https://github.com/LumiaBlack51/twodrive/actions/runs/40807054924/job/123228401486) 均成功，日志已核实测试数及 PDF/DOCX/PPTX 增量/全量逐字节结果。代码与记录由指向 dev 的 [PR #48](https://github.com/LumiaBlack51/twodrive/pull/48) 交付；本记录后续只补验证证据，不改运行时代码/锁文件。未创建 Release，也未安装或替换现用程序。


## TD-20261021-16: hosted tray fixture failed one foreground transfer

- Date: 2026-10-21; status: original failure retained; one unchanged-source failed-job rerun passed. Exact cause remains unconfirmed.
- Scope: source 0c746cc, hosted Windows desktop job. Related: TD-20261015-05 failed the earlier source-foreground precondition; the present failure occurred after that precondition passed, so the mechanisms are not assumed identical.

The [first Windows job](https://github.com/LumiaBlack51/twodrive/actions/runs/39626444352/job/119312819727)
passed compilation, IPC and Flutter checks, then test-desktop-windows-ui.py failed
at SetForegroundWindow(helper). The native trace showed the test panel visible
and focused; the helper transfer returned false. The log does not contain the
post-call foreground owner, so it cannot establish whether timing, session policy
or a fixture/product interaction caused the refusal. Unsigned MSIX checks were
skipped in this first attempt. No product UI code or this test changed since the
earlier 9be73fe baseline, and no local foreground reproduction was attempted.

Only the failed hosted job was rerun once on the **same source**, without relaxing
or skipping any focus assertion. The [retry](https://github.com/LumiaBlack51/twodrive/actions/runs/39626444352/job/119320446479)
passed background-hidden startup, repeated tray toggles/stable position, external
focus dismissal, Escape dismissal and unsigned MSIX validation. All seven
workflows at 0c746cc then succeeded. This bounds the event as non-deterministic in
these two attempts; it is not a proved product fix or evidence of local Explorer
interaction. A recurrence needs actual foreground/owner/session diagnostics,
not suppression of the failure. No system focus policy, application code,
user process, real account, root registration, Release or merge was changed.

## TD-20261021-15: PNG validator omitted its rejected CFAPI result

- Date: 2026-10-21; status: safe validation split verified at 0c746cc on both CI platforms. The Server's native outside-root evidence remains unavailable/skipped; no generic failure is accepted as an unmanaged root.
- Scope: ca81956 Windows Server 26100 adapter. Related: TD-20261021-14's Rust recovery regression now passes; this failure is in the separate Python ordinary-data preflight, before PNG decoding.

The [Windows job](https://github.com/LumiaBlack51/twodrive/actions/runs/39618567696/job/119285630646)
passed strict Clippy and all 76 Rust test executions, including the corrected
ordinary-node regression. The exporter completed, then the Python validator
rejected CfGetSyncRootInfoByPath/BASIC because it did not return 0x80070186. The
assertion omitted the actual HRESULT; decoding and release packaging did not run.
The exact value and root cause are therefore unknown from the original log.

The first diagnostic correction prints the actual HRESULT, filesystem type,
root-exists and root-reparse booleans before refusal; it retains the same gate.
The [diagnostic Server job](https://github.com/LumiaBlack51/twodrive/actions/runs/39620680094/job/119292956248)
recorded `0x80070001`, `NTFS`, `root_exists=true`, `root_reparse=false`.
[Microsoft's API contract](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/nf-cfapi-cfgetsyncrootinfobypath)
states that an outside-root query fails, but does not promise one error for all
environments. Failure alone is not proof of an ordinary, unregistered location.

The validator still requires the specific NOT_UNDER_SYNC_ROOT result and now
independently refuses non-directory, symlink/reparse or unreadable ancestry before
any fixture content. Six regressions cover signed HRESULT normalization, success,
generic/permission/NOT_A_CLOUD_FILE failures, ordinary ancestry, reparse/symlink,
missing/file paths and permission denial. Unknown errors include their exact
HRESULT and stop decoding; an ordinary-file error cannot prove absence of a root.
Local regressions and full PNG decode pass.

The [44f4577 Server job](https://github.com/LumiaBlack51/twodrive/actions/runs/39622625423/job/119299776936)
disproved the stopped-driver explanation: CldFlt was already Running/Auto before
setup, so no driver-start call executed. A fresh ordinary directory on the system
volume still returned 0x80070001 on NTFS. The six Python checks, strict Clippy and
all Rust tests passed; content decoding remained blocked. The unused startup
branch has been removed. CldFlt implements the
[CFAPI minifilter](https://learn.microsoft.com/en-us/windows/win32/cfapi/build-a-cloud-file-sync-engine#cloud-files-api-architecture);
driver state alone does not establish an attached volume instance or root absence.

The next read-only diagnostic records Basic/Standard queries by path and an
attributes-only/no-recall/no-reparse-traversal directory handle, plus filter
instances on the disposable hosted runner. A seventh Windows test performs only
this ordinary-directory metadata preflight before the expensive build. Local
queries all return 0x80070186 and all seven checks pass. The Server result is
confirmed in [990a4ae's job](https://github.com/LumiaBlack51/twodrive/actions/runs/39624152224/job/119305046234):
all five query variants returned 0x80070001, and `fltmc instances -f CldFlt`
succeeded with **No instances for this filter**. The seventh check correctly
refused before any PNG/content read. Running driver state alone was insufficient.

A proposed hosted-only filter attachment was rejected by automatic approval
review before its commit/push command executed: a driver-level operation was not
explicitly authorized, including on a hosted runner. It was not retried or routed
through another tool. The proposal remains private review evidence and is absent
from the executable workflow. No attachment, root registration or driver/service
configuration occurred.

The safe correction keeps filesystem validation strict: a generic failure still
throws before any fixture content. Only the real Windows metadata *test* may be
marked skipped when both a successful read-only filter enumeration explicitly
reports no instances and the probe returns INVALID_FUNCTION. Other unexpected
results still fail. Skipped means outside-root evidence is unavailable; it never
means the directory is unregistered. The normal directory validator has no
fallback/skip flag and still requires 0x80070186 plus ordinary ancestry.

PNG format validation now independently consumes the owned deterministic Rust
generator's stdout. This mode creates/reads no filesystem fixture and never asks
CFAPI to interpret an unknown root. Windows CI can verify exact size, CRCs, zlib,
all pixels and digest while preserving the native-runtime limitation. Linux also
retains the complete ordinary-directory/layout validation; supported local
Windows directory validation and decoding already passed. The corrected
[adapter workflow](https://github.com/LumiaBlack51/twodrive/actions/runs/39626444403)
passed on Windows and Ubuntu at 0c746cc. Server Python reported six passes and
one explicit native-evidence skip; all 76 Rust executions, strict Clippy, generated
PNG decoding (SHA-256 de207d01b8cf2091ca00e4093aee8f4de45bd05e9974829593d41aba6385e7a6),
release build and probe packaging passed. This is not registered-root/visual
acceptance. No foreground action, data restoration, Release or merge occurs.

## TD-20261021-14: ordinary retained-node CFAPI errors vary by Windows build

- Date: 2026-10-21; status: old code failed at 0cd9dd9; corrected ordinary-node regression passed in Server 26100 CI at ca81956. Local Windows 26300 also passed.
- Related: TD-20261021-12 concerned WinRT long-path lookup; that nested-path regression passed in this failed job. TD-20261021-13 added the ordinary-node recovery guard and exposed this separate compatibility issue.

### Symptom, reproduction and cause

The [Windows adapter job](https://github.com/LumiaBlack51/twodrive/actions/runs/39616576509/job/119279156349)
passed strict Clippy and 13 library tests, but
`ordinary_converted_directory_and_files_require_manifest_ids_and_retain_bytes`
failed with `inspect placeholder: HRESULT 0x80070001`. Linux adapter checks
passed. The recovery guard expected ordinary CfGetPlaceholderInfo failure to be
0x80070178, as observed locally. The Server runner instead returned
INVALID_FUNCTION for the same ordinary fixture. No registration or data recovery
was performed by this test, and the existing native roots were not touched.

### Correction and verification boundary

An arbitrary CFAPI error no longer proves conversion. Under the existing
share-none lease, the code first checks the original persisted volume/file ID
and kind, then requires FileAttributeTagInfo to report no reparse tag, no
reparse/offline/recall attributes, and exactly CF_PLACEHOLDER_STATE_NO_STATES.
The [documented state](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/nf-cfapi-cfgetplaceholderstatefromattributetag)
identifies an ordinary node independently of the failing placeholder-info API.
Missing manifests, foreign IDs, Cloud/unknown state and incomplete content still
refuse recovery. The actual ordinary-file/directory regression is retained and
now also checks that the same ordinary nodes are refused without a manifest.
No allowlist of generic failure HRESULTs is introduced.

The failing CI evidence is retained. The corrected source passes all 76 local
test executions and strict all-feature/all-target Clippy. The same Server
regression passed at ca81956; its later Python preflight failure is tracked
separately as TD-20261021-15. New registered directory
teardown and all visual behavior remain unverified. There is no local data
restoration, permission/policy change, new Release or merge for this CI fix.

## TD-20261021-13: visual acceptance fixture lacked directory/image coverage

- Date: 2026-10-21; status: preparation implemented and ordinary-data regressions passed; registered directory, active-root observer and all native visuals remain unverified.
- Scope: PR #41 through c26b508 used three root-level binary files and a short native test lifetime. Related: TD-20261021-11/12; their successful API recheck does not prove image handlers, folder summaries or physical Shell refresh.

### Symptom, reproduction and evidence

The 9be73fe registered-root test ended in 3.70 seconds. It did not provide an
interactive visual interval or a valid image/nested directory. Native API success
therefore could not complete the requested Explorer acceptance. A first prepared
long-path PNG passed independent CRC/zlib/pixel checks and Windows stream decode,
but Windows GDI+ FromFile failed. Target capture without DPI awareness clipped a
150%-scaled window; a target Explorer observer also rejected a process main
window outside the expected fixture. Those captures are not accepted as glyph
evidence. No original account data or cache was affected.

### Cause and correction

The dataset and automatic test lifetime were insufficient for visual coverage.
Use the existing persistent GUI/FixtureService path with a separate VisualV1
dataset: three exact 64 KiB files, one directory, a valid short-path PNG and a
separate long binary. Retain old StandardV1 identities. Root-relative nested
WinRT lookup, filesystem-ID-bound retention/recovery and all-node leases cover
the new shape. A bounded phase log separates Provider download bytes, served
cache ranges and native requests/transfers. Capture uses target DPI awareness,
process/package/root binding and an optional exact Explorer window handle.
It never restores/focuses a window or sends input.

### Verification, recovery and remaining limits

76 local test executions passed, with registration cases ignored. Strict Clippy,
GUI-feature compilation, independent full PNG decode and Windows FromFile decode
passed. Ordinary-file tests cover nested long lookup, manifest replacement/extra
entry refusal, directory/file leases, phase budgets, timer completion and range
counter/offline semantics. No claim is made that an old native visual test was
reproduced or that the new directory CFAPI chain passed. Existing roots, journals,
files and backups remain unchanged; no native recovery was required in this
preparation phase.

The user now requires an isolated virtual screen while coding. No supported
independent native screen/input session is exposed by current tools. Foreground
interaction has stopped; permission for another root alone cannot resolve this
display limitation. No new registration was attempted. The
[bounded one-session plan and remaining matrix](windows-explorer-visual-acceptance-2026-10-21.md)
separate the future native/visual work from completed preparation. No release or
merge accompanies this change.

## TD-20261021-12: WinRT full-path metadata lookup differs across Windows builds

- Date: 2026-10-21; status: old code fails in Windows adapter CI; 9be73fe passes the same Windows 26100 regression and local Windows 26300 checks.
- Scope: PR #41 7072ba7, Windows Server 2025 build 26100 runner; local Windows 11 build 26300 passed the same ordinary-file test.
- Related: TD-20261021-11 includes a suspected long-path property failure but its original error was masked; these mechanisms are not assumed identical.

`metadata_lookup_accepts_chinese_file_beyond_max_path` passed locally and failed
the Windows adapter job with HRESULT 0x800700CE from
StorageFile.GetFileFromPathAsync. The generated ordinary file existed, and the
same CI job passed the truncation-lease test and strict Clippy. Failure evidence:
[Windows job](https://github.com/LumiaBlack51/twodrive/actions/runs/39596970297/job/119211569794).
No registry or long-path policy was changed to make the test pass.

Shell metadata now obtains the known root StorageFolder, verifies that the target
is an immediate generated leaf and calls GetFileAsync with its UTF-16 file name.
It does not fall back to enumerating unrelated folders, opening file contents or
claiming successful publication after error. The same >260-character Chinese
ordinary-file regression is retained. CI on build 26100 confirmed the fix;
the separately approved 9be73fe native recheck also passed property publication
with zero initial hydration callbacks. Glyph/menu/thumbnail visuals remain
unverified. There was no account/root/data recovery for this CI failure.

## TD-20261021-11: native acceptance failure retains a generated root

- Date: 2026-10-21; status: real failure and safe local recovery verified; original validation cause unresolved; revised generated-root operation/concurrency recheck passed at 9be73fe.
- Scope: unmerged PR #41 source 0cc1d4d, Windows 11 build 26300. Related: TD-20261021-07 and 10; neither synthetic owner tests nor untouched-file exit established full native acceptance.

The two specifically approved generated roots were each registered once. The
untouched-file test passed. The full native test failed; its final assertion
reported that the root remained registered after the retention gate refused
cleanup. The test asserted cleanup before reporting its original validation
error, so the original error is unavailable. No claim is made that pin, offline,
reconnect or all-file teardown passed. Long-path property publication is a
suspect only: ordinary-file WinRT lookup beyond 260 characters passes locally.

The failed root remained registered and its process had exited. Read-only CFAPI
inspection found three complete 64 KiB files; one had a full generated concurrent
teardown edit, while the private journal still described its previous generation.
An independent share-none read copied every actual byte with NO_RECALL, checked
all three identities and compared complete content. No old cache was substituted.

A test-feature-only recovery command verifies the unmanaged parent, original
ancestor locks, generated CFAPI context, current user's exact Shell folder binding
and three known leaf identities. It never registers or connects a root. Ordinary
share-none read/WRITE_DAC leases hold all complete files through another fsynced
backup, durable native-dirty recording and CfRevertPlaceholder. WRITE_DAC is a
documented authorization for conversion without data-write access. The exact
existing Shell registration was then removed; CFAPI independently returned
0x80070186 for both roots. All six original files and both modified-file backups
were byte-for-byte verified. The modified item is generation 1, acknowledged 0,
with native_dirty and recovery_required set. Files, private cache and journal
remain. No Explorer restart, real account access or other-root operation occurred.

The normal teardown now uses the same ordinary leases and documented Win32
handle conversion. A real ordinary-file regression verifies that truncation is
refused until lease release. The ignored native test now logs original and
cleanup errors before assertions and enables its concurrent writer only after
the main validation succeeds. Per-item Shell publication failures are logged.
The source and full local regressions compile; this does not establish that the
revised normal exit or full operation chain passed native acceptance at that point.
After explicit approval of one additional finite root scope, the 9be73fe exact
harness passed the full generated-provider operation and concurrency test in
3.70 seconds. All-file leases refused the writer until conversion; complete
concurrent bytes survived unregister. Pin, handle-close retry, offline pin
recovery, callback reconnect and durable dirty veto passed. An independent query
confirmed all three roots unregistered (0x80070186), and all nine 64 KiB files
were exact verified. Each root was registered once. This does not resolve the
masked original error or establish native glyph/menu/thumbnail visuals or GUI
interaction with a registered root. Existing roots will not be registered again.

Sanitized evidence: [native result record](windows-explorer-native-results-2026-10-21.json).
Raw local logs, hashes, journals and generated data are retained outside Git.
Recovery is limited to generated fixture identities; persistent production-root
restart and real cloud writes remain disabled. No release was published here.

## TD-20261021-10: metadata-only teardown check leaves a concurrent-write window

- Date: 2026-10-21; status: first correction failed real native acceptance; see TD-20261021-11 for recovery and revised gate.
- Scope: unmerged PR #41 source f321b48. Related: TD-20261021-07 covers untouched online-only files, not concurrency.

The old teardown released each short-lived metadata handle before unregistering.
A concurrent writer could change the file after validation. The untouched-file
test cannot establish safety across that window; no destructive old-root test was
run to create a data-loss demonstration.

Teardown now rejects incomplete files with attributes-only checks, acquires and
retains exclusive protected CFAPI references to every known generated file,
rechecks identity/residency/length under those references, and reverts all complete
placeholders to ordinary files before unregister. Later ordinary-file writes are
outside CfUnregisterSyncRoot's placeholder deletion behavior. Conversion progress
is retained in the live Session; partial teardown closes native capabilities and
reports cleanup/registration as unconfirmed. No reference is held over a Provider
download. Existing dirty bytes and private journal/cache remain preserved.

The initial correction used an opt-in `native-test-hooks` observation barrier that compiles in adapter CI but is
absent from ordinary GUI release builds. The ignored real-engine test starts a
concurrent generated writer while all references are held, waits for its ordinary
file write after conversion, then checks complete bytes after unregister. It has
subsequently run once on an explicitly approved new root and failed, retaining the
root. TD-20261021-11 records the preservation, successful unregister recovery and
remaining uncertainty. No physical/concurrent CFAPI pass is claimed.

## TD-20261021-09: refused stop loses the owner while capabilities remain active

- Date: 2026-10-21; status: old loop reproduced failing; corrected owner-loop regression passes locally.
- Scope: unmerged PR #41 f321b48; related: TD-20261021-07 retention refusal.

The worker exited on every stop command, even when retention refused cleanup;
the public stopped flag changed only on success. After extracting that unchanged
loop for a generated-file owner harness, `rejected_stop_keeps_owner_for_status_read_and_reconnect`
failed with `fixture owner disconnected`. The same regression passes after the
worker exits only on successful stop. Status, real generated-file reading and
reconnect requests remain possible after refusal.

Worker liveness is explicit; a terminal incomplete teardown disables capabilities
and marks owner unavailable. Drop closes its channel before joining, avoiding a
hang when stop remains refused. These are owner/control regressions, not a real
CFAPI stop-refusal run. No local client or user data recovery was performed.

## TD-20261021-08: failed Shell intentions are lost and polling starves retry

- Date: 2026-10-21; status: three focused old-behavior failures reproduced, corrected regressions pass locally.
- Scope: unmerged PR #41 f321b48; related: no released native Shell intent implementation.

The old tuple was recorded before trying pin/free-space work, then errors were
discarded. The same tuple after an open-handle veto or offline read therefore
never retried. `free_space_intent_retries_after_real_handle_closes` failed using
an actual Windows sharing handle; `pin_intent_retries_actual_engine_after_offline_recovery`
failed against the actual generated Provider/cache bridge. The old receive-timeout
loop also failed `status_traffic_does_not_starve_pending_intent_tick`: frequent
read-only status requests prevented its timeout from firing.

The owner now keeps an explicit pending intent, classifies offline/guarded/dirty
recovery failures, and uses 1/2/4/8/16/30-second capped backoff (dirty recovery at
30 seconds). Only completion of the requested state or a changed/cancelled OS
intent clears it. The OS pin state persists; this in-process retry ledger does
not add persistent root restart support. Timer deadlines survive message traffic.
Each retry still enters the existing native oplock, dirty/pin/open-handle,
journal/generation and digest gates; no failed guard is bypassed.

All three focused regressions fail under their previous loop semantics and pass
after correction. IPC and GUI show local pending intent separately from cloud
acknowledgement. Shell pending/error output remains custom **text properties**,
not accepted native synchronization/error glyphs. Real Shell clicks, native
intent recovery and screenshots remain unverified; registration is still blocked.

## TD-20261021-07: generated-root shutdown can delete untouched placeholders

- Date: 2026-10-21.
- Status: mechanism identified and guarded; native new-root verification pending.
- Scope: opt-in CFAPI generated fixture in unmerged PR #39 / alpha.3; no real-account production native mount is enabled.
- Related: TD-20261021-05/06 protect cache/journal ownership; this is a distinct OS unregistration lifetime issue.

### Symptoms, trigger and evidence

The previous Session::finish unconditionally disconnected and unregistered a root.
Microsoft's CfUnregisterSyncRoot contract says unhydrated placeholders are permanently
deleted. An untouched online-only fixture can therefore disappear on normal exit.
Earlier native tests hydrated all three files before exit, so their complete-byte
checks did not cover this trigger. No real user-data loss is reported, and no risky
old-code unregistration was run to manufacture a reproduction.

### Mechanism, fix and recovery

The generated host now materializes its deterministic internal data before normal
shutdown. The adapter keeps its created-placeholder identities/sizes and refuses
Shell/CFAPI unregister while any is incomplete or has changed length. An incomplete
dirty file requires recovery; cleanup is explicitly unconfirmed and the root is kept.
This does not authorize adoption/cleanup of an existing cloud root. No user-data
recovery operation or installed client modification was performed.

### Validation and prevention

The separately ignored `untouched_placeholders_survive_normal_exit` requires its
own newly approved root. It checks that premature unregister is refused without
hydration, then normal stop preserves all full bytes and removes the registration.
Final native results and any remaining desktop blockers are recorded in
[Explorer acceptance](windows-explorer-2026-10-21.md). Physical icons, thumbnails,
live accounts, process/OS restart recovery and writable native operations must not
be inferred from unit or API results. Follow-up implementation is in
[draft PR #41](https://github.com/LumiaBlack51/twodrive/pull/41); f321b48 is its
initial source checkpoint. The concurrency gap in that checkpoint is separately
recorded in TD-20261021-10 above; it is not covered by the untouched-file test.

## TD-20261021-06: stale cache digest and rejected hydration metadata mutation

- Date: 2026-10-21.
- Status: reproduced on unmerged 33dc546; fixed and locally verified with generated files.
- Scope: opt-in private journal/engine bridge, absent from published alpha.2. No real account or installed client was changed.
- Related history: TD-20261021-05 covers ownership/lease persistence; these are additional consistency defects, not a claim of a released-client incident.

### Symptoms, reproduction and evidence

`reconciliation_must_rehash_inside_the_final_cache_lease` hashes an ambiguous-upload cache under an initial read lease, releases it, makes a same-length untracked edit, then reconciles. The old implementation accepted the stale digest and incorrectly acknowledged Cached. The regression failed at that assertion before the fix.

`rejected_rehydration_must_preserve_owned_cache_metadata_and_journal` hydrates v1, changes the generated Provider to v2, and attempts hydration again. The old call correctly refused replacing the cache but had already changed files.etag/size to v2. The metadata-preservation assertion failed before the fix. Private local logs retain both expected exit-101 failures; no user names or file lists are published.

### Mechanism and fix

The engine previously dropped its hashing lease before core reconciliation reacquired a lock. Core now accepts a hashing callback, computes the digest inside its final file lease and SQLite transaction, and retains that lease through acknowledgement. Hydration now checks identity/path cache ownership before upsert and holds the journal gate through metadata claim, serializing against publication. Existing bytes and journal generation remain unchanged on rejected rehydration.

Arbitrary-string bridge construction is test-only. A future real-provider constructor must use verified profile/provider session authority; no new arbitrary string API is treated as authenticated identity.

### Validation, recovery and limits

Both regressions fail on old source and pass after the changes. Strict Windows harness Clippy and 25 engine/journal tests pass. No data migration or repair of real files was performed. CFAPI integration, Explorer icon acceptance, live accounts and power-loss recovery remain outside this evidence. See [journal contract](windows-local-journal-2026-10-21.md). Changes are unmerged; the existing alpha.2 tag is unchanged.

## TD-20261021-06: stale digest and failed hydration metadata mutation

- Date: 2026-10-21.
- Status: reproduced on unmerged 33dc546; fixed and locally verified using generated files.
- Scope: opt-in journal/engine bridge, absent from released alpha.2. No real account or installed client changed.
- Related history: TD-20261021-05 covers ownership persistence; these are two additional consistency defects.

### Symptoms, reproduction and evidence

`reconciliation_must_rehash_inside_the_final_cache_lease` hashes an ambiguous-upload cache, releases its read lease, makes a same-length edit and reconciles. Old source accepted the stale digest and incorrectly acknowledged Cached. `rejected_rehydration_must_preserve_owned_cache_metadata_and_journal` hydrates v1, changes the generated Provider to v2 and hydrates again. Old source refused replacing the cache but had already changed files.etag/size to v2. Both deterministic regressions failed at their intended assertions before the fix (private logs preserve exit 101).

### Mechanism and fix

Core now computes the engine digest callback inside the final file lease and SQLite transaction, retaining that lease through acknowledgement. Hydration checks identity/path cache ownership before upsert and holds the journal gate through the metadata claim, serializing with publication. A rejected second hydration preserves existing metadata, bytes and journal generations. Arbitrary-string bridge construction is test-only; real production construction requires verified profile/provider authority and is not exposed yet.

### Validation, recovery and limits

Strict Windows harness Clippy and all 25 engine/journal tests pass after the fixes. Old-source failures and fixed-source passes are recorded. No real-data recovery or migration occurred. CFAPI integration, physical Explorer acceptance, live accounts and power-loss recovery are not established by these tests. The existing alpha.2 tag is unchanged. See [journal contract](windows-local-journal-2026-10-21.md).

### Build-boundary follow-up (same work branch)

Full desktop CI on 36813fe caught an unused `Path` import in non-test builds after restricting construction to tests. The lightweight test harness had hidden that cfg mistake. The import is now gated for test/Windows fixture builds; a non-test example compiles the actual engine sources in adapter CI so this boundary is checked before full builds. Opt-in and default strict local Clippy pass. New full-head CI is required; previous failures are not reported as passes.

## TD-20261021-05：Windows 原型状态不能替代持久 journal，旧释放路径必须隔离

- 日期：2026-10-21（UTC）
- 状态：共享持久层及正式引擎 mock 桥接已实现；当前提交的回归与 CI 结果见 PR，原生生产能力继续关闭。
- 影响版本与环境：PR #39 后续开发；已发布 alpha.2 只读预检没有开放此写入/释放路径，没有真实数据丢失报告。
- 相关历史资料：TD-20261021-02；Linux 缓存世代、写前记录、待处理父操作及上传确认的历史约束。

### 症状与影响

早期隔离探针只知道生成内容，其 pending/open 字段和 engine_state 不能证明生产文件已上传。进程退出、旧 Cached 标签、上传成功但响应丢失或父目录操作未完成时，若仅凭标签允许释放，会有丢失未确认数据的潜在风险。此风险此前通过能力关闭隔离，未在真实目录复现。

### 触发条件与复现

生成文件写入后注入旧 Cached；真实读句柄仍打开；上传持久 intent 已登记但未确认；子进程直接退出而不运行析构；条件上传已提交但 mock provider 丢弃回复。新回归逐字节断言本地内容保留，并检查旧世代、待删/父移动和已变化的 provider/cache 绑定拒绝释放。

### 根因与证据

原型没有生产 journal/owner/世代桥接。另一个相邻边界是旧 Database::release_record 只理解旧 FileState/文件锁；它没有新 journal 的所有权证据，必须拒绝 journal 管理的条目，避免未来 GUI 清缓存绕过原生守卫。并非声称已有生产 CFAPI 挂载发生此故障。

### 解决办法与恢复操作

在 twodrive-core 的现有 SQLite/files/FileState 上增加 opt-in CacheJournal：独占 owner、真实文件锁、读写租约计数、写前世代、条件上传快照 CAS、完整字节摘要与结果未知标记。新 owner 只能在 OS 所有权锁到手后恢复旧计数；遗留写入先保留并 fsync，未知上传下载远端完整内容且重验版本后再判断。旧释放/剪枝路径拒绝有 journal 的条目。引擎使用既有 Provider 条件写能力，不在 Flutter 或适配层复制协议。无真实用户数据恢复或迁移。

补充身份边界：首个 journal 提交仍允许调用方逐次传入 Backend，且未持久核对 Provider 身份/私有缓存根。补上固定 Backend 所有权、现有身份 binding 的域分隔摘要和 canonical cache root；错误身份/根在 epoch 与恢复计数变化前拒绝。生成回归构造相同 opaque ID/版本但不同账户 binding，验证不能接管旧 journal，原脏数据保留。没有真实账户参与。

### 验证结果与边界

本机轻量 harness 编译实际 Windows model/provider/引擎桥接源码，生成文件完成下载→编辑/fsync→条件上传→同世代确认。真实子进程无析构退出验证写入与未知上传重开后的保留。35 项适配/IPC/journal/引擎 harness 与原有 40 项 core 回归、严格 Clippy 通过。受控地仅移除旧释放函数中的新所有权守卫后，生成文件回归在预期保护断言失败；finally 原样恢复源码后通过。exact-head CI 在 PR 记录。没有执行 Windows 重启、断电或真实账号验证；native_mount/import/cloud_writes 仍关闭。

修订版独立 CFAPI 夹具已获另一次明确许可：八项检查通过，注销后查询 0x80070186，三个生成文件含整个脏文件完整保留。该结果只证明修订探针，不等于这里的持久桥接已接入 Explorer。详见 guarded evidence。

### 防复发措施与回归

actual_reader_and_owner_locks_block_competing_writes_and_release、legacy_release_cannot_bypass_native_journal_with_a_stale_cached_label、process_restart_preserves_unclosed_write_and_requires_recovery、process_restart_keeps_ambiguous_upload_and_rejects_blind_retry，以及正式引擎 lost-reply/offline/conflict/version-change 回归。原生 callback/oplock、稳定状态 IPC、重命名/删除/冲突恢复未完成前不开放生产能力。

### 关联记录

[PR #39](https://github.com/LumiaBlack51/twodrive/pull/39)；[持久桥接说明](windows-local-journal-2026-10-21.md)；[修订版原生证据](windows-cfapi-guarded-validation-2026-10-21.json)。已发布 alpha.2 的 tag/资产保持原样；后续源码与它的差异由 PR 新提交记录。

## TD-20261021-04：修改预检路径后仍显示旧文件结果

- 日期：2026-10-21（UTC）
- 状态：已验证原代码失败、修复后通过的 widget 回归；实际物理 GUI 操作未验收。
- 影响版本与环境：PR #39 尚未发布的 apps/dav 预检表单。
- 关联历史故障：无；只读结果归属错误，没有导入或云端写入。

### 症状与影响

检查 A 后把路径改成 B，旧的 A 结果仍显示在 B 下。异步结果也需要防止覆盖新的输入或请求。

### 触发条件与复现

新增 widget 测试先完成 A 的预检，再编辑 B，断言旧结果消失。临时切回旧组件代码时，该测试失败于“Expected: no matching candidates”；finally 原样恢复修复代码。没有真实文件/账户操作。

### 根因与证据

表单只在来源类型切换时清空结果，没有路径 onChanged 失效；响应也未绑定输入/请求世代。

### 解决办法与恢复操作

路径或来源变化都递增世代并清空结果。回包与错误/结束状态必须同时匹配世代、捕获路径、来源与 client 实例；旧 A 完成不能清空或覆盖进行中的 B。无用户恢复操作。

### 验证结果与边界

修复后的 4 项预检测试与 analyzer 通过；包括完成后改路径、A 晚到而 B 仍等待、能力版本拒绝和不把完整文件候选称为同步完成。原代码确定性失败，新代码通过；测试不等同于 Explorer 或用户硬件点击验收。

### 防复发措施与后续

保留延迟 Completable 的 A/B 回归和完成结果失效测试，所有将来输入字段都必须参与结果归属。

### 交付记录

[PR #39](https://github.com/LumiaBlack51/twodrive/pull/39)。最终源 SHA 与 CI 结果见 PR/预发布报告。

## TD-20261021-02：原型同步根路径检查可丢失 junction 证据，未知 CFAPI 错误误当未注册

- 日期：2026-10-21（UTC）
- 状态：已验证生成目录的原始 junction 拒绝、目录替换锁和错误分类；修订版注册场景尚未重新运行。
- 影响版本与环境：未合并 PR #39 的 CFAPI 原型 0.1.7-alpha.1。正式桌面未开放原生挂载。
- 关联历史故障：TD-20261021-01 是构建路径脱敏，本条为注册前隔离约束，机制不同。

### 症状与影响

评审发现先 canonicalize 再检查祖先会隐藏原路径中的 junction；CfGetSyncRootInfoByPath 的所有失败都被当成“不在同步根”，无法区分权限/参数/平台错误。没有对用户真实目录执行注册，已批准的旧夹具没有 junction 且成功注销。

### 触发条件与复现

只在生成目录中创建 junction，向其子路径请求新测试根。新增锁回归还发现仅 FILE_READ_ATTRIBUTES 的句柄并不能阻止本机 fs::rename；该受控测试首次失败，改为目录枚举访问后的相同测试通过。

### 根因与证据

规范化路径不是原路径链的审计证据；错误 HRESULT 不是不存在证明。Windows 属性专用句柄不足以建立所需共享锁约束。测试直接观察到锁存活时目录替换成功，未以假定 Win32 语义填充结论。

### 解决办法与恢复操作

检查原始路径链并保留不共享 DELETE、具备目录枚举访问的句柄；原子 NtCreateFile FILE_CREATE 相对锁定父目录创建新叶，保留叶句柄，阻止检查/注册期间替换。只接受明确的 ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT；其他结果失败关闭。原型注册/释放编译于 fixture-probe 非默认特性，正式桌面依赖不启用，不能用原型的 pending/open 假值允许生产释放。无真实同步根恢复操作。

### 验证结果与边界

本机生成目录测试确认：原 junction 被拒且目标下未创建叶；已存在叶不被采用；锁存活时叶/祖先 rename 被拒，释放锁后允许；未知/权限 HRESULT 被拒。旧属性句柄锁回归失败，新实现通过。未复用已结束的一次注册授权；修订版的完整 CFAPI 注册/读取/释放需另一明确授权后验证。 Windows CI 的目录锁测试曾因同步根查询返回 0x80070001 失败，该环境返回值的更深原因未确认。现将纯文件锁/原子建目录测试与 CFAPI 查询分层；生产 Session 仍必须先通过真实查询再创建叶，未知错误白名单没有放宽。脏文件场景改为逐字节验证整个修改后文件，不只前缀，尚未重新执行该场景。

### 防复发措施与后续

保留真实 junction/目录替换/错误分类回归。正式 writable 桥接仍需持久 item lock、generation、pending journal 与打开句柄、排队上传、过时 Cached/版本变化测试；当前只读预检不提供导入/释放接口。

### 交付记录

[PR #39](https://github.com/LumiaBlack51/twodrive/pull/39)。已发布 alpha.1 只记录原生检查点，后续修订不据此声称已验证注册；后续构建和验证以新 Release 报告为准。

## TD-20261021-03：桌面变异回归未复制新增本地适配层依赖

- 日期：2026-10-21（UTC）
- 状态：已修复待完整 CI 验证。
- 影响版本与环境：PR #39 的 d5d5823，TwoDrive desktop Linux/Windows CI。
- 关联历史故障：无；不是 path-as-ID 行为回归。

### 症状与影响

Rust Clippy 成功后，历史 path-as-ID 变异回归因临时源码副本缺少 experiments/windows-cfapi/Cargo.toml 而失败，尚未执行预期行为断言。没有用户数据操作或运行时错误证据。

### 触发条件与复现

新增路径依赖后，test-provider-old-paths.py 仍只复制 crates 与 twodrive-dev。CI 日志明确为 dependency manifest not found，不能记成旧行为成功复现。

### 根因与证据

变异 fixture 没有包含新的本地依赖闭包；真实工作区 Clippy 已通过，缺失只发生于生成的临时副本。

### 解决办法与恢复操作

复制新增适配层源码但排除 target/cache；显式使用 UTF-8/LF 编辑临时源码。保留原先“必须编译并失败于指定行为断言”的要求，不放宽断言或把任意编译失败当通过。无用户恢复操作。

### 验证结果与边界

d5d5823 的两平台失败日志已确认上述原因；修订后完整回归结果待 CI，不预填成功。正式 Provider/传输实现未修改。

### 防复发措施与后续

新增路径依赖时同步维护临时源码 fixture 闭包。原始 source tree 始终不被变异。

### 交付记录

[PR #39](https://github.com/LumiaBlack51/twodrive/pull/39)，失败 workflow 39551831290；最终结果见 PR exact-head 检查。

## TD-20261021-01：新 CFAPI 探针的默认 Rust 构建保留本机源码路径

- 日期：2026-10-21（UTC）
- 状态：已验证仅候选构建的路径重映射及二进制检查；原候选从未上传。
- 影响版本与环境：本次未发布 CFAPI 0.1.7-alpha.1 实验探针，Windows 11 x64；不影响已安装客户端。
- 关联历史故障：无。本条记录新探针发行前发现的隐私边界问题。

### 症状与影响

默认 release/strip 构建仍有两处当前用户目录前缀，可能来自 Rust 运行时诊断位置；strip 不能作为路径脱敏证明。未上传包含这些路径的二进制，未读取用户文件或凭据。

### 触发条件与复现

在原生 Windows checkout 执行普通 cargo build --release，然后只统计可执行文件中的已知私人目录前缀，不输出其内容。原候选计数为 2。

### 根因与证据

Rust 的诊断位置字符串不完全属于可被 strip 移除的调试符号。当前证据确认前缀存在，不将所有匹配归因于某一个具体 panic 或源文件。

### 解决办法与恢复操作

新增 build-cfapi-probe.ps1：单任务离线可选构建、只对子进程临时设置路径重映射，恢复原环境；输出前检查 UTF-8/UTF-16 中的仓库/用户前缀并执行只读 capabilities 查询。生成的包不含 PDB、用户数据或缓存。无用户客户端/系统恢复操作。

### 验证结果与边界

原候选 2 处匹配；最终重映射原生可执行文件 0 处。重映射版本实际通过新生成同步根的 CFAPI 8 项场景；该测试根已注销，二次只读查询确认不再注册，所有夹具保留。详见 windows-cfapi-validation-2026-10-21.json。检查不是对任意第三方二进制的完整敏感信息审计；本次只分发探针、许可证和脱敏证据。

### 防复发措施与后续

Windows 适配层 CI 使用相同打包脚本，既有输出目录会被拒绝，路径检查失败时不会生成包。Explorer 外观、缩略图和整进程重启均未验收，不以测试成功扩大声明。

### 交付记录

[PR #39](https://github.com/LumiaBlack51/twodrive/pull/39)。发行源 SHA、包哈希与 CI 结果在预发布 verification.json 中记录；本条不预填尚未产生的提交。

## TD-20261016-03：MSIX 新回归继承 PowerShell 7 模块路径导致 5.1 校验失败

- 日期：2026-10-16（Asia/Shanghai）
- 状态：已验证测试子进程环境修复后的原生打包；首轮失败保留。
- 影响版本与环境：PR #35 新增 test_msix_packaging.py；Windows CI 的 pwsh → Python → powershell.exe 链。
- 关联历史故障：TD-20261016-01/02 是被测打包策略；本条是新测试环境缺陷，未修改生产打包算法。

### 症状与影响

[首轮 Windows job](https://github.com/LumiaBlack51/twodrive/actions/runs/38862977139/job/116849284750)
的 Rust、IPC、Flutter 和托盘通过；测试身份与合成商店身份的 MakeAppx 均打印
Package creation succeeded，随后两项子用例因 Get-FileHash 未识别失败，未进行包内容断言。
五组保留身份拒绝和 TestPackage 互斥检查通过。Linux 桌面 job 通过。

### 触发条件与复现

PowerShell 7 工作流启动 Python，Python 直接继承环境再启动 Windows PowerShell 5.1。
首轮真实 CI 两个打包子用例均出现相同命令解析失败。

### 根因与证据

fixture 未清理父进程 PSModulePath。微软文档明确说明：经 Python 等中间进程启动 5.1
会保留 PS7 路径，Microsoft.PowerShell.Utility 等共享模块名可解析到不兼容版本。
日志确认宿主调用链与 Get-FileHash 缺失，但未记录具体被选中的模块文件；
清除该单一环境变量后的相同测试将进一步验证此机制，不推断 PS5.1 本身缺少该命令。

### 解决办法与恢复操作

只从测试子进程的环境副本中删除 PSModulePath，让 5.1 按自身规则构造标准路径。
不修改父进程、用户环境、模块安装、打包算法、证书或用户数据。

### 验证结果与边界

首轮失败保留。源码 432e35c 的 Windows CI 已通过三项原生打包测试：五组保留身份拒绝、
参数互斥、两种模式的实际 SDK 包与内容断言。与首轮相比，生产打包输入/算法未改，
只修正测试子进程模块环境（按大小写不敏感删除环境键），支持该继承机制；
具体被选择的模块文件仍未采集。链接见下。本地主机原生跳过不计为通过。

### 防复发措施与后续

保留真实 5.1 子进程打包，不切换宿主回避覆盖。依据：
[微软 PSModulePath / 中间进程说明](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_psmodulepath?view=powershell-7.6)。

### 交付记录

[PR #35](https://github.com/LumiaBlack51/twodrive/pull/35)；首轮 368eae9 失败，
[432e35c 原生复验](https://github.com/LumiaBlack51/twodrive/actions/runs/38864468289/job/116854018353)通过全部测试及包产物检查。

## TD-20261016-02：MSIX 语言声明包含尚未本地化的英语界面

- 日期：2026-10-16（Asia/Shanghai）
- 状态：已验证实际 SDK 包的语言声明；未执行商店认证或全面 UI 本地化验收。
- 影响版本与环境：Development 0.1.6 清单；不影响稳定 Linux 版。
- 关联历史故障：无。属于语言元数据与现有界面不一致，不是 MSIX 安装失败。

### 症状与影响

清单声明 en-us/zh-cn，但 apps/dav 的页面、操作和错误主要是中文，没有英语本地化委托。
商店语言声明不能据此代表完整英语体验。

### 触发条件与复现

读取 packaging/msix/AppxManifest.xml.in 的 Resources，对照 apps/dav/lib/main.dart 和 workspace.dart。
未报告或模拟英语设备上新的运行时崩溃。

### 根因与证据

手工清单声明两种语言，但当前 Flutter UI 尚未实现相应英文资源。此结论来自源码，非认证结果。

### 解决办法与恢复操作

只保留 zh-cn；将来完成英语界面再添加。不修改用户系统语言、应用配置或已发布附件。

### 验证结果与边界

本地主机 XML 解析通过。432e35c 的原生回归已读取两种实际 MakeAppx 包的 Resources，
均只有 zh-cn；见 TD-20261016-03 的 CI 链接。不等于 UI 全面验收或英语系统实机测试。

### 防复发措施与后续

保留 test_msix_packaging.py 的产物语言断言。首版商店材料如实描述中文界面。

### 交付记录

见 [Microsoft Store 评估](microsoft-store-readiness-2026-10-16.md)及
[PR #35](https://github.com/LumiaBlack51/twodrive/pull/35)；验证源码 432e35c，CI 链接见 TD-20261016-03。

## TD-20261016-01：非测试 MSIX 构建允许保留 unsigned 测试身份

- 日期：2026-10-16（Asia/Shanghai）
- 状态：已验证原生输入拒绝及两种打包模式；未核实真实 Partner Center 身份。
- 影响版本与环境：Development 0.1.6 的 scripts/build-msix.ps1；未观察到商店实际误提交。
- 关联历史故障：TD-20261015-01 涉及 SDK 清单 schema；本条涉及测试/商店语义隔离，机制不同。

### 症状与影响

不传 -TestPackage，却手动传入 TwoDrive.Development.Test 或保留 unsigned OID，
旧脚本只检查 Name 格式和 Publisher 的 CN= 前缀，可进入标为 store-unsigned 的输出路径。
输出命名不能保证身份适合商店。没有发现正式签名或真实账号被冒用。

### 触发条件与复现

给非测试路径补齐三个身份参数，其中 Name 为保留测试名称，或 Publisher 含
2.25.311729368913984317654407730594956997722 OID。旧源码的全部输入条件均允许它们。

### 根因与证据

原有互斥仅在 -TestPackage 分支禁止外部身份参数，反向路径没有拒绝保留身份。
微软 unsigned 测试 OID 不属于正式 Publisher；应在 staging/SDK 前失败。

### 解决办法与恢复操作

非测试路径拒绝保留测试 Name（忽略大小写）及 OID（兼容 OID. 前缀、大小写和空格）。
不改正式 Publisher、不生成证书、不信任证书、不安装或提交任何包，无用户恢复操作。

### 验证结果与边界

新增 Windows 回归用五组保留 Name/Publisher 输入确认明确拒绝、且没有创建输出目录；
用真实 SDK 对完整 bundle 分别跑测试模式及临时合成商店身份模式，校验清单/版本/语言、
未签名状态、渠道标记、便携安装器排除、包 SHA-256 与保留文件逐字节一致。
合成身份不是 Partner Center 身份。432e35c 的 Windows CI 已通过三项原生测试，
见 TD-20261016-03 的链接；Linux 主机的原生跳过不计为通过。
本次未运行实机、WACK 或认证，也未把源码推断写成原机重现。

### 防复发措施与后续

Windows MSIX 工作流必须执行 test_msix_packaging.py；合成身份包仅存在临时目录，
不会上传为发行附件。真实 Partner Center 身份所有权仍需发布者核实。

### 交付记录

见 [Microsoft Store 评估](microsoft-store-readiness-2026-10-16.md)及
[PR #35](https://github.com/LumiaBlack51/twodrive/pull/35)；验证源码 432e35c，CI 链接见 TD-20261016-03。

## TD-20261015-19：本地 Deb 验收重复踩到 Windows 换行，并误用 shell 命令缓存

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证本地 fixture 修正；发布 Deb 未修改。
- 影响版本与环境：Development 0.1.6 额外本机 Ubuntu 24.04 Docker/WSL2 验收。
- 关联历史故障：TD-20261015-12 已记录 CRLF/裁剪文档问题；本次 fixture 未沿用已有前置检查，是验收设置复发。命令 hash 缓存是新增断言问题。

### 症状与影响

Deb 安装成功后，直接执行 Windows checkout 的 debian/tests/smoke 返回 set: Illegal option（CRLF）。之后完整 CLI/SQLite、FUSE 和 GTK 检查通过且 apt purge 成功，但同一 dash 进程的 command -v 仍返回曾缓存的 /usr/bin/twodrive，导致误报。

### 触发条件与复现

把 Windows 工作区直接只读挂进 Linux 容器，并在同一 shell 中先运行命令、卸载包，再只用 command -v 判断文件是否存在。所有路径、配置和 mock 数据均在一次性容器内，无宿主应用安装或用户数据操作。

### 根因与证据

日志明确定位到 checkout 脚本的第二行 set，未进入产品 smoke；后续 apt 日志明确记录移除及 purge，但命令查找仍命中 shell cache。两个错误属于 fixture，不归因于 Debian 归档或 FUSE。沿用已有验证方法，本地脚本副本规范 LF，安装前去掉容器文档排除规则；卸载后清空 hash 并同时检查真实文件路径。

### 解决办法与恢复操作

仅修正 dist/ 下的一次性容器脚本，不修改发布 Deb。最终新容器全程退出码 0：已安装 CLI/SQLite、真实 mock FUSE hydration/write/upload/move/delete/pin/release、GTK 设置在 Xvfb 存活、purge 后命令和文件均移除。

### 验证结果与边界

最终产物 twodrive_0.2.14+dev0.1.6-1_amd64.deb；CI 的升级/remove/reinstall/purge 与合成用户状态保护另外独立通过。不是 live OneDrive 或 Debian unstable/sbuild/piuparts 验收。

### 防复发措施与后续

复用 tracked 生命周期脚本的 LF/文档前置条件；删除程序后同时检查文件并 hash -r。发布 runtime-validation.json 记录最终成功与前置失败，保留错误边界。

### 交付记录

应用/打包输入 e64bd76 未变；最终下载文件哈希以 Development 0.1.6 附件报告为准。

## TD-20261015-18：MSIX 验收退出辅助函数可能选择错误的窗口线程

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证受控旧失败/新通过与实际 MSIX 全流程；首次超时当时的窗口枚举序列未采集，不能反推为已还原现场。
- 影响版本与环境：test-msix-runtime.ps1，Windows 11 本机 Development 0.1.6 验收；应用二进制未修改。
- 关联历史故障：TD-20261015-11 为包上下文启动边界；本次是测试窗口选择，根因不同。

### 症状与影响

首轮实际 MSIX 安装、GUI/引擎包身份和更新标记通过，测试注入 WM_QUIT 后十五秒未退出；finally 回收进程、卸载并恢复依赖注册。没有修改信任证书或用户应用。

### 触发条件与复现

旧 Close 按 PID 选择 EnumWindows 的第一个窗口，未校验窗口类。新增真实 Win32 fixture：同 PID 的 Flutter 类 runner 与另一线程的 STATIC helper；把 helper 放在枚举首位并先断言前置顺序，再调用原 Close。旧 helper 测试 3.52 秒失败，runner 未收到 quit；新 helper 0.45 秒通过。

### 根因与证据

PID 不等于 GUI message-loop 线程；同进程的 COM/OLE/helper 窗口可能位于另一线程。受控 fixture 证实该选择缺陷。原首次超时未记录窗口类/线程，所以不声称证明它一定由同一个 helper 窗口触发。

### 解决办法与恢复操作

仅改测试：PID 与 FLUTTER_RUNNER_WIN32_WINDOW 同时匹配，检查 PostThreadMessage 返回值、记录候选类/线程并拒绝找不到 runner 的情况。保留原等待时限、真实包身份与退出后引擎回收断言；无应用代码、权限或证书策略变更。

### 验证结果与边界

受控原 helper 失败、新 helper 通过；同一最终 CI MSIX 的真实安装、AUMID GUI/引擎身份、分发标记、正常消息循环退出/引擎回收、包调试上下文六组 IPC/QUIC、卸载与依赖注册恢复通过。消息注入不等于真实鼠标托盘菜单点击；包调试 token 也不等同于全部 GUI/OAuth 交互。

### 防复发措施与后续

保留 test_msix_window_target.py 的原生多线程窗口回归，Windows MSIX CI 包装前执行；首次现场未采集的窗口顺序保持未知，不以重跑成功补写根因。

### 交付记录

测试辅助函数与记录单独提交；应用/打包输入仍为 e64bd76，runtime-validation.json 记录同一最终 MSIX 的实际结果。

## TD-20261015-17：dev 发布工作流仍沿用不完整的 Debian 源码准备

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证完整离线源码/二进制构建、Lintian、清单、安装升级/移除/purge 与源码解包 clean 校验。
- 影响版本与环境：dev 截至 c577813 的 Debian preparation 工作流及 Development 0.1.5 源码附件；不影响 MSIX 或既有用户数据。
- 关联历史故障：main 的 Debian 标准打包迁移；本条记录 dev 尚未移植完整路径的差异。

### 症状与影响

旧 dev 工作流只从预先填充的 Cargo cache 离线构建二进制，再独立生成没有
vendor 组件的源码包。接收源码包者无法在空 Cargo home 中离线重建。
二进制可运行并不证明源码输入闭包完整。

### 触发条件与复现

检查旧 scripts/build-deb.sh、debian/rules 和 debian-package.yml：binary-only
构建与后续 git archive 源码打包分开，未提供锁定依赖源组件。尚未执行新的
完整源 CI；不将静态证据写为已完成运行验证。

### 根因与证据

main 已增加完整源码准备、vendor 和许可证输出；dev 只合入强制政策文档，
仍保留先前的 preparation 实现，因此政策与执行路径不一致。

### 解决办法与恢复操作

移植 main 的源码准备、debhelper、完整清单校验、vendor 清理保护、包内容
和生命周期检查；保留 dev VCS 元数据并使用独立 upstream 版本
0.2.14+dev0.1.6，避免以稳定版原 tarball 名覆盖不同源。无用户恢复操作。

### 验证结果与边界

构建输入为 e64bd76485c225c460c8189f54477a9e22728a3f；[直接源 Debian CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38739280494) 已成功：完整 dpkg-buildpackage/debhelper 构建、包内容/权限/自动依赖、所有源码与二进制清单、Lintian 无 error、旧包升级及 remove/reinstall/purge 保留合成用户状态、从 .dsc 解包后的全部 vendor 文件校验、clean 后复验和空 Cargo home 离线依赖解析。

[原生 Linux/Windows CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38739010221) 与 [双平台 QUIC CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38739010210) 已成功。本机额外执行 Nautilus 25 项、Linux verifier fixture 1 项与 Windows 发布附件 6 项/进程清理 fixture 1 项回归通过。Windows 的 POSIX 远端路径 fixture 不能替代 Linux 执行；本机缺少 GTK 依赖的全脚本 discovery 不记为通过，带完整依赖的 Linux CI 已验证相应套件。

Ubuntu CI 的 rustup 编译器依赖检查旁路继续明确记录，不据此声称通过 Debian 官方入库、sbuild 或完整 Rust 授权审计。最终下载附件及额外安装运行检查以 Release 的 verification.json/runtime-validation.json 为准。

### 防复发措施与后续

main/dev 共用同一 build-deb.sh 入口；从 .dsc 解包后检查全部 vendor 校验和，
执行 clean 再复验，并在空 Cargo home 中离线解析锁定依赖。上传全部 source/
binary/debug/buildinfo/changes，发布前校验每份清单与远端附件。

### 交付记录

代码与打包输入提交 [e64bd76](https://github.com/LumiaBlack51/twodrive/commit/e64bd76485c225c460c8189f54477a9e22728a3f)，[PR #33](https://github.com/LumiaBlack51/twodrive/pull/33)。后续提交仅补充本文验证证据，不改变该构建输入。发布源、最终公开状态与附件哈希以 Release 机器报告为准；详情见 [发布记录](development-0.1.6.md)。

## TD-20261015-16：同实例元数据写者竞争耗尽 SQLite 锁等待

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证受控旧失败/新通过与错误后放行；最终 Linux/Windows 候选检查见 PR #30。原历史 Windows 宿主的逐操作时序仍不可追溯。
- 影响版本与环境：main 与 dev 共用 twodrive-core；历史 Windows native CI 38730372070 的八 writer 测试间歇失败。受控复现在 Linux 临时测试数据库，无用户数据操作。
- 关联历史故障：[main TD-20261015-15](https://github.com/LumiaBlack51/twodrive/blob/main/doc/incidents.md) 与 TD-20261014-07 此前未确认或修复争用机制。本条补充已证明的同实例写者争用机制，不将旧历史缺失的时序补写成已确认事实。

### 症状与影响

八 writer 各写二十条时可返回 SQLite Busy，导致部分调用报错而非完成全部 160 条。没有已确认的用户数据损坏。原 native Windows 日志只有 join 处的 error code 5，不能从它推断具体语句。

### 触发条件与复现

诊断候选 a9c28c7 增加静态阶段、合成 worker/序号、耗时及扩展码。Linux 十轮单项、独立 20/40ms sync 延迟均通过；Windows 原 core 套件加八轮默认并发、随后八轮 32 测试线程的 core 复验也全部通过，证明原故障并非每次出现。

保留 scripts/test-database-contention.sh：仅在 disposable core 测试进程中预加载 fixture，将 fsync/fdatasync 放到共享队列并各延迟 40ms，32 个测试线程并行。旧代码实测 39 passed、1 failed，100.54 秒；同一并发写入测试报告 writer=0 item=0 elapsed=10.0536s sqlite_code=5，阶段为 metadata upsert: beginning write transaction。这是可控共享存储竞争，不是原 Windows 宿主的精确重放。

### 根因与证据

Database clones 共享路径但没有共享元数据写入准入；每次 upsert 先提交元数据事务，再开启单独的全表 pin 重算事务。竞争者可在多个事务之间反复取得唯一写锁，使等待中的调用在事务开始处耗尽十秒 busy_timeout。受控日志定位到 BEGIN，排除了该复现中的快照升级或 WAL 读恢复错误。SQLite 只允许一个 writer，busy handler 不是应用级完整操作的排队协议；见 [SQLite isolation](https://www.sqlite.org/isolation.html) 与 [busy handler](https://www.sqlite.org/c3ref/busy_handler.html)。历史 Windows 原始日志缺少阶段，仍不能证明当时每一步与本次完全一致。

### 解决办法与恢复操作

Database clones 共享一个元数据写入 Mutex；单条和批量导入均在准入后完成整次元数据更新、后代移动和 pin 重算，返回或出错时释放。读取不取该锁，其他 Database 实例和进程继续由 SQLite 仲裁。十秒 SQLite 超时、Immediate 事务、持久化、八 writer/160 条断言均保留，不吞掉 Busy 或放宽断言。没有修改用户服务、账号、现有数据库或已发布包。

### 验证结果与边界

相同受控 fixture 下，仅加入准入后的原 40 项 core 全部通过，128.17 秒；旧失败/新通过成立。新增 failed_metadata_write_does_not_block_another_clone，以真实 SQLite trigger 拒绝单条写入后，另一 clone 的批量导入必须在十秒内完成，并验证拒绝行未写入、接受行内容正确。最终本机正常 core 41 项、严格 core Clippy、格式及 diff 检查通过。最终可复用 fixture 全 41 项通过（129.75 秒）；本机完整工作区 build、fmt、严格 Clippy、test 及 Nautilus 25 / Settings 4 项 Python 检查通过。最终原生结果见 PR #30 及本分支 PR 的检查；验证只使用临时 SQLite，没有真实 OneDrive 端到端验收，也不证明不同实例/不同进程的永久持锁或所有 SQLite Busy 均已消失。

### 防复发措施与后续

保留原八 writer 并发测试及 worker/扩展码诊断、错误后放行回归、Linux 慢共享 sync fixture；Windows CI 额外执行三轮 32 线程 core 套件。后续 Busy 应据阶段、扩展码及耗时区分同实例排队、外部写者和存储故障，不以重跑成功代替机制验证。

### 交付记录

修复与记录随 [PR #30](https://github.com/LumiaBlack51/twodrive/pull/30) 交付；首轮诊断提交 [a9c28c7](https://github.com/LumiaBlack51/twodrive/commit/a9c28c7fbd49af60612ed4b2f424297abb1456a2)、更高并发诊断提交 [94192f0](https://github.com/LumiaBlack51/twodrive/commit/94192f0c7b0bf19042ba0315096f0530d5a313b2)。机制修复提交 [aa83dcd](https://github.com/LumiaBlack51/twodrive/commit/aa83dcd)；本轮未创建 Release。

## TD-20261015-12：隔离 Deb 安装验收的根文件系统前置配置不完整

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证隔离安装、GTK 启动、已安装二进制的 mock FUSE 及卸载；不是 Deb 产品缺陷。
- 影响版本与环境：0.2.14-2 Deb 的本地 Ubuntu Base / WSL2 验收 fixture。
- 关联历史故障：TD-20261015-04/06 涉及包内容；本次是测试环境前提，不是同根因。

### 症状与影响

首轮 apt 因 `_apt`/nobody 的 setuid/setgroups 被拒绝而停止；随后 fixture 的 shell CRLF 触发非法 set 参数；Ubuntu Base 的默认 dpkg path-exclude 又使安装时省略手册，必需文档断言失败。均发生于隔离根目录，无宿主安装。

### 触发条件与复现

bubblewrap 单 UID user namespace 中运行需要多个系统 UID 的 apt；直接复制 Windows checkout 中的 shell fixture；使用带有裁剪手册/文档配置的 Ubuntu Base。三项是不同前置配置问题，未将它们归因于同一产品机制。

### 根因与证据

UID 映射只允许 0，apt 无法切换 `_apt`；fixture 含 CRLF；根文件系统显式配置 `path-exclude=/usr/share/man/*`。Deb 归档本身的手册与依赖检查均通过，CI 标准环境也已通过。

### 解决办法与恢复操作

仍隔离 mount/PID/IPC/UTS，使用已有 root 身份的完整 UID 范围；只对测试副本规范换行，只在 disposable rootfs 去除文档裁剪规则并重装。宿主应用、服务与系统策略未修改。

### 验证结果与边界

真实 deb 安装、上传清单/权限/依赖、CLI、GTK 设置进程在 Xvfb 存活、实际 `/usr/bin` CLI/daemon 的 FUSE hydration/write/upload/move/delete/pin/release，以及卸载全部通过。GTK accessibility bus 警告保留，未见 traceback/崩溃。后端为 mock，不是云账号或 Debian unstable/sbuild 验收。

### 防复发措施与后续

复验时检查 UID 范围、LF 和 dpkg 排除策略；保留包内容检查与运行记录。[0.1.5 验证](development-0.1.5.md)。

### 交付记录

[本机脱敏记录](runtime-validation-0.1.5.json)；最终发布源与附件哈希以 dev-v0.1.5 的 verification.json 为准。

## TD-20261015-11：MSIX 验收进程从包外启动内部程序被拒绝

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证实际 GUI 激活及包调试上下文 IPC/QUIC；没有修改应用安全权限。
- 影响版本与环境：Development 0.1.5 原生 Windows 11 安装验收 fixture。
- 关联历史故障：TD-20261015-09 同属测试生命周期；不是同一根因。

### 症状与影响

MSIX 安装、GUI/引擎真实包身份、退出及卸载通过，但从普通 Python 直接执行 WindowsApps 内引擎被 WinError 5 拒绝；失败构造器还遗留 stderr 文件句柄，使临时目录清理报 WinError 32，掩盖原错误。

### 触发条件与复现

未带包身份的外部 Popen 启动安装程序内部 helper。另一次外部代理尝试也由 GetCurrentPackageFullName 明确证实不继承包身份，已弃用该测试代理。

### 根因与证据

真实 GUI 启动的内部引擎拥有包身份；外部 fixture 不拥有身份，不能作为同一运行环境。旧 Engine fixture 在 Popen 抛错前打开日志，异常路径未关闭；受控 PermissionError 回归能复现句柄仍打开。

### 解决办法与恢复操作

使用微软官方 Invoke-CommandInDesktopPackage -PreventBreakaway 启动原共享测试，验证调试进程身份；实际 GUI 仍单独通过 AUMID 激活验证。Popen 异常路径关闭 fixture 日志。没有放宽 WindowsApps ACL、添加信任证书或修改安全设置。

### 验证结果与边界

安装、真实 GUI/引擎身份、更新通道标记、退出回收、包调试上下文六组真实 IPC/QUIC、卸载和依赖注册恢复通过。句柄关闭回归通过。包调试 token 与真实 GUI token 并非完全相同，不能据此宣称所有原生文件对话框、OAuth 或商店认证已验证。

### 防复发措施与后续

保留 test-msix-runtime.ps1、test-msix-context.py 和受控创建进程失败回归；测试拒绝替换已有同名测试包，最终恢复注册状态。[0.1.5 验证](development-0.1.5.md)。

### 交付记录

[本机脱敏记录](runtime-validation-0.1.5.json)；正式发布前须通过最终源 CI 与附件校验。

## TD-20261015-10：Windows 配置替换拒绝再次阻断桌面 CI

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证受控短时占用恢复与持续拒绝保护；CI 原拒绝来源仍未确认，最终原生结果见 PR #22。
- 影响版本与环境：PR #22 的 d3a40f3，Windows 原生 runner；旧连接管理测试在编辑非活动配置时失败，尚未进入本次共享烟测。
- 关联历史故障：TD-20261014-09 同为 persist 的 os error 5，但未采集竞争句柄，不能确认具体来源相同。TD-20261014-10 的内存配置保护仍保留。

### 症状与影响

`test-dav-desktop.py` 编辑非活动连接时返回 `failed to persist temporary file: Access is denied. (os error 5)`。CI 正确中止；没有用户数据操作。

### 触发条件与复现

[失败运行 38694139035](https://github.com/LumiaBlack51/twodrive/actions/runs/38694139035) 的 Windows IPC 步骤；Linux 相同步骤通过。与历史类似的错误再次出现，因此补充实际 Windows 句柄占用测试，不仅重跑检查。

### 根因与证据

可以确认配置原子替换被操作系统拒绝；原 runner 的占用句柄/属性/竞争者仍未知，不断言是防护软件。受控测试用缺少 FILE_SHARE_DELETE 的真实只读句柄重现替换拒绝，证明这种短时占用机制及旧内容保留边界；不把它当成已还原原 CI 竞争者。

### 解决办法与恢复操作

Windows 在错误 5/32/33 时重用同一已写入并同步的临时文件，最多重试六次，总等待 525 ms，仍失败则返回原错误；其他错误立即返回。始终使用原子替换，不先删除旧文件，不改变 ACL、关闭防护或忽略测试。共享连接文件的显式替换同样使用此机制。失败时内存配置仍在落盘成功前保持旧值。没有用户恢复操作。

### 验证结果与边界

Windows 两项新测试通过：同一真实句柄先证明无重试的 persist 失败、旧内容保持，再于第一次重试等待中确定性释放句柄并成功替换；持续占用严格尝试六次等待后失败，实际 State::save 也返回错误且旧 JSON 保持。严格 Clippy、完整 Rust 64 项测试均通过。最终双平台 CI 结论见 PR #22。不能承诺所有 Windows 原子替换拒绝已消失；永久占用和权限拒绝仍应明确报错。

### 防复发措施与后续

保留短时占用释放后成功、持续占用重试有界/旧字节保持两项 Windows 回归，以及已有配置持久化失败时内存不变化测试。最终记录在 [PR #22](https://github.com/LumiaBlack51/twodrive/pull/22)。

### 交付记录

修正代码、验证产物与最终原生 CI 见 [PR #22](https://github.com/LumiaBlack51/twodrive/pull/22)。没有发布新版 Release。

## TD-20261015-09：共享退出烟测在 quit 后继续复用 IPC

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证测试修正；未发现本项对应的产品退出缺陷。
- 影响版本与环境：本次尚未提交的退出烟测草稿；本机 Windows 隔离临时目录。
- 关联历史故障：无；测试生命周期问题。

### 症状与影响

加固退出检查时，在 `quit` 后继续发送 `status`，测试等待无响应。没有操作用户配置或真实文件。

### 触发条件与复现

同一 Engine fixture 发出 `quit` 后又调用 `status`。worker 已按协议退出，不能再处理后续请求；reader 仍等待关闭标准输入。

### 根因与证据

既有 desktop worker 明确在成功 quit 后 break。新烟测误把 quit 当作仅停止共享，与产品协议不符。不是 GUI 的常规状态轮询死锁。

### 解决办法与恢复操作

使用 fixture.close 发送 quit、关闭 stdin 并等待进程退出，再创建新的引擎检查共享未启动及运行锁释放。本地只终止已核实为该草稿烟测的 Python 进程；测试引擎/共享子进程当时已退出。

### 验证结果与边界

修正后真实 IPC/本机 QUIC 六组检查全部通过，包括同引擎同时作为客户端/服务端，以及退出后新引擎立即恢复相同共享。未修改产品 quit 行为。

### 防复发措施与后续

保留进程重启而非 quit 后复用 IPC 的验收步骤；双平台 CI 运行相同脚本。

### 交付记录

代码与后续原生 CI 见 [PR #22](https://github.com/LumiaBlack51/twodrive/pull/22)，尚未发布新版 Release。

## TD-20261015-08：新增共享导航使最小窗口的设置入口被裁剪

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证 Flutter 最小窗口回归；原生桌面 CI 结论见 PR #22。
- 影响版本与环境：本次尚未发布的桌面共享 GUI；840×580 管理窗口。
- 关联历史故障：无同根因已知条目；此次为第六个导航项目带来的新回归，触发现有最小窗口测试。

### 症状与影响

新增“共享”后，原设置页测试点击未命中，无法找到自启动开关。导航文本存在于树中，但被下方状态区裁剪。没有文件或配置损坏。

### 触发条件与复现

原有 `minimum-height window keeps long errors and navigation usable` 及更新提示/MSIX 测试在 840×580 稳定失败；点击坐标落入底部状态容器。

### 根因与证据

侧栏品牌区域较高，六项默认高度的 ListTile 超过可用高度；原测试的 scrollUntilVisible 判断元素已存在，未使可点击中心进入视口。没有放宽测试或忽略 hit-test 警告。

### 解决办法与恢复操作

缩短品牌留白并使用 dense 导航行，使六项在最小窗口可点击。共享页继续独立滚动。没有用户数据恢复。

### 验证结果与边界

修复后完整 33 项 Flutter 测试通过；新增共享测试也在 840×580 覆盖选择目录、启动/导出/停止。首次新 widget 测试另因 fake-async 区内等待异步临时目录创建停滞，改为同步隔离临时目录并保留文件导出异步等待；这是测试 harness 修正，不是产品 IO 卡死证据。Flutter analyze 通过。

### 防复发措施与后续

保留原最小窗口/设置回归及新增共享页交互测试。原生 Windows 构建、托盘及 Linux CI 结果见[共享验证](desktop-sharing-validation.md)。

### 交付记录

实现 [d3a40f3](https://github.com/LumiaBlack51/twodrive/commit/d3a40f307de116d4a868ee3874a1fd2b6de96db3)，原生 CI 结论见 [PR #22](https://github.com/LumiaBlack51/twodrive/pull/22)；尚未发布新版 Release。

## TD-20261015-07：邀请导出边界比较混用 Windows 路径形式

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证本机 Windows 单元及实际 IPC/QUIC 边界；Linux CI 结论见 PR #22。
- 影响版本与环境：新增 GUI 导出测试；既有 State.invite 的路径边界比较。正常 serve 会保存 canonical 根路径，尚未证明正常发布流程存在泄露。
- 关联历史故障：无；与 TD-20261015-04 打包路径不同。

### 症状与影响

新测试手工保存非 canonical 的 share.root 后，把连接文件写到共享目录内没有被拒绝。若使用这种异常状态，访问邀请可能放进已共享目录；没有使用私人配置或发生实际用户泄露。

### 触发条件与复现

Windows fixture 的共享根为普通盘符路径，输出父目录 canonicalize 后带扩展路径前缀。新增导出测试的 inside-share 拒绝断言在旧比较方式失败。

### 根因与证据

代码 canonicalize 输出父目录，但直接与保存的 share.root 作 starts_with 比较。文本形式不一致使包含判断漏过。实际 serve 的 prepare_share 已 canonicalize，因此不把 fixture 结果扩大为所有正常服务都能复现。

### 解决办法与恢复操作

两边都 canonicalize 后判断。GUI 额外禁止共享私有状态目录及导出到私有状态；导出采用同目录临时文件和禁止静默覆盖，显式替换后才轮换邀请。没有本地用户数据恢复操作。

### 验证结果与边界

旧比较方式在新增 Windows 导出测试失败，修复后通过。Rust 38 个库测试和 9 个 desktop 引擎测试通过；实际隔离 IPC 烟测证明共享目录内导出被拒绝、原文件不被静默替换、单次邀请授权、撤销及权限轮换。Unix 0600 断言仅在 Unix CI 执行，不能依据 Windows 结果宣称已验证。

### 防复发措施与后续

保留非 canonical fixture、导出/导入秘密边界测试及双平台真实 QUIC 烟测。公开文档不得包含邀请文件实际秘密。

### 交付记录

实现 [d3a40f3](https://github.com/LumiaBlack51/twodrive/commit/d3a40f307de116d4a868ee3874a1fd2b6de96db3)，原生 CI 结论见 [PR #22](https://github.com/LumiaBlack51/twodrive/pull/22)；尚未发布新版 Release。

## TD-20261015-06：Lintian 输出管道掩盖失败退出码

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证 CI 错误门禁及完整产物清单；官方发行版入库仍未完成。
- 影响版本与环境：本次新增 Debian workflow 至 b190d36；没有据此上传正式 Release 或 Debian archive。
- 关联历史故障：TD-20261015-02 同样涉及校验未阻止后续动作，但这里根因是 shell 管道状态，非 Python 发布逻辑。

### 症状与影响

CI 显示成功，下载的 lintian.log 却有 E 级错误：历史 changelog 使用 noreply@localhost；源码中的紧凑原型 HTML 被判定 source-is-missing。首条进展曾依据 CI 绿色状态报告 Lintian 通过，下载日志后已纠正。

### 触发条件与复现

lintian --fail-on error ... | tee ... 位于未显式指定 shell 的 run 中；没有 pipefail 时退出码来自 tee。38688223783 保存了二进制和源码的实际错误。

### 根因与证据

流水线缺失显式 bash/pipefail，失败检查仍允许安装烟测与源码打包继续。原型是库内自包含 HTML/CSS/JS 源码，但极长压缩式行触发 Lintian 启发式检查；不是可以省略来源证明的外部二进制。

### 解决办法与恢复操作

设置 job 的 shell: bash，并在两个 Lintian 步骤显式 set -euo pipefail。早期手工包 changelog 原文移至 doc/legacy-debian-changelog.txt 保留，不篡改其作者与邮箱；debian/changelog 从本次标准化包装开始。原型用固定版本 Prettier 展开为可读源码；pkg-config Build-Depends 更新为 pkgconf。无用户数据恢复。

### 验证结果与边界

此前 CI 的 debhelper 构建、实际依赖/权限检查、安装/移除及源码归档已通过，但 Lintian 未通过，不能引用旧绿色状态作为规范合格证据。[修正管道后的 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38689079843) 二进制 Lintian 日志为空，源码检查仍报原型长行且正确失败，证明错误会阻断任务。Prettier 未拆分模板字面量中的 HTML，最长仍为 1425；再用 JavaScript 行续接保留原字符串值，最长降到 341，删除新增续接后的文本一致且 node --check 通过。最终源码 Lintian 待复验，仍不代表官方入库。

### 防复发措施与后续

保留显式 pipefail；上传二进制/源码 Lintian 日志，人工复核错误及警告，禁止以覆盖或 continue-on-error 隐藏发现。[732c7f6 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38689723105) 已实现二进制 Lintian 空日志、源码无 E 级错误，仅有旧检查器不识别 Standards-Version 4.7.4 的 W 级提示。下载后对 .changes 做完整校验时又发现 helper 未复制其引用的自动生成 .ddeb；主 deb 可安装但上传集合不完整。已补齐 debug 包复制，并把 .changes/.dsc 的所有文件存在性、大小和 SHA-256 纳入 CI 门禁。最终结果见发行准备记录。

### 交付记录

[受影响 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38688223783)、[PR #21](https://github.com/LumiaBlack51/twodrive/pull/21)。最终 [f22a121 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38690583450) 成功，下载后再次完成两份上传清单的全部文件大小/哈希核对。二进制 Lintian 空日志、源码无错误，唯一提示为旧检查器不认识 Standards-Version 4.7.4。产物哈希见[验证记录](distribution-validation-2026-10-15.json)。

## TD-20261015-05：本机 Windows 托盘烟测未获得前台焦点

- 日期：2026-10-15（Asia/Shanghai）
- 状态：独立原生 CI 对照通过；本机前台焦点前提未满足的原因仍未确认。
- 影响版本与环境：本次 b190d36 Windows 便携构建；用户桌面内运行既有 test-desktop-windows-ui.py，未使用私人配置。
- 关联历史故障：TD-20261013-11 是窗口已在前台时的内部 focused_ 状态缺口；本次断言表明窗口未在前台，不能视为同根因复发。

### 症状与影响

程序启动、托盘配置和面板显示成功，但 fixture 在转移焦点前断言 GetForegroundWindow()==source 失败；尚未执行后续外部焦点、Escape 或重复切换检查。

### 触发条件与复现

执行既有原生 Windows smoke 一次；日志 panel shown 为 panel=1、visible=1、focused=0，进程没有崩溃。测试正常回收自己创建的进程和临时状态。

### 根因与证据

断言证实当前前台窗口不是测试面板，不能凭此确定是 Windows 前台权限、其他窗口活动还是产品缺陷；未重复干扰用户前台，也未放宽测试断言。

### 解决办法与恢复操作

保留日志与结果，交由 GitHub 原生独立 runner 对照；没有修改托盘代码、用户配置或系统焦点策略。

### 验证结果与边界

Flutter analyze、30 项测试及 Windows release 构建通过；真实公开 GitHub 元数据检查识别 Development 0.1.4；最终测试 MSIX 通过 MakeAppx 并核对四个核心二进制哈希、图标尺寸、身份和更新通道标记。这些结果不能替代未完成的本机原生焦点检查，更不是签名 MSIX 安装后验证。

### 防复发措施与后续

保留原生 CI 烟测和明确的环境边界，最终 CI 结果见发行准备记录；不将没有进程崩溃表述成所有桌面行为已通过。

### 交付记录

[PR #21](https://github.com/LumiaBlack51/twodrive/pull/21)，[最终 Linux/Windows 桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38690583401) 全部成功。Windows 原生烟测明确通过后台隐藏、重复展开、稳定位置、外部焦点收起及 Escape 通知收起；本机未重新争抢用户前台，未将该 CI 结果表述为本机实体交互已通过。

## TD-20261015-04：Debian 手工包缺少自动依赖与源码打包流程

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证标准构建、依赖/权限、安装/移除及源码/完整产物清单；官方入库准备仍未完成。
- 影响版本与环境：dev 47e264f，原 scripts/build-deb.sh 与 debian/；稳定版 0.2.14-1 的现有资产未修改。
- 关联历史故障：TD-20261015-02 是发布校验错误；本项是包内容生成与元数据问题，根因不同。

### 症状与影响

原脚本直接 dpkg-deb 打包，独立 control 写死 amd64/版本且缺少 ELF 运行库依赖；debian/rules 仅 dh，无 Cargo 构建过程；不存在 source/format、DEP-5 版权、manpages。可以生成可解压的 deb，但不足以作为 Debian 规范源码打包及官方入库依据。

### 触发条件与复现

静态检查 scripts/build-deb.sh、packaging/deb/control、debian/control 与 debian/rules 即可确认两份元数据和缺少构建规则。新 tests 检查真实产物的 libc/SQLite/Python 依赖、root 权限与必需文档；旧版 0.2.14-1 发布二进制在新检查上因缺少 libc6 依赖稳定失败，新包通过。

### 根因与证据

手工归档绕过 dh_shlibdeps、dh_python3 和标准文档处理。另在本次新增 MSIX 首轮校验时，PackageDependency 使用了 foundation schema 不支持的 ProcessorArchitecture，MakeAppx 报 0x80080204；删除该属性后同一工具通过。它是新打包脚本的开发错误，不是既有发布故障。

### 解决办法与恢复操作

改用 debhelper、dpkg-buildpackage、唯一 debian/control；加入离线构建、系统 SQLite、标准源码格式和安装 smoke。不启动用户服务。新增 MSIX 脚本用 Windows SDK 校验，并要求正式包提供 Partner Center 身份。无用户数据恢复操作。

### 验证结果与边界

Windows 37 个库测试及 7 个引擎测试通过，旧发布 bundle 的 MSIX 通过 SDK 校验。Flutter 首轮因本机 SDK 3.47.4 与锁文件不兼容停止，已下载 CI 同版 3.41.8 且 enforce-lockfile 通过；没有升级项目依赖。新更新代码首轮分析发现三处缺少花括号，正在修正后复验。本机 WSL WSAETIMEDOUT，未重启用户环境；Linux 验证交由 CI。官方依赖闭包、完整第三方版权审计、Debian unstable sbuild、商店认证/签名安装尚未完成。

### 防复发措施与后续

CI 执行 MakeAppx、Debian 实际包检查、Lintian 和隔离安装/移除；保留 Development 通道隔离、节流、关闭持久化与失败测试。不得将“能打包”表述成已进入微软商店或 Debian 官方源。详见 [发行准备](distribution-preparation.md)。

### 交付记录

首轮 [提交 ae57379](https://github.com/LumiaBlack51/twodrive/commit/ae57379a9fd2f755048ef9c248db9d4bf8feff9d)、[PR #21](https://github.com/LumiaBlack51/twodrive/pull/21)。首轮 [Debian CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38687822125) 编译与测试通过，但 dh_installsystemduser 不支持 --no-start 而中止；改用 --no-enable，此 user helper 本身不启动服务。Flutter 三处 lint 修正后 analyze 通过；五项更新检查测试通过，两项旧 widget 测试原先假定设置只有一个 SwitchListTile，改为按“登录后自动启动”定位后完整 29 项通过；补充更新提示/MSIX 自启动边界后最终为 30 项。本轮收尾已核实 [f22a121 全套桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38690583401) 与 [Debian CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38690583450) 全部成功，Windows MSIX 的 33 个保留 bundle 文件与同次便携目录逐一哈希一致，两份 Debian 上传清单完整。详见[发行准备验证](distribution-preparation.md#验证记录)与[产物记录](distribution-validation-2026-10-15.json)。

## TD-20261015-03：桌面状态轮询堆积与连接身份展示不足

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证本地及 Linux/Windows 原生 CI 的相关机制；真实云端本轮未复验。
- 影响版本与环境：Development 0.1.3 桌面客户端；本轮基线 dev `52d7f42`，修复交付于 Development 0.1.4。
- 关联历史故障：TD-20261014-05（在线管理连接）、TD-20261013-02（操作目标绑定）、TD-20261014-03（Google 冷索引时间）。不是云端删除待办 TD-20261015-01 的同一队列。

### 症状与影响

上轮升级验证期间出现 request queue is full；用户本轮更正为随后已正常。不能继续把“始终无法恢复”写作事实。保存后登录入口不明确、当前连接缺少明显选择标记、托盘和连接页显示配置名而非实际登录账号。没有本轮数据丢失证据。

### 触发条件与复现

历史操作是绕过 Flutter busy 状态直接向 desktop IPC 请求长时间挂载，同时桌面每三秒轮询 status；原截图完整时序未重新还原。可控延迟替身中，旧 refreshStatus 连续调用 100 次确实发出 100 个请求；引擎切换活动连接后，旧桌面 selected 保持旧值。受控 loopback WebDAV 阻塞连接时，32 项队列可稳定填满，读入取消仍成功。

### 根因与证据

状态定时器没有在途请求合并，status 与长操作共用串行工作队列，满时拒绝。成功状态不清理轮询错误，状态变化未同步活动 profile，当前目录没有定期刷新。账号展示只使用配置字段，Google 已探测的 account_name 未传到桌面。队列已接收但尚未执行的普通任务没有统一检查取消标志，可在被取消后仍执行。后者用隔离变体恢复旧 worker 检查方式时稳定失败：被取消 save_profile 仍成功；恢复修复后通过。这些机制有独立证据，不将历史截图直接归因于某一真实云端请求失败。

### 解决办法与恢复操作

合并在途 status 和整个轮询周期，前端限制未完成请求数量；保留有界引擎队列和绕过队列的取消，执行前再次检查取消。新版 UI 先订阅后，引擎推送无秘密的操作开始/完成与公共状态快照；旧前端默认不接收事件，新前端遇旧引擎回退轮询。外部操作也触发忙碌/取消入口；完成后、窗口聚焦时刷新，当前目录每 15 秒浅层检查。轮询暂时错误自动消除，明确操作错误保留。过期目录结果不能覆盖新连接。

保存后每条配置都有“登录并使用／切换使用”，真正活动项显示“当前使用”。适配器把真实账号展示字段写入 kind/revision 绑定的 session，并仅投影该字段到公共 profiles/status。Google 显示已探测邮箱/名称，OneDrive 使用 /me 的显示名称/邮箱/UPN（既有 User.Read 不变；展示探测失败不阻断可用连接），WebDAV 为用户名/匿名访问，设备共享为连接名称。管理标题、连接页、托盘面板和 Linux/Windows 原生托盘提示同步更新；配置名仍单独显示，离线只标上次登录。账号文字不参与稳定身份 binding。

本地恢复操作：本轮未操作生产账号、数据库、服务或挂载。测试只使用临时状态和自有 loopback 目录。

### 验证结果与边界

旧 Flutter 两项回归分别以 100 != 1、old != new 失败；新实现通过。队列旧 worker 变体退出 1（取消后的等待任务仍成功），新受控 IPC 检查通过，覆盖队列满/取消通道/跳过任务/完成快照/用户名/恢复。前端另覆盖轮询错误恢复且保留操作错误、外部操作同步、整轮刷新合并、销毁后不继续刷新、明确登录/切换/当前标记及原生托盘参数；Rust 覆盖 /me 请求与名称回退、session kind/revision 和秘密不公开。

根工作区默认 Rust、独立实验完整 mount 套件、两工作区严格 Clippy/格式、Flutter analyze 与 24 项测试、Linux 原生构建、25 Nautilus/13 scripts 单元测试通过。实际隔离 WebDAV IPC/FUSE 完成 2 MiB 字节往返、读写、暂停与恢复、缓存/备份/冲突保护；本机签名 UDP/QUIC 发现、邀请、读写、重连和切回原连接通过。

不将上述本机协议/FUSE 或模拟 Graph 当作真实 OneDrive/Google 端到端；本轮未访问真实账号、重新浏览器登录或验证原生产截图时序。根工作区默认忽略的 7 项 FUSE 与原生更新进程测试本轮未显式运行，独立桌面 FUSE 冒烟另已运行。原生 Linux/Windows 桌面 CI 均通过（Rust 65/59 项、Flutter 各 24 项、两平台队列/取消/订阅兼容回归、Windows 托盘 Shell 通知模拟）；用户实体桌面交互未验收。

### 防复发措施与后续

保留 state_refresh_test、connection_identity_test、session 投影/Graph 账号测试和 scripts/test-desktop-queue.py；后者加入 Linux/Windows 桌面 CI。保留账号/配置版本、未上传缓存和原有绑定约束。状态/目录刷新是轮询与操作通知，不承诺云端实时推送或任意网络 I/O 立即取消；第三方接入规范更新为 1.1。

### 交付记录

[PR #19](https://github.com/LumiaBlack51/twodrive/pull/19) 已合入 dev，合并提交 `41929a9e36aaaa7b973df4ab8dcaaf59f2c057a8`。最终源提交/标签 `dev-v0.1.4` 指向 `791a006187856bb9d5eb81f3eca3990382f855a7`；合并时两树逐字节等价（Git tree 相同）。[桌面 Linux/Windows CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38675643955)、[必需原生检查](https://github.com/LumiaBlack51/twodrive/actions/runs/38675644048)、[独立 WebDAV/QUIC](https://github.com/LumiaBlack51/twodrive/actions/runs/38675644035) 和 [Windows 综合预览](https://github.com/LumiaBlack51/twodrive/actions/runs/38675643947) 均成功。

[Development 0.1.4 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.4) 已于 2026-10-15 11:38:42 +08:00 公开为预览版，稳定 Latest 核实仍为 0.2.14-1。6 项附件经过保护脚本的公开前后校验：集合、上传状态、大小、SHA-256 与实际远端标签/源 SHA 均匹配。Linux tar 是 CI 原件；Windows ZIP 的 35 个文件与 CI 原件逐字节相同、CRC 通过，三个 EXE 为 PE x64。详见[交付记录](development-0.1.4.md)和 Release verification.json；本轮未安装用户本机程序。

Linux 包 SHA-256：`02384eefc6a1dc5430ee7aa2d8b4b44d63d78015c12f39e6717bdb81811431fe`；Windows ZIP：`50baf3ad6ed5991ad1dc20cb598d9e20a38583ea3fb9b22ddea2193ac2b680a5`。

## TD-20261015-02：Release 校验失败后仍执行公开操作

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已恢复并验证保护脚本与两次正式发布；保护代码随交付记录同步。
- 影响版本与环境：本轮 0.2.14-1 的临时发布命令，不是客户端代码缺陷。
- 关联历史故障：TD-20261015-01 是客户端删除诊断/调度；本条是交付脚本顺序错误，根因不同。

### 症状与影响

草稿的 5 项附件尚未全部上传，版本被提前公开。该阶段未完成附件集校验；是否有人在该窗口下载未核实。未包含令牌、私人数据库或用户文件。

### 触发条件与复现

Python 断言当前附件数量为 5 失败时，临时 shell 的下一行仍调用 gh release edit --draft=false；实际输出同时出现 AssertionError 和公开成功，shell 最终退出 0。

### 根因与证据

校验与公开操作没有退出码/控制流依赖，发布又与未结束的上传命令重叠。错误发生在临时编排命令，不归因于 Graph 403 或 GitHub 附件内容。首次以本地 remote-tracking 名 origin/main 创建草稿亦收到 422，未创建版本，随后改用已核实完整提交 SHA；此项与提前公开分开说明。

### 解决办法与恢复操作

立即退回草稿，确认 Latest 仍是 0.2.13-1；等待原上传完成，核对全部 5 项附件 state/size/digest 与本地一致后重新发布。最终 tag 指向 `6f64df2f9be88a92d5fab6193a06776d0ba98951`，未移动已有标签。

新增 scripts/verify-release-assets.py：精确校验 tag（含实际远端标签解引用）、完整源 SHA、完整附件集、上传状态、大小和 SHA-256；可只读检查，只有全部通过且草稿/通道一致时才从同一 Python 控制流调用公开操作。校验失败不会到达发布调用。Linux 原生 CI 增加保护脚本单元测试。

本地恢复：无客户端安装/服务/数据库操作。只恢复 Release 的草稿状态并完成正确交付。

### 验证结果与边界

首轮 4 项、补充后 6 项保护回归通过，覆盖未完成/额外附件、状态/大小/摘要/目标错配、通道错配、只读不发布及合格草稿仅发布一次。真实草稿经只读校验成功，再用新脚本完整校验并公开成功；公开后再次核实 5 项摘要、tag 与 Latest。不将附件元数据校验说成已完成用户端下载执行。

### 防复发措施与后续

先推送核实的源标签，上传结束后才运行该脚本；默认只读，公开必须显式传 --publish stable/preview。不得使用未受前一步退出码约束的后续 shell 行进行发布。

### 交付记录

[0.2.14-1 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.14-1) 已于 2026-10-15 10:03:50 +08:00 重新公开并设为 Latest；Development 0.1.3 亦已通过同一保护脚本公开，Latest 保持稳定版；6 项单元回归和实际 tag/digest 校验通过，保护代码见 [main PR #17](https://github.com/LumiaBlack51/twodrive/pull/17)。

## TD-20261015-01：持续 403 删除待办的 main/dev 存续与修改必要性评估

- 日期：2026-10-15（Asia/Shanghai）
- 状态：已验证安全诊断/持久退避（模拟/隔离 FUSE/原生 CI），main/dev 均已交付；原对象拒绝根因未确认。
- 影响版本与环境：本轮 fetch 核实 origin/main `e62942798cc6ab4b221a31d6d766b16dd4be8edf`、origin/dev `ecb05392324275f3e0c4317f6e5937d3a4b52ed7`；代码与公开脱敏历史证据评估。
- 关联历史故障：main 的 TD-20261014-16（旧任务持续 403）、TD-20261014-12（本地 unlink 限制）及 TD-20261014-15（探针等待条件）。本轮是对既有问题的分支复核，尚无新的真实账号复发证据。

### 症状与影响

附件报告旧删除任务被拒绝，普通 HTTP 错误缺少安全诊断，恢复扫描周期性重排。附件作为待核验材料，其中真实账号检查与代码修改建议不作为执行指令。

### 触发条件与复现

隔离副本中，模拟远端持续拒绝后，待办经三轮重新打开数据库/恢复扫描均再次排队，拒绝解除后清理；loopback HTTP 验证 Graph 403 本身只发一次请求。没有重放生产删除或读取私人数据库/凭据。

### 根因与证据

旧对象具体权限、共享来源、历史身份绑定或服务端策略未确认；不能从相同 HTTP 状态推断根因。两分支普通 HTTP helper 完全相同，错误只保存 status/retry_after；恢复扫描均无逐项到期/阻塞分类。合成 token 刷新 403 在实际 GraphBackend::delete 中亦传播为无来源 HTTP 403，不能仅凭历史聚合文字区分请求来源。保留待办是正确行为，诊断不足与持续拒绝仍被周期重排是尚未改进的机制。

dev 桌面另有账号/Drive binding 及部分 remoteItem 过滤；直接文件页删除不加入 FUSE 待办，只有可写挂载继承周期恢复。main 已修复本地 Dirty/Uploading unlink 限制，dev 尚未同步；这与本次服务端拒绝不同。完整证据与建议见[评估报告](pending-delete-403-assessment-2026-10-15.md)。

### 解决办法与恢复操作

最初仅评估与记录；后续经用户授权实施，见下方补记。本地恢复/安装操作：无。

### 验证结果与边界

已 fetch 核实两分支提交；各自独立临时源码副本追加 5 项评估探针，每分支 backend 4 项、fs 1 项全部通过，确认 403 诊断信息丢弃、token 刷新来源歧义、持续拒绝后重启保留与扫描重排，并检查 404、长 429、503 行为。仅 loopback HTTP/合成凭据/临时 SQLite，不是真实 OneDrive 或真实 FUSE；未运行全工作区、Windows 原生测试。探针只存在于临时副本，不是已提交的长期回归；无修复版本，不声称旧失败/新通过。没有核实原生产任务今日状态。

### 防复发措施与后续

建议优先安全诊断，随后持久化逐任务退避与可重新评估的受阻状态；保留任务、缓存和同路径屏障，不把 403 当成功。账号/Drive/Item 绑定及 remoteItem 删除语义变更待对象证据。正式修改时补入长期回归。现有证据支持维护改进，不支持保证改代码即可解除旧对象拒绝，也没有必须立即紧急发布的证据。

### 交付记录

最初为本地[评估报告](pending-delete-403-assessment-2026-10-15.md)与记录；保留评估前已有的 TD-20261014-12 未提交条目。评估阶段确认修复的机制：无；后续实施见补记。尚未验证范围：原对象根因、生产当前状态及后续改进能否解除其拒绝。

### 2026-10-15 实施进度

用户已授权按建议修复并同步 main/dev 与 Release。已实现有界/白名单 HTTP 诊断（操作来源、状态、细分码、UUID 请求关联、日期/内容类别与 claims 布尔值），不输出远端正文、ID、URL；token 刷新失败不再被 DELETE 请求构建循环额外重复。持久化 attempt_count/next_retry_unix/last_status/failure_kind：权限/认证/其他拒绝从 300 秒翻倍至 21600 秒，临时错误从 60 秒翻倍至 3600 秒，更长 Retry-After 优先；仅到期任务入队，执行时再次核验。保留任务、缓存及同路径屏障，新增 CLI 状态与 retry-deletes（仅重置到期时间，不删除任务）。

两项正式回归在隔离副本恢复旧 HTTP/auth/delete 和旧恢复实现后分别 exit 101（只得 HTTP 403 / 未记录失败与退避），新实现通过；这不是将整个依赖/数据库回退至旧版。工作区默认 Rust 170 项通过，随后新增的旧队列迁移回归与退避回归 2 项通过；25 Nautilus、4 Settings、严格 Clippy 与格式通过。7 项默认忽略的真实 FUSE 回归显式执行通过，隔离 MockBackend daemon 冒烟通过；FUSE 的远端为 mock，未访问真实 OneDrive 或检查原旧对象。既有缓存恢复测试改为明确提前重试后再断言成功，保留缓存/不重复上传断言。

dev 已同步 main 的既有本地 unlink 修复和本次共享引擎改动；保留原 dev 的账号/Drive binding、create-only 条件上传、提供者隔离与全部故障历史。Development 升至 0.1.3，Linux 状态页显示删除待确认/被拒绝/退避计数，拒绝进入同步错误状态，提供 retry_deletes IPC 与“重新尝试”按钮；只让任务到期，不发送额外 DELETE 或改目标。桌面文件页直接删除仍同步返回错误，Windows 没有 FUSE 队列。

本机 dev 根工作区 173 项默认 Rust 通过，独立工作区原有 64 项通过，新增删除状态/手动重试回归独立通过；两工作区严格 Clippy 与格式通过。Flutter analyze 通过；后续 Flutter 16 项及最终 Linux/Windows 原生 CI 已通过，正式发布见下方。稳定 deb 0.2.14-1 已构建，包内 CLI/daemon 实际版本与构建输出逐字节一致，包内 MockBackend daemon 冒烟通过。目标稳定版本 0.2.14-1、Development 0.1.3；最终交付、CI 与发布链接见下方补记。原对象具体拒绝根因仍未确认。本地恢复/安装：未执行。

### 2026-10-15 Development 正式交付补记（TD-20261015-01）

[PR #16](https://github.com/LumiaBlack51/twodrive/pull/16) 已合入 dev，合并提交 `bffbe0e7866df8b29b32050d2b7035e135ffb96f`。最终 [Linux/Windows 桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38668500026) 成功：Rust 65/59 项、Flutter 各 16 项、原生 IPC、严格 Clippy/分析/格式、GUI 构建及 Windows 托盘烟测通过；[root 原生](https://github.com/LumiaBlack51/twodrive/actions/runs/38668500005)、[独立 DAV/QUIC](https://github.com/LumiaBlack51/twodrive/actions/runs/38668499972) 均成功。[Full/Lite 额外 Linux/Windows CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38668499994) 也已成功。

Linux 直接保留 CI tar，Windows ZIP 的 35 个文件均与 CI 原件逐字节相同，三个 EXE 的 PE x64 和 ZIP CRC 已核对；Linux 包内工具实测 0.1.3，包内引擎/CLI 再次完成 loopback IPC/WebDAV 2 MiB 与签名 UDP/邀请/QUIC 回归。GUI 重试绑定状态中的活动 profile，另有选择其他配置时仍针对当前显示挂载的回归。未执行真实 OneDrive 或原对象权限检查，也没有本地安装。

Development 0.1.3 的源标签为上述 dev 合并提交，CI 代码头 `8f4aa72e1e1b4d83c223fc8839a2c2dae40c8701`；两源码树完全相同。[Development 0.1.3 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.3) 已于 2026-10-15 10:19:03 +08:00 公开为预览版，6 项远端附件 state/size/digest 与本地全部一致；保护脚本核对了实际远端注释标签解引用。Latest 仍是稳定 0.2.14-1。Linux tar SHA-256：`1e2c62620b9f82bd94d87449919fd35a98a3af3e0c4769a9460ac7496296ed50`；Windows ZIP：`b46e31a63476f19db300e2e027ff3669346e4f2dc393fa995617e9ac9d0bba61`。

[稳定版 PR #15](https://github.com/LumiaBlack51/twodrive/pull/15) 合并到 main 的 `6f64df2f9be88a92d5fab6193a06776d0ba98951`，对应 [0.2.14-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.14-1)，5 项附件也已核验；deb SHA-256：`a7000c3c7b8ae6913fd8bc145b5257eb0ff78729190b584641d12b9dd9362c5d`。main 的 [core/backend/peer](https://github.com/LumiaBlack51/twodrive/actions/runs/38667682109)、[独立 DAV/QUIC](https://github.com/LumiaBlack51/twodrive/actions/runs/38667681924)、[Full/Lite](https://github.com/LumiaBlack51/twodrive/actions/runs/38667682052) 最终六项原生任务成功。

确认修复的是安全诊断和任务退避机制，原对象具体 403 根因与生产当前状态仍未验证。本轮无本地恢复/安装。后续交付记录和发布保护脚本不改变已发布客户端程序源码或资产。完整 Development 验证见 [0.1.3 记录](development-0.1.3.md)。


## TD-20261014-16：升级前既有云端删除待办持续返回 HTTP 403

- 日期：2026-10-14
- 状态：调查中 / 具体服务端拒绝原因未确认；待办保留。
- 影响版本与环境：旧 0.2.10 数据库已有 1 项删除待办，升级 0.2.13 后仍未完成；真实 OneDrive。
- 关联历史故障：TD-20261014-12 已说明不能把同期云端删除错误归为关闭 Dirty 文件拒绝；本条单独记录服务端拒绝。

### 症状与影响

本地上线验证通过，新测试对象成功清理，但升级前已有的一项云端删除任务仍失败。该对象的远端删除未完成；本条不包含对象 ID、私人路径或响应正文。

### 触发条件与复现

正常后台恢复重试既有队列。以停止服务时的私有 SQLite 备份与当前数据库只读比较，剩余 pending_deletes ID 集合没有新增，剩余 1 项属于升级前集合。

### 根因与证据

升级后服务日志脱敏统计：queued delete retry error 17 次、HTTP 403 17 次、旧 cannot delete ... changing 拒绝 0 次。已确认服务端拒绝该删除请求，具体对象权限、共享状态或其他策略未核实，不能凭 403 断言某一原因。相同登录的自有临时目录/文件创建、上传、读取、删除已成功，不据此认为所有对象均有删除权限。

### 解决办法与恢复操作

保留持久化待办，不直接改数据库清除、改变权限或绕过服务端。除已授权的正常软件升级和自有探针清理之外，没有对该旧对象执行额外恢复操作。

### 验证结果与边界

已验证剩余任务为原有任务，没有本轮新增未完成删除；只读数据库比较和脱敏日志不能定位具体远端拒绝策略，未对该对象权限或共享来源另作检查。不是证明 0.2.13 新增的删除竞态。

### 防复发措施与后续

后续若处理此项，应针对具体对象的可删除权限/云端响应分类调查，继续脱敏，并区分本地 unlink 成功与云端完成。不得因本次程序修复通过而宣布此待办完成。

### 交付记录

观察发生于 [0.2.13-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.13-1) 本地上线之后；本条仅记录现象和边界，没有针对 403 的程序修复或发布。

## TD-20261014-15：真实验证探针在 FUSE release 完成前误判同步已结束

- 日期：2026-10-14
- 状态：已验证旧探针条件失败/修正后真实 OneDrive 流程通过；程序源码未因本条改变。
- 影响版本与环境：本轮忽略目录下的临时真实 OneDrive/FUSE 验证探针，使用 0.2.13 源码及发布锁定依赖。
- 关联历史故障：TD-20261014-12 区分本地关闭与云端完成；本条为新探针观察条件错误，不是同一产品根因。

### 症状与影响

探针首个 8 MiB+123 字节文件在远端查询时尚不存在，误认为保存验证失败。两轮均清理自己创建的随机云端目录及私有凭据副本；未改生产程序、数据库或用户已有文件。

### 触发条件与复现

std::fs::write 返回并关闭描述符后，FUSE release 仍可能异步等待处理，数据库暂为 Writing。探针只将 Dirty/Uploading/Conflict 计为未完成，在 Writing 时提前结束等待；远端立即查询得到 None。

### 根因与证据

新探针忽略 Writing，而生产恢复代码有意不上传仍在写入的文件。以发布 Cargo.lock 的依赖版本再次运行旧观察条件，得到受控 missing uploaded object 错误。根工作区和程序源码未改变。

### 解决办法与恢复操作

探针等待条件同时包含 Writing，等待 release 发布并完成云端上传后再读回；保留对字节、删除、同名重建和自有云端目录清理的断言。仅修改临时验证辅助程序，无生产恢复操作。

### 验证结果与边界

旧条件失败已确认；等待 Writing/release 后，真实 OneDrive + 隔离 FUSE 完成 8 MiB+123 字节上传/下载比对、20 次 Dirty/Uploading 锁文件关闭删除（本机最大 8ms）、16 MiB+123 字节已认领上传 unlink 后同名重建为 256 KiB+31 字节并读回比对、所有测试文件与随机云端父目录删除确认。[脱敏汇总](unlink-live-validation-2026-10-14.json)。不把替身或成功的本地 write 计为真实云端完成，也不据此断言真实云端存在文件丢失。

### 防复发措施与后续

端到端探针必须等待本地句柄关闭发布和云端确认两个阶段；发布依赖与探针依赖锁定版本一致。

### 交付记录

辅助程序与私有运行目录位于忽略路径 target；凭据副本和临时云端目录已清理，仅把上述脱敏汇总纳入发布验证。记录提交待补充。

## TD-20261014-14：稳定组件升级后独立工作区锁文件仍引用旧版本

- 日期：2026-10-14
- 状态：已验证独立 locked 解析及最终 Linux/Windows 原生 CI。
- 影响版本与环境：0.2.13 发布准备提交 eba3604，独立 experiments/twodrive-dev 工作区，Linux/Windows CI。
- 关联历史故障：TD-20261014-13 为稳定发布基线遗漏，本条为另一套 Cargo.lock 未同步版本；不归因于云端删除机制。

### 症状与影响

根工作区构建/测试及 deb 验证通过，独立 WebDAV CI 在 cargo clippy --locked 的依赖解析阶段失败，未进入测试。阻止发布；用户现用软件和云端数据未改变。

### 触发条件与复现

共享 core/backend/fs 从 0.2.11 升至 0.2.13，独立 Cargo.lock 仍锁定三个本地 path package 的 0.2.11。Linux 和 Windows 原生任务均失败。

### 根因与证据

[失败 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563070060) 的两个任务报告 cannot update the lock file .../experiments/twodrive-dev/Cargo.lock because --locked was passed。根锁已同步，独立工作区使用另一份锁。虽 Windows 不启用 Linux mount，Cargo 仍需要一致的跨目标锁文件。

### 解决办法与恢复操作

离线重新解析独立锁文件，限定 diff 为三个本地共享 package 的版本号，不升级第三方依赖、不修改实验功能。没有用户恢复操作。

### 验证结果与边界

原失败证据为两个真实 CI job；新锁离线 locked metadata 解析通过，diff 只有三个 path package 的版本。最终 [Linux/Windows 独立工作区 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563697140) 均通过；[peer 原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563697042) 及 [Full/Lite 原生构建和 IPC CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563697093) 也通过。不是运行程序测试失败，不能把修复锁文件说成新云端功能验证。

### 防复发措施与后续

升级共享 path package 时搜索所有独立 Cargo.lock 并检查锁定解析，保留原生工作区 CI。

### 交付记录

修复提交：[53e0133](https://github.com/LumiaBlack51/twodrive/commit/53e01335f5c92e5e1c978e0fa8d31cc7d5d196a3)；随 PR #13 合入 main，最终 CI 见上文。[Release 0.2.13-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.13-1) 已发布；本地升级结果见 TD-20261014-12 后续补记。

## TD-20261014-13：main 发布基线落后于上一稳定包的 Nautilus 下载反馈

- 日期：2026-10-14
- 状态：已验证两份已发布源码原字节恢复、25 项 Nautilus 测试及候选安装包；合入 main，正式发布待完成。
- 影响版本与环境：main 基线 258438e（稳定组件 0.2.11），上一稳定 Release v0.2.12-1；尚未发布本轮包。
- 关联历史故障：上一稳定包包含 a749e43 的固定下载反馈修复；与 TD-20261014-12 的删除限制是不同机制。

### 症状与影响

从 main 直接组装下一稳定包会遗漏已在 0.2.12-1 发布的 Nautilus 等待下载反馈、刷新后反馈保留和失败重试逻辑。发布准备时发现；没有交付这一回退包，也没有改变本地客户端。

### 触发条件与复现

比较 main 与已存在的 v0.2.12-1 标签：稳定 Rust 组件只有版本号差异；另外两份 Nautilus 程序/测试源码存在实际差异，上一稳定版为 25 项测试，main 为 19 项。

### 根因与证据

上一稳定包从包含稳定修复的 dev 集成提交构建，main 尚未同步这些已发布文件。已核对 Release 的源码标签 93d1b44 和文件 diff，不根据分支名推断稳定代码相同。v0.2.12-1 中稳定五个 Rust crate 与 main 的差异仅 Cargo.toml 版本号，实验组件不属于本次恢复范围。

### 解决办法与恢复操作

把 v0.2.12-1 的 packaging/nautilus/twodrive_nautilus.py 和 test_twodrive_nautilus.py 原字节恢复到 main 修复分支；只恢复这两份已发布稳定源码，不合入 dev 实验功能。稳定五个 crate 及 deb 版本更新为 0.2.13 / 0.2.13-1，避免覆盖已存在的 0.2.12 Release。尚未执行本地安装；用户已授权 CI 完成后合入 main、发布和升级。

### 验证结果与边界

两份源码与 v0.2.12-1 原字节相等，25 项 Nautilus 和 4 项 Settings 测试通过；候选 deb 中的程序/辅助程序/扩展与当前构建源码逐字节一致，包内 CLI/daemon 为 0.2.13，包内程序驱动的 7 项真实 FUSE 和隔离 MockBackend daemon 冒烟通过。本条只防止发布回退，不重新宣称旧下载反馈全部真机路径得到验证。

### 防复发措施与后续

稳定发布前逐组件比较上一稳定标签与 main，核实版本递增和实际源码差异；不能把开发分支整个并入作为恢复稳定文件的替代。

### 交付记录

[上一稳定 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.12-1)。本轮准备提交：[eba3604](https://github.com/LumiaBlack51/twodrive/commit/eba360484cd852834ac9fe797f17497878b032fe)，随 PR #13 合入 main；[正式 Release 0.2.13-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.13-1) 已发布；本地已安装 0.2.13，详见 TD-20261014-12 的上线记录。

## TD-20261014-12：关闭后的待上传文件不能删除，导致 Git 锁文件残留

- 日期：2026-10-14
- 状态：已验证本机删除/并发/模拟重启机制及隔离真实 FUSE；真实 OneDrive 本轮隔离文件删除/重建/读回已通过，见后续发布补记。最初仅调查阶段的结论和恢复操作保留如下。
- 影响版本与环境：本机运行 `twodrive-daemon 0.2.10`，Linux、真实 OneDrive FUSE 挂载。当前源码提交 `ecb05392324275f3e0c4317f6e5937d3a4b52ed7`（daemon 0.2.12）仍包含对应删除限制；未运行该源码构建作版本对照。
- 关联历史故障：[2026-07-24 可靠性修复](reliability-fix-2026-07-24.md)仅覆盖打开句柄的 dirty 文件删除；[2026-09-03 local-first 设计](local-first-sync-2026-09-03.md)约定 unlink 先更新本地命名空间，再持久排队云端删除。本次是关闭句柄后仍受同步状态限制的遗漏。

### 症状与影响

在挂载目录初始化、提交及推送 Git 仓库时，删除 `HEAD.lock`、`maintenance.lock`、`config.lock`、`index.lock` 先后返回 `Input/output error`。残留 `config.lock` 导致后续 upstream 配置写入报 `File exists`。GitHub 推送实际完成，38 个项目文件和远端提交核对一致，未发现项目文件损坏。迁移后删除旧 `.git` 的一个内部引用文件也暂时遭遇同一错误。

### 触发条件与复现

Git 创建、写入并关闭短生命周期文件，随后在 TwoDrive 后台同步完成前 unlink。真实操作多次出现删除错误，服务日志在 19:22:56、19:23:18、19:25:18、19:26:32 等时刻输出同一拒绝信息。迁移清理时有一个引用文件连续重试仍失败，稍后只读查询显示该文件已变为 pinned，随后正常删除成功。没有新建独立探针或回归测试；未在每次原始错误瞬间抓取数据库状态。

### 根因与证据

`crates/twodrive-fs/src/filesystem/writes.rs` 的关闭提交路径将持久缓存标为 Dirty 并排队上传；`filesystem/fuse.rs` 的 release 路径随后移除写句柄。unlink 仅在 `unlink_record_with_open_handles` 找到打开句柄时绕过普通删除分支；没有打开句柄时进入 `delete_record`。`filesystem/mutations.rs` 明确拒绝 Writing、Dirty、Hydrating、Uploading 四种状态，错误为 `cannot delete a file while it is changing`；`filesystem/fuse.rs` 将该错误统一返回 `libc::EIO`。

本机日志实际出现：

```text
twodrive delete file error: cannot delete a file while it is changing
```

这使已完成本地关闭的短生命周期文件仍需等待云端状态变化才能删除，与上述 local-first unlink 约定不一致。Git 的锁文件生命周期暴露该实现缺口。源码拒绝分支、实际服务日志和稍后成功删除的状态变化相互吻合；不据此认定所有 FUSE 文件系统都有同样限制，也不将同期其他云端删除错误或 Git 首次 HTTPS 连接等待归为此根因。

### 解决办法与恢复操作

程序、服务和数据库未修改，未重启或升级 TwoDrive。按用户要求仅定位原因。

本地恢复单独执行：将项目文件及完整 Git 元数据复制到普通本地 ext4 工作目录，核对 38 个项目文件的 SHA-256、提交及远端跟踪配置，运行 `git fsck --full`；确认新仓库完整后删除原挂载目录的 `.git`，保留项目文件作备份。删除清理先遭遇上述限制，等待后台状态正常变化后完成。未绕过同步状态检查或直接修改同步数据库。

### 验证结果与边界

新目录 `git fsck --full` 通过，工作区干净，main 跟踪 origin/main；提交与迁移前及 GitHub 一致。原目录 `.git` 已完全移除，Git 仓库探测失败符合预期；原目录与新目录的 38 个项目文件校验值一致。读取生产 SQLite 使用 `mode=ro`。这些结果验证迁移和备份完整性，不代表 TwoDrive 缺陷已修复。未新增或运行 TwoDrive 回归测试，未验证其他系统、提供者、发布版本和首次 HTTPS 等待原因。

### 防复发措施与后续

如后续授权修复，应覆盖已关闭的 Dirty/Uploading 文件 unlink、后台上传与删除并发、删除后的重启恢复，以及 Git 锁文件快速创建/关闭/删除的真实 FUSE 操作；保留待上传内容及远端删除的正确排序，不能仅移除保护条件。当前继续使用普通本地工作目录，将挂载目录保留为项目文件备份。

### 交付记录

仅本地调查记录，未提交或发布 TwoDrive 修复。迁移的项目原有提交已核实为 `0f60e17259f8f77f4bf45f96552e1c1514510b6e`；本记录不收录用户私人路径、令牌或完整文件清单。


### 2026-10-14 主分支修复补记

用户在分析后授权修复；基线为 main `258438eb230409cc30d600c722f434f2e08bb3bc`（daemon/core/fs 0.2.11），在独立 `codex/pending-unlink-main` 工作树开发，不将 dev 后续改动一并合入。本次修复对应原删除限制的遗漏；不是另一场复发。上文“未修改/未测试”描述的是最初调查阶段，本节更新当前状态。

**机制与改动**：unlink 在 SQLite Immediate 事务中重新读取当前记录、持久化删除意图并移除本地命名空间，随后返回成功。后台通知失败也不撤销已提交的 unlink。未被上传任务认领的本地文件直接取消上传；已绑定云端身份的待上传修改可以直接排队删除原对象。正在进行的上传与删除使用相同的本地身份锁，上传持有缓存共享锁，清理先取得缓存独占锁；缓存和删除记录保留到任务和打开的缓存句柄结束。原上传请求的路径、目标和缓存路径持久化，迟到的成功响应绑定到旧文件的删除记录，不能发布到同名新文件。

上传已开始但新对象的云端身份未确认时，重启/失败恢复保留缓存，完成或续传该已认领的请求以获得云端 ID，然后删除；这条保守路径可能仍会上传被删除的内容，不把所有删除都强制先上传。云端已接受、进程尚未记录响应的窗口由模拟丢失响应回归覆盖。旧路径、当前本地路径和响应路径均作为后台上传屏障，允许前台立即创建同名文件，但新文件云端上传等待旧对象清理完成。冲突复制已开始的情况下，原对象和迟到的冲突对象分别排队清理，不让它们重新进入本地目录。delta 的删除回声不再提前丢弃尚有上传/缓存工作未结束的删除记录。

迁移为 files 的 3 个可空上传声明字段及 pending_deletes 的 6 个可空字段；旧删除队列可兼容读入，重复初始化保留新队列状态。恢复/回滚需要使用理解这些记录的程序，或恢复升级前的停止服务数据库备份；不能把旧程序理解新增队列语义视作已验证。原有防止旧缓存覆盖新内容的代次检查保留。

**验证进度**：新增关闭 Dirty 文件、上传中 unlink/同名重建、丢失上传响应前后重启、云端删除失败重试、上传冲突及冲突副本上传中删除、缓存锁和 Hydrating 完成取消回归；数据库覆盖删除时读取最新云端绑定、重复迁移、改名后的迟到响应及旧路径屏障。临时恢复 main 原始 filesystem/mutations.rs，两项关闭 Dirty 文件回归均因 `cannot delete a file while it is changing` 失败；恢复修复后通过。这是恢复旧删除实现的对照，不冒称所有代码都回退到旧版。开发中新增替身曾因嵌套持有自身 Mutex 自锁，已改为先复制观测结果再调用后端；上传前误刷新旧任务快照被既有 stale_empty_upload 回归识别并撤回。

已在真实隔离 FUSE 挂载、模拟上传/删除离线后端执行 Git init/config/add/commit/status，HEAD.lock、config.lock、index.lock、maintenance.lock 各 5 次创建/关闭/删除/重建通过。该测试不访问真实 OneDrive。同一挂载回归临时恢复 main 原删除实现后失败：日志再次出现拒绝 Changing 的删除分支，并因残留锁在 create_new 时返回 File exists；恢复修复后通过。最终 `cargo build --workspace --locked`、`cargo fmt --all -- --check`、严格 Clippy 和工作区 162 项默认 Rust 测试通过（8 项默认忽略）；7 项 FUSE 挂载测试全部显式通过，包括新 Git 锁回归以及解压、chmod/执行、下载取消、延迟读取和大目录响应。Nautilus 19 项与 Settings 4 项 Python 测试通过；独立 MockBackend daemon 冒烟的 hydration/write/upload/move/delete/pin/release 全部通过。另一个默认忽略的 peer 更新进程测试本轮未执行。构建和测试日志保存在本工作树忽略目录 target/unlink-validation，不含真实账号数据。

**验证边界和本地恢复**：本轮不操作现用服务、数据库或真实云端文件；最初的 Git 仓库迁移仍单独列于上文。未执行真实 Graph 上传/删除及断网/崩溃探针、Windows 原生运行、多进程完整竞争、外部客户端同名云端并发编辑、未缓存延迟读取句柄的 unlink 语义和正在云端创建目录时的 rmdir。测试中的重启是重开隔离数据库/重新运行恢复函数，不是真实生产进程崩溃。不能据此承诺所有 EIO 均已修复。

**发布前真实验证补记**：用户随后授权合入 main、Release 和本地升级。使用同一 0.2.13 源码及发布 Cargo.lock 的依赖，在私有凭据/配置/数据库副本和隔离 FUSE 中运行真实 Graph：8 MiB+123 字节上传下载 SHA-256 一致，20 次 Dirty/Uploading 锁文件关闭删除及重建通过（本机最大 unlink 8ms），16 MiB+123 字节已认领上传删除后同名重建为 256 KiB+31 字节并读回一致；Graph 确认所有自有测试文件和随机父目录已删除，隔离队列归零。结果见[脱敏报告](unlink-live-validation-2026-10-14.json)。旧探针提前观察的问题单独记为 TD-20261014-15。此项是真实账号验证，不是 MockBackend；仍未执行真实断网/生产进程崩溃、组织账号、其他云盘、多进程完整竞争、未缓存延迟句柄 unlink 或正在创建云端目录时的 rmdir。Windows CI 为原生构建/单元/IPC，不能视作 Windows FUSE/真实 Graph 本次语义验证。本地正式升级结果独立记于下方上线补记。

**防复发与交付**：新增测试保存在 filesystem/deletion_tests.rs 和 core/tests.rs，上传并发用开始/放行屏障，默认忽略的挂载测试须显式执行。修复提交：[4d4f7ae](https://github.com/LumiaBlack51/twodrive/commit/4d4f7aec7424b75b686b60ab67cc1fe258a8fe64)；[指向 main 的草稿 PR #13](https://github.com/LumiaBlack51/twodrive/pull/13)。本机验证结果见上文；[最终 peer 原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563697042)、[最终独立 WebDAV 原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563697140) 和 [最终 Windows Full/Lite 构建与 IPC CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38563697093) 的六项任务全部通过，测试代码头为 53e0133。[main 合并提交 a380b48](https://github.com/LumiaBlack51/twodrive/commit/a380b48bf6f9ad5bf8eec517756800d42024420c) 已核实；[Release 0.2.13-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.13-1) 已正式发布并设为 Latest，5 个附件的 GitHub digest 与本地 SHA-256 全部相等。标签/源码/最终包输入为 [cfa1ffa](https://github.com/LumiaBlack51/twodrive/commit/cfa1ffaefbd5877dc281025085cdaefac1ab2164)，与最终 CI 代码头只差故障记录/脱敏汇总。deb SHA-256 为 `62e16998fc1f037ef60a6385995afc86c112a4defe7f6b3bba4cbcce274d01a3`。本地安装结果见下一段。


### 2026-10-14 本地上线与恢复保障补记

用户已明确授权更新本地软件。沿用用户目录安装方式，将稳定 CLI/daemon 从 0.2.10 升级到 0.2.13，托盘/Settings/Nautilus 扩展/桌面入口及 emblems 使用最终 deb 的原字节，保留现有自定义 systemd unit。升级前确认 Writing 为 0，停止 twodrive-daemon.service 后以 0700 私有目录备份 SQLite 及当时存在的 WAL/SHM、上传会话状态、旧程序/桌面组件和配置；需保护的未同步缓存数量为 0。备份 SQLite integrity_check 通过，凭据未写入本记录或发布附件。未删除用户原有文件或通过直接编辑数据库清队列。

新服务正常启动，CLI 与正在运行的 daemon 均实际报告 0.2.13，进程可执行文件 SHA-256 与最终包一致，FUSE 挂载可访问。数据库 integrity_check 通过，新增上传/删除队列字段已迁移。稳定托盘和 Nautilus 已重启加载已安装组件；未升级或重启 Development 桌面软件。

在已升级 daemon 的实际 OneDrive 挂载内，仅使用本轮新建的随机目录验证：512 KiB+7 字节上传并通过真实 Graph 下载比对 SHA-256，HEAD.lock/config.lock/index.lock 各 4 次创建/关闭/删除通过；临时文件和父目录云端清理确认。此项验证运行中的已安装程序，区别于上文同源码/锁定依赖的隔离 FUSE 探针；[脱敏报告](unlink-live-validation-2026-10-14.json)新增 installed_daemon_validation。测试不包含真实进程崩溃或其他账号。

上线后 Writing/Dirty/Uploading 均为 0，本轮无新增待删除记录。升级前已经存在的 1 项旧云端删除待办仍保留，不能把它视作本次关闭文件拒绝机制已经解决，也未擅自清除该记录；后续若调查其云端失败应单独定位和留档。现用服务/挂载和本次验证数据均已检查完成。


## TD-20261014-11：Windows 发布文本字节核对误报 CRLF 差异

- 日期：2026-10-14
- 状态：已验证发布审计规则；程序未修改。
- 影响版本与环境：0.1.2 发布组装的临时核对脚本，Windows Git checkout 文本。
- 关联历史故障：TD-20261014-07 也是跨平台检查假设遗漏，但根因不同。

### 症状与影响

Windows 包的安装脚本/说明/规范与 Linux 工作区原始 SHA 比较失败。PE x64 程序验证已通过；未发现程序损坏或文本语义变化。

### 触发条件与复现

对 Windows CI 的 CRLF 文本与 Git 源码 LF 文本直接按字节比较。

### 根因与证据

三份原始字节不同，全部在 CRLF→LF 后严格相等；属于核对方法的行尾假设，不是运行程序缺陷。

### 解决办法与恢复操作

原生程序/DLL/资产保持 CI 字节与 SHA 不变；核对文本时明确行尾归一化。最终组装文档/安装脚本采用发布源码的 LF 文本并记录组装来源，不冒称原始 Windows CI 文本字节相同。无用户本地恢复操作。

### 验证结果与边界

旧字节断言失败，行尾归一化后三份内容严格一致；Windows 三个 EXE 的 PE x64 和 ICO 资产通过。此项不属于 Windows 程序执行测试；原生执行证据由桌面 CI 单独提供。

### 防复发措施与后续

发布证据区分原生程序字节和源码文本组装，不以行尾差异否认已验证二进制。

### 交付记录

[构建 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。

## TD-20261014-10：配置落盘失败仍改变内存中的连接列表

- 日期：2026-10-14
- 状态：已验证本机旧代码失败/修复通过，最终 Linux/Windows 原生单元与 IPC 检查通过。
- 影响版本与环境：既有 desktop save_profile/remove_profile；本次允许在线管理其他连接后补查错误分支。
- 关联历史故障：TD-20261014-05 的在线管理；TD-20261014-09 的 Windows 持久化失败触发调查。

### 症状与影响

保存/删除配置先改变 App.config，State::save 失败后返回错误，但内存中连接列表已经改变，与磁盘不一致。未确认用户发生此现象；隔离测试以不可替换的目录目标复现。

### 触发条件与复现

新增/编辑/删除非活动配置，profiles.json 原子替换失败；源码中 retain/push/selected 操作均先于 State::save。

### 根因与证据

配置修改未采用先构造候选、落盘成功后发布的顺序；失败没有回滚。它不是导致 Windows os error 5 的根因，两条机制分别记录。

### 解决办法与恢复操作

新增/编辑/删除先 clone 并修改候选 Config；State::save 成功后才发布到 App.config。失败保留 profiles、selected 和活动 binding/revision；提供者 prepare 的私有目录副作用不在这一 Config 事务回滚范围。不操作用户配置恢复。

### 验证结果与边界

将 profiles.json 替换成目录目标触发持久化失败；同一回归临时恢复旧修改顺序时退出 101（配置一致性断言），修复后对新增/编辑/删除三条失败路径均通过。独立引擎 64 项测试、fmt 和严格 Clippy 通过。本轮未复现导致 Windows os error 5 的具体系统占用，不把错误注入当作该根因证据。

### 防复发措施与后续

保留落盘失败时列表、selected、活动 revision 不变的回归。

### 交付记录

[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。

## TD-20261014-09：Windows IPC 检查一次配置原子替换被拒绝

- 日期：2026-10-14
- 状态：后续两轮完整 Windows IPC 未复现；根因未确认，不宣称系统占用机制已修复。
- 影响版本与环境：dev 6aebdb6，Windows 桌面 CI 的隔离临时配置。
- 关联历史故障：TD-20261014-07 的路径断言已通过；本条发生于实际文件持久化，根因不同。

### 症状与影响

在线删除另一条临时连接时，State::save 的 NamedTempFile::persist 返回 Access is denied (os error 5)。阻断 CI；用户设备未执行此操作，未观察到用户配置丢失。

### 触发条件与复现

桌面 38552173558 的 Windows IPC 检查：新增、编辑其他配置、活动保护断言通过，remove_profile 保存时失败。目前单次记录，最终提交的独立任务正在执行相同回归。

### 根因与证据

已确认失败在替换 profiles.json 的持久化阶段；具体占用句柄、文件属性和竞争者未采集，不能直接断言是杀毒软件或归因于云盘协议。State::save 使用原子替换，没有先删除旧配置。

### 解决办法与恢复操作

待重复验证和定位。没有修改用户配置/桌面，也没有为了通过检查关闭 Windows 防护。

### 验证结果与边界

原失败任务 IPC 未完成；c80371b 与最终 863bf6d 的独立 Windows 原生 IPC 均完整通过，包括在线新增/编辑/删除、2 MiB 往返、备份与自启动隔离。没有复现具体系统拒绝来源；State::save 的原子替换实现未改，失败时内存配置保护另见 TD-20261014-10。

### 防复发措施与后续

保留在线连接管理的真实 IPC 回归；如稳定复现，补充原子替换错误分类/旧内容保留和有界重试证据。

### 交付记录

[失败 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38552173558)。[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。

## TD-20261014-08：既有上传并发测试依赖 100ms 调度窗口

- 日期：2026-10-14
- 状态：已验证本机旧窗口失败/新屏障通过；最终 Linux 原生根工作区 CI 通过。
- 影响版本与环境：既有 FUSE 单元测试；dev 6aebdb6 的 Linux peer CI。
- 关联历史故障：TD-20260916-04 是另一个 daemon 测试的短观察窗；本条不是相同代码路径。

### 症状与影响

immediate_reopen_during_upload_keeps_the_local_record 在进入重开操作前断言 active_uploads > 0 失败，阻断发布检查；没有证据表明产品的重开/云端保存行为失败。

### 触发条件与复现

测试等待后台线程进入上传，最多轮询 100 次、每次 1ms；模拟后端上传仅持续 100ms。GitHub Linux job 115812503295 在等待结束后未观察到活动上传。

### 根因与证据

源码用定时休眠近似上传开始与结束；调度稍迟或采样错过短窗口都会使前置条件失败，日志不能还原具体调度。拟改为明确的上传开始/放行屏障，保持“上传未完成时重开”断言，并注入超过旧窗口的启动延迟证明边界。

### 解决办法与恢复操作

给 mock 后端的首次上传增加开始信号与有界放行屏障；两项并发测试在后台确认上传开始后执行重开/替换，最后明确放行。补充云端 mock 最终内容断言。不修改生产上传逻辑，无用户恢复操作。

### 验证结果与边界

同一 mock 首次上传注入 150ms 启动延迟，临时恢复旧 100×1ms 轮询时退出 101（active_uploads 断言失败）；恢复新同步后两项测试通过。FUSE 单元套件 34 通过/6 项按默认忽略，fmt 与严格 Clippy 通过。最终 Linux peer/根工作区 CI 已通过相同测试；这些是 mock 测试，不是实际挂载或真实云端验证。

### 防复发措施与后续

相邻使用同一轮询的上传竞态测试同步改用受控屏障；保留最终本地身份和内容断言。

### 交付记录

[失败原生任务](https://github.com/LumiaBlack51/twodrive/actions/runs/38552173503/job/115812503295)。[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。

## TD-20261014-07：Development 发布 CI 的跨平台检查失败

- 日期：2026-10-14
- 状态：已验证两项检查修复的 Linux/Windows CI；原数据库争用未在原样重跑及最终 CI 复现，根因未确认。
- 影响版本与环境：dev 37f8f3b，GitHub Ubuntu 24.04 / Windows 原生 runner；尚未发布 0.1.2。
- 关联历史故障：TD-20261013-08 为 Windows 条件编译检查，TD-20260916-04 为 CI 时间窗；不能仅因 CI 失败认定相同根因。

### 症状与影响

桌面 Linux Python 套件缺少 Gtk4 namespace，Windows 独立引擎账号路径断言失败；另一 Windows peer 套件的并发数据库测试返回 database is locked。阻断 Release，用户客户端/稳定服务未被修改。

### 触发条件与复现

新源码推送后原生 CI：桌面 38550735306、存储 38550735280、peer 38550735303。本机 Linux 的相同套件此前通过；Windows 账号断言在两个独立任务失败，数据库失败目前一条任务。

### 根因与证据

Linux 新增全部 scripts unittest 后包含依赖 Gtk4 的既有 Settings 测试，runner 原先仅装 Gtk3，未声明 Gtk4 introspection 依赖。Windows State::new 正常 canonicalize 后产生 verbatim 路径前缀，测试用未 canonicalize 的 temp.path 做词法 starts_with，两个任务重复失败；实际账号目录没有落到稳定配置。这两项分别是 CI 依赖遗漏和跨平台测试断言错误。core 并发写入结果为 database is locked（busy_timeout=10s），尚未还原具体争用轨迹。原样重跑失败 Windows job 后整项 peer CI 38550735303 成功；不把重跑当作机制修复，不把它归因于新增云盘代码。

### 解决办法与恢复操作

声明 python3-gi/Gtk4/Adw introspection 依赖；账号回归比较 canonicalize 后的独立目录及确切文件名，保留账号隔离断言。数据库未修改生产代码/测试；保留偶发失败证据，根因未确认。无用户桌面、账号、云端文件恢复操作；只处理后台源码与 CI。

### 验证结果与边界

初次 Windows 原断言在两个任务失败；修正后的本机账号隔离测试、fmt 及 scripts 7 项 unittest 通过。最终原生 Linux scripts 全部 7 项通过，Windows 独立账号断言通过，四组最终 CI 全部成功；数据库单次失败不作为已修复机制。

### 防复发措施与后续

分别核对依赖声明、Windows 路径规范化和数据库争用机制，保留原有断言。

### 交付记录

[初次桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38550735306)。[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。

## TD-20261014-06：托盘状态面板未固定在任务栏图标正上方

- 日期：2026-10-14
- 状态：已验证 GNOME 46 私有 Wayland 与 Xvfb/X11 原生面板三轮切换；已安装并排队新桌面组件，当前旧客户端/桌面遵照用户约束未重启。
- 影响版本与环境：当前 dev Development Linux/Zorin 桌面，用户截图中的状态面板明显偏离下方任务栏图标。
- 关联历史故障：TD-20261013-06、TD-20261013-12、TD-20261013-13；分别处理鼠标抖动、GNOME 转发坐标与首映射位移。新症状尚不能仅凭漂移认定相同根因。

### 症状与影响

托盘弹出面板偏离任务栏图标，悬在屏幕右侧较高位置。用户期望水平中心对齐图标、底边稳定位于任务栏上方。无文件读写影响证据。

### 触发条件与复现

用户当前真实桌面截图已确认现象；受控重复打开、鼠标移动与缩放验证待执行。

### 根因与证据

已确认定位公式使用 `x + 20 - width`，把面板右侧对齐到点击点附近，未水平居中；纵向又依赖指针 y，可能离开任务栏边缘。当前桌面会话为 Wayland，客户端由旧启动器强制使用 X11/XWayland；旧逻辑在 GNOME XEmbed 激活时读取 X11 指针，无法把它证明为 Shell 图标边界。前次历史回归只核对指针公式，没有核对真实图标矩形，属于原修复覆盖遗漏，不是 Google 功能导致的回归。

新原生 Wayland 候选在隔离 GNOME 46 会话第一次展开正确、第二次却为 133,131（期望 1042,40）；确认 map-event 后过早请求时 MetaWindow 可能尚未就绪。改为有界映射后重试，覆盖初始 configure，并在定位前保持面板透明、取消/切回管理页时恢复不透明度，最终三轮一致。

### 解决办法与恢复操作

面板水平中心使用实际图标矩形中心，底边距离任务栏保留工作区边界 8 个逻辑像素；顶部任务栏相应向下展开，屏幕边缘必要时才限制横向边界。GNOME/Zorin 提供 TwoDrive 专用小组件，从 Shell 托盘 actor 读取准确逻辑矩形/工作区；X11 在 GTK/GDK 映射后定位，原生 Wayland 由 GNOME compositor 定位自己的 TwoDrive 面板。PositionPanel 校验 D-Bus 发送者 PID 等于目标 TwoDrive PID，窗口类匹配且无装饰，不提供任意窗口/坐标控制。小组件只调整 TwoDrive SNI 单击入口，不修改其他指示器；不依赖全局鼠标坐标或开启 Shell unsafe Eval。Windows 公式同步居中与任务栏边界；最终原生 Windows CI 验证三轮展开/收起及稳定位置，硬件点击/物理多屏的居中位置未验收。

遵守用户不影响前台编码的约束：测试全部使用私有 D-Bus/显示环境，不在用户桌面激活窗口、移动指针或测试弹窗。实际部署：已安装新 release 和 TwoDrive GNOME 组件，仅追加本组件启用项；新组件在当前 Shell 尚未加载，须下一次桌面登录。使用安装器保留旧运行路径到 `~/.local/share/twodrive-dav/runtime-before-7055ad73047e`，未终止当前 GUI/引擎，不修改 Google 私有令牌、连接列表、稳定服务或自启动设置。

### 验证结果与边界

恢复 HEAD 旧几何代码时，同一“中心对齐/任务栏边界”断言退出 -6，修复通过。C++ 几何回归与 GJS 几何/提供者身份测试通过，覆盖顶部/底部、负坐标屏幕、边缘约束与图标实际移动。`test-dav-panel-isolated.py --backend wayland` 在真实 GNOME 46 headless compositor + 原生 GTK Wayland 完成三轮展开/收起：真实 Shell actor 1328,0,36,32、工作区 0,32,1440,868，最终面板均为 1042,40,390,570（该虚拟图标靠屏幕右边缘，横向必须限位）；错误的 Activate(16,16) 不影响真实图标定位，其他进程的 PositionPanel 被拒绝，后台启动未映射窗口。[Wayland 摘要](tray-wayland-validation-2026-10-14.json)。

最初在私有 Wayland compositor 中启用 XWayland 的尝试缺少 systemd X11 session 服务，未当作 X11 产品失败或成功。后续转到独立 Xvfb/X11 compositor，使用真实虚拟显示器输入（核心合成事件没有使 GTK 激活，不把这一测试入口限制当作产品失败）；最终三轮单击展开/收起通过，稳定后矩形均为 1042,40,390,570，真实图标来源被核对。[X11 摘要](tray-x11-validation-2026-10-14.json)。Flutter analyze/release build 与 14 项测试通过，安装器保留运行中旧路径/账号配置与排队仅本组件的 2 项回归通过。当前实体 Zorin 2× 桌面遵照用户约束未弹窗实测；最终 Windows 原生 CI 托盘消息、外部焦点及 Escape 烟测通过，不等于用户硬件点击或物理任务栏位置验收。多实体显示器、其他 Wayland compositor 未实测，不承诺所有平台组合。

### 防复发措施与后续

保留图标中心/任务栏边界、重复展开位置不变、账号身份范围与坐标单位回归。CI 执行 C++/GJS 几何测试并包含组件；版本化 `test-dav-panel-isolated.py` 必须使用私有 D-Bus/显示器，不允许把虚拟输入发到用户前台显示器。

### 交付记录

[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。[定位实现与后台验证](tray-positioning-2026-10-14.md)。

## TD-20261014-05：连接任意账号后禁止添加或管理其他连接

- 日期：2026-10-14
- 状态：已验证在线配置管理、本机 UDP/QUIC、WebDAV FUSE/备份及真实 OneDrive 普通文件；实体 NAS/跨公网 NAT/真实 Windows 云端登录未验证。
- 影响版本与环境：现用 Development Flutter 客户端与 desktop IPC；Google 接入前已有的单连接管理限制。
- 关联历史故障：TD-20261013-15 的提供者边界和 TD-20261013-17 的凭据身份保护相关，但不是 Google 登录失败或凭据混用。

### 症状与影响

用户已连接 Google Drive，连接页“添加连接”及所有编辑/删除入口变灰，无法添加 WebDAV、设备共享或另一个云账号。截图中的当前连接正常，未确认数据损坏。

### 触发条件与复现

connected=true、busy=false、available=true；workspace.dart 将 connected 放入添加和所有配置操作的禁用条件，desktop save_profile 又无条件要求 connected=None。

### 根因与证据

保护活动连接的配置/revision 被扩大为保护整个配置列表。其合理边界应是不能编辑或删除正在使用的那一条；新增与修改其他连接不应断开当前连接。现有提供者目录只有 WebDAV、配对设备与 Google；OneDrive 和局域网 UDP 发现是另外的功能缺口，解除禁用本身不会补齐它们。

### 解决办法与恢复操作

允许在线添加/管理其他配置，保持活动连接与选择项；仅禁止编辑/删除正在使用的配置。点击已保存连接切换时沿用待上传缓存保护。补充 OneDrive schema/独立账号目录、复用 Microsoft OAuth/Graph 的适配器、浅层目录与严格条件上传；CLI 登录输出和浏览器子进程输出不得污染 IPC。增加 opt-in IPv4 UDP 发现与可信邀请导入，签名发现记录不包含授权秘密，导入必须匹配选定公钥，成功配对后删除私有邀请副本。表单由 schema 驱动，文件选择器/自动私有目录/自动挂载目录使一般设置不依赖内部状态路径。WebDAV NAS 的数字私有 IP HTTP 须显式允许。

本地恢复操作：已生成私有 runtime/state 备份 `~/.local/share/twodrive-dav/pre-connections-20261014-219835c6`。通过现有 guarded quit 先断开 Development，再替换运行包并重开；Google 已自动恢复连接。更新前后 profiles.json 字节一致，Google 私有令牌仍在，两个已安装引擎 SHA-256 与本轮 release 构建一致。原 Windows 配对目录、稳定 OneDrive daemon 与自启动配置未改动。

### 验证结果与边界

Rust `connected_account_can_manage_other_profiles_without_changing_active_revision` 和 Flutter `connected account can open add dialog and edit inactive accounts` 均已恢复旧条件证明失败，修复后通过。真实隔离 IPC 验证在线新增/编辑/删除其他配置时 selected、活动 revision/身份保持，活动配置禁止修改、未知 ID 不断开原连接。`test-storage-connections.py` 完成真实本机签名 UDP 发现、邀请身份检查/复制、QUIC 配对及读写/条件覆盖/改名/删除、无邀请重连/切回已有连接。

真实本机 WebDAV + FUSE 验证 2 MiB 上传下载 SHA-256 一致、挂载读写、暂停后待上传保护/恢复、备份创建修改与冲突保留。真实个人 OneDrive 使用稳定版登录文件的临时私有副本（不修改原配置/令牌或操作稳定服务），完成 8 MiB+123 字节上传/下载比对、条件覆盖/旧版本拒绝、改名/删除，自有随机目录已删除；[独立脱敏报告](onedrive-desktop-validation-2026-10-14.json)。这验证的是已有登录恢复和普通文件操作，新浏览器 OAuth、本轮真实 OneDrive FUSE/备份、组织账号未执行。

整个主 workspace 测试、fmt、严格 Clippy 通过（默认忽略的 FUSE/原生进程测试仍未执行）；独立实验 mount 测试 64 项、Flutter 14 项及 analyze、Python 30 项通过。最终 Windows CI 的提供者单元、在线配置管理、实际 loopback WebDAV IPC/备份及 Flutter 14 项通过，Windows 浏览器云端登录未执行。实体 NAS、跨公网 NAT 打洞未验证，不能把局域网发现或中继连接等同于所有网络的 UDP 打洞成功。

### 防复发措施与后续

保留 connected 状态的控件回归与在线配置管理 IPC 回归；新增提供者保持 schema 驱动。邀请过期/选错设备/私有副本权限与 NAS HTTP 范围均有回归；发现不包含秘密，不产生授权。真实 OneDrive 验证脚本仅使用自有随机目录，测试状态与稳定服务分开。

### 交付记录

[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。[存储连接使用与验证](storage-connections-dev.md)。

## TD-20261014-04：Google 验证脚本的无挂载分支错误限定 time 变量作用域

- 日期：2026-10-14
- 状态：已验证旧分支失败、新分支通过；默认无挂载真实 Google 验证也通过。
- 影响版本与环境：本轮新增 verify-google-drive.py；不影响产品读写或已通过的 `--mount` 运行。
- 关联历史故障：TD-20261014-03 的验证脚本维护发现，根因是 Python 局部变量作用域，与云端索引不同。

### 症状与影响

不带 `--mount` 时，备份等待函数可能因未绑定的局部 time 变量退出，不能完成验证。

### 触发条件与复现

脚本同时在模块和 `if args.mount` 中 import time；后者把整个 run 的 time 变成局部变量。无挂载分支不执行该导入，嵌套等待函数首次读取 time 时未绑定。

### 根因与证据

Python 静态局部变量规则。带挂载的实际成功测试不能覆盖该分支；没有把它归因于 Google 备份失败。

### 解决办法与恢复操作

移除条件内的重复导入，统一使用模块 time；同时移除清理改造遗留的无用 `if True`。没有用户数据恢复操作。

### 验证结果与边界

`test_without_mount_finishes_backup_and_owned_cleanup` 在本地 IPC 替身覆盖完整无挂载流程；恢复旧条件导入时脚本退出 1，报告确认未绑定 time，修复后通过。同一默认无挂载脚本另在真实 Google 完成 8 MiB+123 字节分块上传/下载 SHA-256 比对、覆盖/移动/删除与备份创建/修改；测试目录移入回收站。[独立脱敏摘要](google-drive-large-validation-2026-10-14.json)。模拟回归与真实结果分别记录。

### 防复发措施与后续

以默认无挂载命令完成另一轮真实临时文件和备份验证；只有 `--mount` 运行通过不能当作两种分支均通过。

### 交付记录

[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。[Google Drive 接入](google-drive-dev.md)。

## TD-20261014-03：Google 全目录逐层请求超过挂载启动时限

- 日期：2026-10-14
- 状态：已验证本机真实 Google 冷索引及 FUSE 读写；其他账户/网络启动耗时未验证。
- 影响版本与环境：本轮 Google 适配器的真实 Linux 挂载验证；桌面挂载有 60 秒启动预算。
- 关联历史故障：TD-20261014-02 的内容 ETag 问题已处理；本次真实文件读写/覆盖/移动已完成后，挂载阶段超时。

### 症状与影响

第二轮真实临时目录测试完成上传、下载比对、条件覆盖、旧版本拒绝、移动，随后 `mount` 返回挂载超时。测试挂载未建立，进程被回收，测试目录已移入回收站；检查无本轮遗留挂载。

### 触发条件与复现

mount worker 先拉取全盘元数据，初版 snapshot 对每个目录发出一次 children 请求，完成后才启动 FUSE。桌面在 60 秒后终止启动。尚缺完整 worker 阶段耗时，不能仅凭超时断言 FUSE 本身失败。

### 根因与证据

初版存在逐目录请求，分页库存改造的隔离回归证明旧代码需要 3 次请求、修复后同一树只需 1 次。2026-10-14 更正/补充：分页改造后仍在 60 秒超时，不能把复杂度改进视为已解决实际故障。脱敏 worker 阶段显示 account probe 已完成、inventory 尚未完成；独立只读探针在 142.61 秒读取 80 页/36,800 个对象仍有后续页。这确认了真实大库存的冷索引超过固定启动预算，不是已证明的 FUSE 失败。

### 解决办法与恢复操作

Google 全量索引改为分页库存+内存建树，并拒绝重复分页游标/ID；提供者声明独立的最长 600 秒冷启动预算，通用挂载服务读取该预算并保持取消与进程回收。其他提供者仍为原 60 秒预算。增加只含阶段、页数和对象数的日志，绝不记录名称、ID 或 page token。没有操作稳定 OneDrive 服务；失败脚本已安全清理自有临时目录，登录状态保留。

### 验证结果与边界

旧逐层函数在 `snapshot_uses_one_inventory_for_nested_directories` 失败（3 次请求，期望 1）；新函数通过。保留 60 秒预算的分页版本再次真实超时；600 秒预算版本完成真实个人盘冷索引并成功挂载：读取临时文件正确、挂载写入完成后云端读回字节一致，随后卸载。相同真实运行还通过文件夹备份创建/修改和本地删除不传播云端删除，全部测试对象移入回收站，无遗留测试挂载。实际其他账号/更大库存/网络变化仍可能超出预算。

### 防复发措施与后续

增加批量 snapshot 请求数量回归与实际 FUSE 云端往返；记录失败与成功各自边界。

### 交付记录

[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。[Google Drive dev 接入](google-drive-dev.md)。

## TD-20261014-02：Google 真实下载拒绝元数据 ETag 的 If-Match

- 日期：2026-10-14
- 状态：已验证本机真实 Google 普通文件下载与读写闭环；其他文件种类/平台未验证。
- 影响版本与环境：本轮新 Google 适配器；Linux 对真实 Google Drive 的首次读写验证。
- 关联历史故障：TD-20261014-01 是中文显示问题，根因不同；TD-20261013-15 只验证模拟非 DAV 合同，没有验证真实 Google API。

### 症状与影响

真实账号成功登录、创建目录并分块上传后，下载返回 HTTP 412。模拟服务相同操作通过。未验证下载成功，没有修改用户既有云端文件。

### 触发条件与复现

使用上传对象的 v2 文件元数据 ETag，在 `files.get?alt=media` 中传入 If-Match；真实读写脚本的第一次下载失败。测试目录已由 finally 移入回收站。

### 根因与证据

真实临时对象探针：无条件内容 GET 200；元数据字段 ETag 与内容 HTTP ETag 不同；以元数据 ETag 条件内容 GET 412。另用真实旧版本 ETag 验证：元数据 PATCH 412，resumable 初始化 200、最终 PUT 412，原内容保持不变。证明下载和最终条件写入的资源语义不同。任意伪造 ETag 的早期探针出现 PATCH 500，不能当作有效旧版本的条件冲突证据；之后改用实际已过期 ETag。

### 解决办法与恢复操作

内容下载不再携带元数据 If-Match；读取前后比较同一元数据 ETag 和内容长度，变化时报错，桌面/挂载临时下载不发布结果。条件上传仍在初始化与最终 PUT 携带旧版本 ETag。另将真实挂载验证脚本改为先卸载、确认无挂载后才清理本地临时树，避免失败清理触及挂载内容。本机保留私有登录状态，没有既有数据恢复。

### 验证结果与边界

首次真实下载失败；修复后的真实 256 KiB 上传/下载 SHA-256 一致，正确版本覆盖成功、旧版本被拒绝且新内容保留；移动和回收站删除成功。真实旧 ETag 的服务端 final PUT 412 与原内容保留已由独立临时探针确认。`media_etag_differs_and_metadata_change_during_download_is_rejected` 覆盖不同资源 ETag 及下载中元数据变化。真实 FUSE 与备份也通过，见 TD-20261014-03 和[脱敏验证摘要](google-drive-validation-2026-10-14.json)。没有重新测试 OneDrive，也没有 Google 在线文档字节编辑或 Windows 实测。

### 防复发措施与后续

新增区分内容与元数据 ETag 的回归；修复后重跑真实临时目录读写、服务端条件覆盖及 FUSE 比对。

### 交付记录

[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。[Google Drive dev 接入](google-drive-dev.md)。

## TD-20261014-01：新增 Google 适配器将中文文件名显示为百分号编码

- 日期：2026-10-14
- 状态：已验证中文名称的模拟 HTTP 回归修复；实际账号接入验证单独记录。
- 影响版本与环境：本轮尚未提交的 Google Drive 实现；未安装到用户运行环境。
- 关联历史故障：[TD-20261013-15](#td-20261013-15桌面文件与备份服务绑定-webdav阻碍其他存储提供者) 的提供者接口是接入基础；此次为新适配器文件名编码错误，不是旧提供者回归。

### 症状与影响

Google 中文名称可能显示为 `%E4…`，影响浏览可读性。本地 HTTP 替身测试发现，没有证据表明真实云端文件名曾被修改。

### 触发条件与复现

`duplicate_titles_are_distinct_and_native_docs_are_explicit_shortcuts` 构造中文名称和云端名称中的斜杠；初版未通过中文可读路径断言，另六项 Google 测试通过。

### 根因与证据

初版使用 percent_encoding::percent_encode，它会编码非 ASCII 字节，即使 ASCII escape set 未包含它们。并非 Google API 返回中文乱码。

### 解决办法与恢复操作

逐 Unicode 字符保留普通字符，仅转义控制字符及本地路径保留字符；空名、点名及首尾空白有独立显示别名。普通百分号/波浪号保持原样，同名与特殊路径别名通过远端 ID 解析，避免把新名称解码为另一目标。没有真实账号数据恢复操作。交付时另备份用户 Development 原 runtime 与 profiles，安装新 Development 二进制并保存独立 Google 连接；原连接配置保留，稳定 OneDrive 服务未操作。

### 验证结果与边界

旧实现的上述断言失败；修复后 Google 回归通过，包括中文名称、保留字符显示与普通百分号/波浪号名称、同名独立 ID 和在线文档快捷入口。替身测试不等于真实 Google 或 OneDrive 端到端验证。

### 防复发措施与后续

保留中文/斜杠/百分号/同名/点名往返断言；后续实际登录与隔离读写验证另记在本轮接入文档。

### 交付记录

[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)；[最终桌面 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38553734864)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（预览版，6 个附件；程序来自 863bf6d，源码标签 d39b32e；详见[交付记录](development-0.1.2.md)）。接入说明：[Google Drive dev 接入](google-drive-dev.md)。

## TD-20261013-18：黄色托盘回归把 Windows ICO 误当 Linux SVG

- 日期：2026-10-13
- 状态：已验证测试平台假设修复；Linux 与 Windows 完整 CI 均通过。
- 影响版本与环境：本轮新测试在 Windows CI 38363626181 失败；未证明产品图标实现异常。
- 关联历史故障：TD-20261013-16 的验证遗漏，非路由或显示机制复发。

### 症状与影响

Windows Flutter 测试实际正确选择 cloud-warning.ico，却被只允许 cloud-warning.svg 的断言拒绝，阻止后续 Windows 原生构建与资产产出。

### 触发条件与复现

在 Windows 上执行 successful proxy fallback uses warning triangle and native warning asset。CI 日志显示12项通过、1项失败，Expected cloud-warning.svg，Actual cloud-warning.ico。

### 根因与证据

新测试硬编码 Linux 后缀；StatusTray 原有实现正确按 Windows 选择 ICO。属于测试缺陷，没有证据支持修改产品资产选择逻辑。

### 解决办法与恢复操作

回归按实际平台断言 SVG 或 ICO，错误图标断言同样处理；不放宽 warning/error 严重程度检查。没有本机恢复或安装操作。

### 验证结果与边界

失败证据来自真实 Windows CI；Linux analyzer 与13项控件回归复验通过。修正后的原生 Windows CI 同样通过 Flutter 检查、13项回归、原生构建、IPC/备份和托盘事件烟测；这不代表用户 Windows 设备上的硬件点击验收。

### 防复发措施与后续

保留原生 Windows/Linux 的同一断言；产物资产按各自平台实际加载格式核对。

### 交付记录

失败[CI 38363626181](https://github.com/LumiaBlack51/twodrive/actions/runs/38363626181)；[修复4554021](https://github.com/LumiaBlack51/twodrive/commit/4554021886852e71b04299fd2b2ccc240442c2c7)；[通过的 Linux/Windows CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38364410735)。已通过[PR #11](https://github.com/LumiaBlack51/twodrive/pull/11) 合入 dev，Release 已发布，见 TD-20261013-15 的交付记录。

## TD-20261013-17：编辑连接身份时沿用原账户保存的密码

- 日期：2026-10-13
- 状态：已验证隔离配置保存回归；未操作真实账户。
- 影响版本与环境：dev `8482ee4` 桌面 save_profile；本次解耦中发现。
- 关联历史故障：TD-20261013-02 的连接绑定遗漏；这里涉及保存凭据的身份边界，而非文件请求目标。

### 症状与影响

编辑同一 profile 的服务器或用户名、或切换提供者时，密码留空仍可能沿用旧账户密码。源码确认保存逻辑只按 profile ID 保留密码，没有检查服务器/用户名/提供者；未确认真实凭据曾被发送到错误服务器。

### 触发条件与复现

使用隔离合成配置：已保存密码的 DAV profile，编辑用户名或切换 peer，密码留空。旧逻辑无条件复制 previous.password；没有使用真实凭据。

### 根因与证据

旧 desktop save_profile 仅检查 p.password.is_empty 与 clear_password，不验证认证身份。将凭据保留规则移到提供者 schema 的 identity_fields 后，同一身份才能保留；切换提供者只保存其 schema 的字段。

### 解决办法与恢复操作

提供者声明秘密字段与身份字段。相同身份留空保留、显式清除可删除；更换身份不会继承秘密，转换提供者会移除其他提供者字段。未知提供者不将不透明 options 输出给 UI。没有修改真实配置或令牌。

### 验证结果与边界

legacy_profiles_roundtrip_and_redact_secrets 验证旧扁平配置兼容、脱敏、同身份保留、更换用户名不保留、切换提供者清理旧字段；unknown_provider_never_exposes_opaque_options 验证未知提供者不输出 password/token。为隔离配置回归，未执行真实账号认证或更换服务器；未主张过去发生实际泄露。

### 防复发措施与后续

新增提供者必须声明身份和秘密字段；禁止通用服务按 password 字段猜测凭据规则。

### 交付记录

[修复0be09fa](https://github.com/LumiaBlack51/twodrive/commit/0be09fa5d1c36558c9a25804d37b1c62a1aa16bf)；[PR #11](https://github.com/LumiaBlack51/twodrive/pull/11) 已合入 dev；Release 已发布，与 TD-20261013-15 同批交付，见该条交付记录。

## TD-20261013-16：自动路由成功兜底被显示为红色错误

- 日期：2026-10-13
- 状态：已验证 Flutter 控件与原生托盘图标参数回归；完整硬件交互未验证。
- 影响版本与环境：dev `8482ee4` Linux/Windows Flutter 桌面。
- 关联历史故障：TD-20261013-04 的代理兜底状态展示；本条纠正提示严重程度，不改变实际网络路由。

### 症状与影响

用户要求无法绕过代理但通过兜底成功连接时使用黄色三角感叹号。旧 updateTray 把 proxyFallback 和连接/备份错误合并为 cloud-error，令已成功连接被表示为错误。

### 触发条件与复现

connected=true，自动直连探测失败并成功选择 system/proxy fallback；没有其他错误。隔离 Flutter 控件回归验证传给原生托盘的资产路径。

### 根因与证据

旧条件 c.error != null || c.syncError || c.proxyFallback 统一选择 error。使用原始 dev 前端与同一托盘回归（仅兼容类名）时，cloud-warning 断言被 cloud-error 路径拒绝；修复后同项通过。

### 解决办法与恢复操作

成功连接后的 route_fallback 用独立 warning 状态，新增黄色三角感叹号 SVG 与 Windows ICO；托盘面板和工作区显示相同警告。真实连接/同步错误优先，断开后不保留兜底警告。UI 消费结构化布尔状态，不根据任意日志文本判断严重程度。兜底标志独立于24条诊断环，避免日志轮转丢失；每次连接使用独立诊断对象，旧进程残留输出不污染新连接。未更改代理配置或运行中的安装。

### 验证结果与边界

Linux 与 Windows 的13项 Flutter 回归、analyzer 和原生构建通过；warning 控件回归检查 native configure 的 cloud-warning.svg / cloud-warning.ico、黄色三角图标及错误图标，另验证离线、暂停、备份失败优先级。原始 dev 控件回归失败。诊断回归验证兜底标志在日志轮转后仍保留；[持久状态修复f2bc413](https://github.com/LumiaBlack51/twodrive/commit/f2bc41367615bee573af115a4f5eee4a3fef8085)。Windows CI 的原生托盘事件、外部焦点与 Escape 烟测通过。控件、MethodChannel 替身和合成原生事件不等于用户硬件点击、所有桌面主题或实际代理网络验收。

### 防复发措施与后续

保留 provider_contract_test.dart 的严重程度和托盘回归，成功兜底与传输失败分别处理。

### 交付记录

[修复0be09fa](https://github.com/LumiaBlack51/twodrive/commit/0be09fa5d1c36558c9a25804d37b1c62a1aa16bf)；[PR #11](https://github.com/LumiaBlack51/twodrive/pull/11) 已合入 dev；Release 已发布，与 TD-20261013-15 同批交付，见该条交付记录。

## TD-20261013-15：桌面文件与备份服务绑定 WebDAV，阻碍其他存储提供者

- 日期：2026-10-13
- 状态：已验证提供者边界与非 DAV 回归、Linux/Windows CI、发布产物和隔离真实 FUSE 挂载；真实云端未验证。
- 影响版本与环境：dev `8482ee4` 的 Flutter 桌面客户端、IPC 引擎、备份与挂载适配层；Development 版本更新为 0.1.1；合并后的 Linux 软件包版本为 0.2.12-1。
- 关联历史故障：TD-20261013-02 的账户绑定与 TD-20261013-04 的备份/路由功能；[历史 provider 边界](refactor-2026-09-16.md)。本次是新增桌面实现的边界遗漏，非已证明的历史存储重构回归。

### 症状与影响

用户报告 WebDAV 与前端及其他组件耦合，影响非 WebDAV 支持。确认通用 IPC、备份、mount 都要求具体 WebDavBackend；前端账户表单和标签只能识别 dav/peer，且用 writable 猜测全部修改能力。未确认数据丢失。

### 触发条件与复现

使用隔离非 DAV 提供者，远端 ID 为 object-N，与 /folder/file 路径不同；执行实际 IPC 文件操作、备份已提交意图恢复和界面能力控制。没有网络或真实云端账户。

### 根因与证据

- desktop App 与 backup/mount 签名引用具体 WebDavBackend，通用代码同时承担 DAV 认证/路由和配对网关启动；worker 使用 DAV 根 URL 传递连接。前端写死提供者字段、名称和 kind 分支。
- download/rename/delete 和备份恢复直接把路径作为 CloudBackend 的 remote_id。DAV 正好以路径作为 ID，掩盖了这一假设；Graph 等 ID 与路径分离的提供者不符合该假设。
- CloudBackend 的历史 upload_with_etag 默认实现不能保证条件写入。若仅替换为 trait 而仍依赖默认实现，备份和覆盖保护可能失效，故新增接口要求显式条件上传能力。

### 解决办法与恢复操作

- Provider/Backend 定义目录浏览、流式条件上传、能力、路径到不透明 ID 的转换；备份和挂载使用通用接口，CloudBackend 与稳定恢复契约保留。
- 提供者适配器拥有认证、连接、路由、账户绑定、保护目录与不透明 worker recipe；私有 session 文件绑定提供者和 profile revision，worker 恢复不依赖 DAV URL 参数。原扁平 profiles、mount/backup binding 保持兼容。
- Flutter StorageClient、动态 schema 表单、提供者标签与逐项能力来自 IPC。新增提供者无需更改通用文件/备份/mount 服务或前端 provider kind 分支。未知提供者不能静默作为 DAV 连接。
- 备份歧义提交恢复先按路径查元数据，再通过提供者 ID 下载比对哈希；冲突保留意图及远端内容。条件上传能力不足时不提供上传/备份/读写挂载。
- 已合并先前遗漏的 codex/pin-download-feedback 三项提交，并保留双方故障记录；其他远端功能分支已在 dev 的祖先中。main 的已合并内容已在56248a7同步合入，以对齐历史。

**本地恢复操作**：在独立工作区开发。未修改原工作区未提交调查、真实配置、私人文件、运行服务或已安装客户端；本轮仅用临时测试数据。

### 验证结果与边界

- Development Rust 49 项通过：29 库测试、4 引擎单元测试、11 集成、3 非 DAV 契约与2签名控制。严格 Clippy 与格式通过。
- 旧表达式回归在隔离源码副本分别恢复 desktop 和 backup 的 path-as-ID 调用，两项行为测试编译成功并在不透明 ID 断言失败；正常代码通过。这是对旧机制的受控回退，非原始完整旧树已支持非 DAV。
- 13 项 Flutter 测试、analyzer 和 Linux release 构建通过。新提供者表单测试只用自定义 bucket 字段，无 DAV/peer 字段；能力分别控制上传/新建/重命名/删除/备份。
- 完整根工作区151项默认 Rust、25项 Nautilus、4项 Settings 回归通过；根工作区格式和严格 Clippy 通过。默认忽略的7项中，6项 FUSE 回归使用新 deb 内的 CLI 显式执行并通过，覆盖权限/编译重挂载、索引器复查、目录元数据免下载、大目录读写响应、释放取消/旧句柄和待处理目录解压；余下1项云端/设备环境测试未执行。包内 Nautilus 25项回归及 CLI/daemon 隔离 FUSE+MockBackend 烟测通过。
- 验证非 DAV 文件下载/移动/删除、浅目录浏览、create-only/陈旧版本保护、取消与暂停、备份恢复/远端冲突、旧配置/凭据脱敏与 worker revision；本地真实 FUSE + loopback WebDAV 的 CLI 进程烟测已通过（文件读写、挂载所有权、缓存释放、暂停/恢复、备份创建/更新/歧义提交恢复/冲突保护、只读和默认关闭的隔离自启动）；同一烟测使用最终 CI 的 Linux 分发包再次通过。Windows CI 已通过原生构建、IPC/备份、自启动隔离与托盘合成事件/焦点/Escape 检查；未安装到用户 Windows 设备。
- **边界**：非 DAV 是隔离内存替身，不代表真实 OneDrive、Google Drive 新 OAuth 集成或端到端验证；本轮未使用实际账户。当前配对适配器内部仍通过 loopback DAV gateway 访问设备，这是适配器的实现细节，不再是前端/通用服务合同。新提供者仍需自行实现并注册适配器和服务契约测试。

### 防复发措施与后续

保留不透明 ID 的实际 IPC、备份恢复和条件上传能力回归，以及动态 schema/控件测试。CI 新增两条旧表达式失败验证；新增适配器须明确条件上传、路径/ID 与能力约定，不能仅依赖兼容 trait 的默认行为。详见[边界与验证](provider-boundaries-2026-10-13.md)。

### 交付记录

- 已推送调查提交8c43805、遗漏修复合并237c87c、[修复0be09fa](https://github.com/LumiaBlack51/twodrive/commit/0be09fa5d1c36558c9a25804d37b1c62a1aa16bf)、main历史合并56248a7；[PR #11](https://github.com/LumiaBlack51/twodrive/pull/11) 已于2026-10-13合入 dev，合并提交 [bff03bd](https://github.com/LumiaBlack51/twodrive/commit/bff03bdfc13eab7ab079433bdde8d4155a043ef1)。所有现存远端功能分支均为该提交祖先。
- Linux 0.2.12-1 deb 从干净源码56248a7822156ea2e10fade28bbb2dedcc2e7f2e构建；SHA-256 `12bd85c3e97f3931f06fdc295cc35c6f26663bbc732afbcf3fdb8dfa43bf16a9`。解包二进制与release构建、桌面辅助程序与源码字节一致；包内 CLI 为0.2.12、无运行令牌或状态，包内 CLI/daemon 隔离 FUSE+MockBackend 冒烟通过。它不代表实际云端验证。后续提交对稳定组件没有源码变化，已逐字节核对。Release 已发布，见 TD-20261013-15 的交付记录。
- 最终验证源码4554021886852e71b04299fd2b2ccc240442c2c7与 dev 合并树完全一致。四组 CI 均成功：[桌面 Linux/Windows](https://github.com/LumiaBlack51/twodrive/actions/runs/38364410735)、[Development DAV/QUIC](https://github.com/LumiaBlack51/twodrive/actions/runs/38364410787)、[Peer 原生构建](https://github.com/LumiaBlack51/twodrive/actions/runs/38364410773)、[Windows 双预览/根工作区](https://github.com/LumiaBlack51/twodrive/actions/runs/38364410768)。Development Linux/Windows 分发包取自该桌面 CI，黄色 SVG/ICO 资产均已核对。
- 发布前脱敏验证摘要及二进制 SHA-256 见 [provider-validation-2026-10-13.json](provider-validation-2026-10-13.json)。安装包中的故障记录为构建时快照；源码归档将包含发布前完整验证记录。发布链接已核实，见以下补记。


- 已发布 [Linux TwoDrive 0.2.12-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.12-1)（Latest）和 [Development 0.1.1 Linux/Windows](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.1)（预览版）。两标签均指向已推送 dev 提交93d1b44e7256f19d4b0822f22c2f90a066ec3305；两个源码归档均由该提交 git archive 生成，不含工作区未提交文件。
- 两个 Release 各5项资产（分发包/校验/源码/脱敏验证摘要）均为 uploaded，GitHub 报告 SHA-256 与本地全部一致；完整摘要保存在 [provider-validation-2026-10-13.json](provider-validation-2026-10-13.json)。本次交付补记仅更新链接和摘要，不修改已发布二进制。安装包内记录为构建快照，Release 源码与 verification.json 为发布前验证快照。

## TD-20261013-14：映射后仅移动窗口保留了错误的缓存尺寸

- 日期：2026-10-13
- 状态：已验证本机既有连接配置与隔离空状态的完整矩形回归。
- 影响版本与环境：a1060fa 的本机 GNOME/X11，已配置实际 Windows 连接的 Development 客户端。
- 关联历史故障：TD-20261013-13 的修复遗漏；本条是完整矩形与缓存尺寸，不是锚点来源复发。

### 症状与影响

隔离空配置的候选已通过位置和尺寸回归，但安装到已有配置后首次展开只有 592×952 物理像素，期望 780×1140。位置 936,88 已正确，WM_NORMAL_HINTS 的最小/最大尺寸仍是 780×1140。没有改动远端文件。

### 触发条件与复现

安装运行时以既有 Windows 连接后台启动，再进行同一原生单击位置/尺寸检查；旧 gdk_window_move 候选稳定被尺寸断言拒绝。

### 根因与证据

映射后回调只更新位置，保留 GDK 当时缓存的尺寸，未重新应用目标完整矩形。实际 X11 尺寸与 GTK 设置的几何约束不一致。窗口缓存尺寸出现差异的更深层装饰/布局时序仍未确认。

### 解决办法与恢复操作

映射后使用 gdk_window_move_resize 同时提交已计算的位置和尺寸。部署复验必须覆盖既有连接配置，不仅覆盖空状态。仅替换独立 Development 运行时，保留配对状态。

### 验证结果与边界

旧仅移动的候选在已部署客户端中被 592×952 尺寸断言拒绝。完整矩形候选在已配置 Windows 连接的安装运行时通过三轮：实际位置 936,88、尺寸 780×1140，展开 28–54ms、收起 4ms；隔离空状态另行通过，展开 31–36ms、收起 4ms、退出码 0。每次都等待 400ms 后核对矩形。安装运行时随后连接实际 Windows 网关，PROPFIND 207；自启动仍关闭，稳定 daemon PID 2306/NRestarts 0。验证仅覆盖本机 GNOME/X11、2× 缩放和合成宿主转发事件，未覆盖多屏、硬件输入或其他桌面。

### 防复发措施与后续

原生回归核对最终位置与最终尺寸；部署后的检查与隔离检查分别记录。挂载和稳定版守护进程不参与此次窗口恢复。

### 交付记录

[修复 3f2470a](https://github.com/LumiaBlack51/twodrive/commit/3f2470afacc3af51d7972ab61eaa5a0b2f681f6e)；[交付 PR #10](https://github.com/LumiaBlack51/twodrive/pull/10)；[最终 Linux CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38354827650) 成功。验证后清理本轮构建和临时文件，安装运行时及身份保留。

## TD-20261013-13：GTK 面板首映射的实际位置偏离已计算边界

- 日期：2026-10-13
- 状态：已验证本机映射后定位修复；首映射偏移的更深层原因未确认。
- 影响版本与环境：本机 GNOME/X11、2× 缩放，TD-20261013-12 坐标来源修正后的未提交候选。
- 关联历史故障：TD-20261013-06/12；不同于把图标内部坐标误认为屏幕坐标。

### 症状与影响

真实指针来源修正后，面板仍偏离预期位置。Native trace 在激活时得到逻辑指针 838,634、工作区 0,0,1440,843，计算边界 468,44；稳定后 X11 窗口为物理 1030,162，未达到预期 936,88。

### 触发条件与复现

后台启动的新进程首次显示无装饰状态面板，同一个真实指针只读查询回归重复失败。

### 根因与证据

已排除本轮指针来源错误：native trace 的点与真实指针一致，计算结果也正确。GTK 请求与实际 X11 位置不同，首映射/窗口装饰坐标转换的深层原因尚未确认；不能将偏移归于某个窗口管理器机制而无证据。

### 解决办法与恢复操作

映射事件到达后以一次 GLib idle 回调，用 GDK 原生窗口位置接口应用已计算边界；隐藏后回调不移动窗口，销毁时移除待执行源。映射前立即调用 GDK move 的候选仍失败，等待 map-event 后同一断言通过。[GTK move 文档](https://docs.gtk.org/gtk3/method.Window.move.html) 说明初始位置请求可能被窗口管理器修改。仅重启独立 Development 客户端。

### 验证结果与边界

旧候选的实际坐标断言失败。修复后三轮原生事件回归通过，最终坐标逐次匹配真实锚点计算，打开 32–42ms、收起 4–5ms、退出码 0；多屏、其他桌面和缩放组合未实测。

后续安装到既有连接配置时发现位置已正确但尺寸不一致，另记 TD-20261013-14；旧隔离检查不足以代表实际部署。映射后完整矩形修复已通过两种状态的最终检查。

### 防复发措施与后续

位置回归同时检查真实锚点、最终稳定坐标和重复展开，而非仅比较多次位置是否相同。

### 交付记录

[修复 a1060fa](https://github.com/LumiaBlack51/twodrive/commit/a1060fa097658c0491afb1d39c96da7c325ff663)；部署尺寸补充见 TD-20261013-14。

## TD-20261013-12：GNOME 传统托盘转发坐标被误认为屏幕坐标

- 日期：2026-10-13
- 状态：已验证本机原生坐标来源、三轮切换与安装运行时；尺寸补充见 TD-20261013-14。
- 影响版本与环境：7632033 的 Linux GTK XEmbed 托盘，GNOME 46/Zorin。
- 关联历史故障：TD-20261013-06/07 的位置与单击修复遗漏；保留原结论并补充坐标边界更正。

### 症状与影响

GNOME 传统托盘将激活消息转发到 GTK 隐藏图标窗口，当前客户端把转发的 x_root/y_root 当屏幕点击坐标，使面板出现在屏幕左上角，而非用户点击的任务栏图标附近。之前合成事件证明了切换和尺寸稳定，但没有证明真实宿主坐标含义。

### 触发条件与复现

GNOME Shell.TrayIcon.click 转发 EnterNotify 和按钮消息，根坐标等于图标内部中心。本机 GtkStatusIcon 窗口及其宿主 socket 位于 0,0，图标已嵌入；旧合成测试稳定得到面板 16,56（物理像素），与实际鼠标位置无关。

### 根因与证据

[GNOME 46 na_tray_child_emulate_event](https://github.com/GNOME/gnome-shell/blob/46.0/src/tray/na-tray-child.c) 将 x_root/y_root 设置为 width/2、height/2；原生 legacy_activate 直接读取该值。GtkStatusIcon 的窗口几何也不是 Shell 克隆图标的任务栏位置。SNI Activate 的宿主坐标是另一条路径，不能据此推断 XEmbed 坐标相同。

### 解决办法与恢复操作

仅 GNOME GTK 传统托盘在激活回调到达时立即读取指针屏幕坐标，异步窗口操作之前保存；小范围重复点击沿用固定锚点。键盘激活已有锚点时复用该点。SNI 保留宿主坐标。不修改系统扩展或移动全局指针。

### 验证结果与边界

需要在完全仿照 GNOME 的内部中心转发消息下，对照只读查询的实际指针/工作区验证位置。同一原生回归在旧代码报告位置不匹配并失败；修复坐标来源并完成 TD-20261013-13 的映射定位后通过，实际位置与真实指针、工作区和 2× 缩放计算一致。不得将先前“稳定”结论当成真实图标附近位置已正确的证据；未进行硬件输入、多屏或其他宿主实测。

### 防复发措施与后续

原生回归除了稳定性还要核对锚点来源，故意保持转发坐标与真实指针不同；保留工作区和 HiDPI 坐标计算检查。

### 交付记录

[修复 a1060fa](https://github.com/LumiaBlack51/twodrive/commit/a1060fa097658c0491afb1d39c96da7c325ff663)。

## TD-20261013-11：Windows 面板已有焦点时失焦关闭状态可能未启用

- 日期：2026-10-13
- 状态：已验证 Windows CI 原生外部焦点与 Escape 检查；整体任务成功。
- 影响版本与环境：b3ccfda 的 Windows native tray；不涉及 Linux 宿主单击机制。
- 关联历史故障：TD-20261013-07/09 的原生交互边界。

### 症状与影响

审查发现 panel 调用先将 focused_ 清为 false，随后只依赖新的 WM_ACTIVATE 来启用失焦关闭。如果窗口在恢复/调整时已经是前台窗口，SetForegroundWindow 不需要再次发送激活事件，可能使外部点击无法收起。尚未在用户 Windows 桌面观察到此条件。

### 触发条件与复现

面板复用已有前台窗口时，焦点事件可能发生在 panel 调用之前。原有烟测只验证通知消息切换，没有覆盖外部窗口取得焦点。

### 根因与证据

窗口的实际前台状态与 focused_ 记录可能不一致；源码缺少显示完成后的实际前台状态查询。需要原生外部焦点转移测试补足边界，不以合成单击通过代替它。

### 解决办法与恢复操作

显示面板并请求前台后，以 GetForegroundWindow 核对当前状态；保留后续 WM_ACTIVATE 处理。测试创建自有临时焦点窗口验证失焦收起，并验证 Escape 通知收起。无用户配置恢复操作。

### 验证结果与边界

[原生 Windows GUI 检查](https://github.com/LumiaBlack51/twodrive/actions/runs/38351595568/job/115145005154) 的烟测步骤通过，涵盖外部临时窗口取得焦点、重新展开和 Escape 通知收起；此前六次消息切换测试不覆盖此状态条件。测试仅操作 CI 内自有窗口，不使用私人账号或硬件鼠标输入；旧版本未单独复跑该边界。

### 防复发措施与后续

保留单击、窗口稳定、外部焦点转移和 Escape 的独立断言，不依赖点击前后总会产生新的激活事件。

### 交付记录

[修复 7632033](https://github.com/LumiaBlack51/twodrive/commit/7632033f0f60dbfee3c381d426493d697b33d8a1)；[成功 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38351595568)。

## TD-20261013-10：管理窗口在较小尺寸和错误提示下布局溢出

- 日期：2026-10-13
- 状态：已验证隔离 widget 回归、Linux 原生构建和部署；Windows 原生构建通过。
- 影响版本与环境：8aefcb6，共享 Flutter 管理窗口。
- 关联历史故障：TD-20261013-04 的界面功能；本条是新回归测试发现的尺寸边界。

### 症状与影响

启动初始化回归在 800×600 逻辑尺寸、引擎启动错误状态下发现侧栏底部溢出 6 像素，内容空态溢出 18 像素。应用最小高度为 580，因此实际小窗口也可能触发。只影响显示，未操作远程文件。

### 触发条件与复现

Flutter widget 测试在默认窗口尺寸启动客户端，缺失的隔离测试引擎产生错误提示。旧布局稳定报告两个 RenderFlex overflow。

### 根因与证据

侧栏将所有导航和退出项放在不可滚动 Column 内，总高度超过窗口；错误提示缩小内容区域后，空态的固定 Column 也超过可用高度。

### 解决办法与恢复操作

导航区域和内容空态使用滚动容器，保留连接状态和退出入口；限制错误文本区域高度并允许滚动。无本地数据恢复。

### 验证结果与边界

将 8aefcb6 的布局恢复后，最小高度/长错误提示测试重现 RenderFlex 溢出并失败；修复后全部 9 项 Flutter 回归和 analyzer 通过。不代表所有屏幕或字体缩放组合均已实测。

### 防复发措施与后续

保留最小窗口高度、长错误提示与全部导航入口可达的 widget 回归。

### 交付记录

[修复 b3ccfda](https://github.com/LumiaBlack51/twodrive/commit/b3ccfda5d792a6d24c5ddf5a0d449e5d005ed94d)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-09：Windows 原生托盘切换烟测失败

- 日期：2026-10-13
- 状态：已验证 Windows 原生后台启动及六次托盘消息切换。
- 影响版本与环境：8aefcb6，Windows 原生 CI，Flutter 3.41.8。
- 关联历史故障：TD-20261013-07；Linux 宿主双击机制已另行修复，本条尚不能归为相同根因。

### 症状与影响

原生桌面构建和隔离 IPC、文件、备份、自启动测试通过，GUI 烟测等待托盘切换超过三秒失败；尚未部署该 Windows GUI 到用户设备。

### 触发条件与复现

以独立临时状态启动 --background，向该进程窗口发送 Shell 通知消息，检查展开/收起。旧测试只报告 tray did not toggle，没有阶段或原生事件日志。

### 根因与证据

已确认源码中的初始化缺口：window_manager 0.5.1 的 Windows WaitUntilReadyToShow 才创建 taskbar_ COM 接口，ensureInitialized 只保存窗口句柄；应用未调用前者，showPanel 的 setSkipTaskbar 会解引用空 taskbar_。带诊断的旧代码复验确认 configure 与第一条 activate 已收到，随后进程以 3221225477（0xC0000005，访问违例）退出，尚未进入 native panel；与空接口路径一致。修复 b3ccfda 的 Windows 原生同一烟测通过，确认初始化遗漏对应首次激活访问违例。[失败任务](https://github.com/LumiaBlack51/twodrive/actions/runs/38347237158/job/115131358658)。需要区分托盘尚未就绪、Dart 回调异常、窗口操作和焦点引起的收起；启动后的异步 hide 与无条件窗口恢复调用也是待排除路径。

### 解决办法与恢复操作

补充原生事件追踪、就绪判断和失败阶段日志；在创建托盘和使用任务栏方法前调用 waitUntilReadyToShow。无数据恢复操作。

### 验证结果与边界

初始化顺序的 widget 回归在删除初始化步骤后失败，修复后通过；全部 9 项 Flutter 测试通过。带诊断的 [旧代码原生复验](https://github.com/LumiaBlack51/twodrive/actions/runs/38349552109/job/115138569071) 在第 1 次展开访问违例退出；修复后的 [Windows GUI 原生复验](https://github.com/LumiaBlack51/twodrive/actions/runs/38349916841/job/115139721893) 的托盘烟测步骤通过，覆盖后台隐藏、三轮展开/收起、稳定后可见性和位置。不把模拟 Shell 通知消息视为用户 Windows 桌面物理点击验证。

### 防复发措施与后续

保留后台启动、重复单击、稳定后可见性和位置断言，失败日志应指明阶段且不包含账号或凭据。

### 交付记录

[修复 b3ccfda](https://github.com/LumiaBlack51/twodrive/commit/b3ccfda5d792a6d24c5ddf5a0d449e5d005ed94d)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-08：Windows 专用备份分支未通过严格 Clippy

- 日期：2026-10-13
- 状态：已验证 Windows 原生严格 Clippy；GUI 故障另记 TD-20261013-09。
- 影响版本与环境：d168d34，Windows CI Rust 1.99，新增桌面引擎；Linux 条件编译不会检查此分支。
- 关联历史故障：TD-20261013-07 的 Windows 适配验证，不是网络故障。

### 症状与影响

Windows desktop/peer CI 在编译检查阶段退出，尚未进入新桌面实测。没有更新用户 Windows 服务。

### 触发条件与复现

Windows 原生 cargo clippy --all-targets -- -D warnings。

### 根因与证据

desktop_backup::is_link 的 Windows 条件块末尾多余 return 触发 needless_return；Linux 条件块没有此写法。[失败任务](https://github.com/LumiaBlack51/twodrive/actions/runs/38346820973/job/115130049741)。

### 解决办法与恢复操作

使用块尾表达式，保留重解析点检测语义和严格检查。无需数据恢复。

### 验证结果与边界

旧 Windows 检查失败；8aefcb6 的 Windows 原生 Clippy、Rust 和 IPC/备份步骤通过，随后 GUI 烟测失败另记 TD-20261013-09。[原生任务](https://github.com/LumiaBlack51/twodrive/actions/runs/38347237158/job/115131358658)。此变更是平台条件编译检查覆盖问题，没有虚构文件传输故障或额外单元测试。

### 防复发措施与后续

保留 Windows 原生桌面引擎 Clippy、IPC、备份和 GUI 烟测。

### 交付记录

[修复 8aefcb6](https://github.com/LumiaBlack51/twodrive/commit/8aefcb64cf8d9897889612eeee0946d3b32bbff1)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-07：托盘收起迟滞及前端平台依赖暴露

- 日期：2026-10-13
- 状态：已验证 Linux 原生宿主转发路径与 Windows CI 原生 GUI；用户 Windows 桌面 GUI 尚未实测。
- 影响版本与环境：未提交的 Development 桌面客户端，GNOME/Flutter；Windows 原生桌面 runner 尚未接入。
- 关联历史故障：TD-20261013-04/06，分别涉及功能入口和窗口锚点。

### 症状与影响

用户报告收起窗口等待较久，并将标题中的 Windows 理解成客户端的平台。Windows 实际是远程设备配置名。代码还存在 HOME、xdg-open、Linux-only engine 与托盘通道硬依赖，单有 Flutter 不等于已可在 Windows 运行。

### 触发条件与复现

实际桌面点击迟滞尚未稳定复现。D-Bus Activate 连续四次展开/收起，200ms 检查点时状态均已改变、位置固定。该结果不能替代桌面真实点击路径。

### 根因与证据

当前收起等待 Dart 窗口方法回调，失焦也仅由 Flutter 插件转发。不能据此断言这是用户迟滞的唯一原因。标题缺少“远程设备”语义；原生 runner 与引擎的 Linux 限制由源代码直接确认。

### 2026-10-13 根因补充

读取本机 Zorin AppIndicator 的 indicatorStatusIcon.js，确认 vfunc_button_press_event 对主键仅调用 _maybeHandleDoubleClick；只有点击计数为 2 才发 Activate。单击只有菜单路径，而当前客户端要求 GUI 面板。之前 D-Bus 人工激活跳过了这个真实故障点。因此 GNOME/X11 使用已启用的 GTK XEmbed 托盘接口，其他桌面保留 SNI。原生主键事件不再等待双击；没有修改系统扩展。初步独立扩展方案因新扩展无法即时加载而撤回，已清除尚未启用的本地目录。

### 解决办法与恢复操作

增加原生即时收起、失焦和 Escape 路径，避免托盘点击失焦后立即重新展开；明确设备名称。将共享 UI、路径和业务引擎接到 Windows runner，并通过能力字段区分平台挂载支持。不宣称未实现的系统挂载或其他平台已可用。

### 验证结果与边界

本机按 GNOME 实际转发方式发送 EnterNotify 与主键按下/释放，三轮打开 33–36ms、收起 3–6ms，位置与大小一致。此前仅人工 Activate 的测试未覆盖宿主双击机制，这一验证结论已更正。测试早期没有发送 EnterNotify，导致合成输入未触发 GTK；修正测试后通过。Windows CI 构建、IPC/备份测试和 GUI 烟测分别记录；不把 CI 当作用户 Windows 桌面实测。

### 防复发措施与后续

原生窗口可见性检查、Linux/Windows 构建和共享引擎测试。网络操作不得阻塞收起窗口。

### 交付记录

[修复 6752762](https://github.com/LumiaBlack51/twodrive/commit/6752762381eb0e1aedea54c16ca13333f147ec44)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-06：托盘状态面板随鼠标位置改变而漂移

- 日期：2026-10-13
- 状态：已验证本机位置计算与重复激活；多屏仅几何回归。
- 影响版本与环境：本次未提交的 Linux Flutter 桌面客户端，GNOME、2× 缩放。
- 关联历史故障：TD-20261013-04 解决托盘入口和尺寸；本条是位置锚点遗漏。

### 症状与影响

用户要求单击云朵图标后在其上方展开状态面板，不应随鼠标或管理窗口位置乱跑。当前代码在异步退出最大化、调整窗口后才获取鼠标位置。

### 触发条件与复现

托盘 Activate 已携带点击坐标，但 native panel 调用忽略该坐标，改用较晚的 gdk_device_get_position。点击后移动鼠标会影响定位；此前人工 D-Bus 激活也使用了与点击参数不同的位置。

### 根因与证据

StatusNotifierItem 的 Activate 坐标用于提示窗口位置；本机 GNOME 扩展发送事件坐标。代码没有保存该事件，而是在 Flutter 异步窗口操作结束后读取全局指针。此缺陷与已修复的窗口尺寸约束不同。

### 解决办法与恢复操作

在收到激活事件时保存锚点，小范围重复点击复用同一锚点；面板展开只读取锚点，优先放在图标上方并约束在工作区。没有收到有效坐标时才在事件接收时获取一次指针。安装程序只为本应用选择可定位的 X11 后端，保留用户显式覆盖。

### 验证结果与边界

新增 tray_geometry_test 覆盖同图标小范围点击、上下边缘、负坐标屏幕和工作区变化并通过。本机 2× 屏幕多轮位置与尺寸一致；旧代码使用指针位置得到 x=853，新代码按同一个激活锚点得到 x=980（逻辑像素），不再跟随指针。多屏和其他桌面环境未实测，不将几何模拟描述成真实多屏验证。 后续 Windows 适配曾把最小尺寸插件调用同时加到 Linux，复测发现 Mutter 后续配置会移位；现已将该调用限制到 Windows，Linux 保持 GTK 单一尺寸控制。重新验证时增加等待窗口管理器稳定后的坐标断言。

### 2026-10-13 坐标边界更正

后续发现 GNOME XEmbed 转发的根坐标实为图标内部中心；先前多次稳定不能证明面板位于真实图标附近。TD-20261013-12 修正真实锚点来源，TD-20261013-13 补上首映射后的最终位置校验。保留先前切换、尺寸和缓存锚点证据，不再把它们当作完整宿主定位验证。

### 防复发措施与后续

位置计算独立回归覆盖重复点击、边缘和屏幕变化；实际面板重复展开检查位置、尺寸和正常退出。

### 交付记录

[修复 8aefcb6](https://github.com/LumiaBlack51/twodrive/commit/8aefcb64cf8d9897889612eeee0946d3b32bbff1)；[桌面交付与验证](linux-dav-desktop.md)。
位置规范：[StatusNotifierItem](https://specifications.freedesktop.org/status-notifier-item/latest/status-notifier-item.html)。

## TD-20261013-05：真实 Windows 备份验证中出现一次 WebDAV 502

- 日期：2026-10-13
- 状态：调查中 / 根因未确认。
- 影响版本与环境：本次新 Linux 桌面引擎，连接既有 Windows 配对服务；备份开始后的目录读取出现一次 HTTP 502。
- 关联历史故障：TD-20261012-02/05 涉及 Windows 注册和 DNS；当前没有证据将这次目录请求失败归为同一机制。

### 症状与影响

测试的早期 WebDAV/FUSE 往返完成，随后备份等待中的 PROPFIND 返回 502，严格测试终止。finally 清理未报告失败，测试目录被移除；原始配对状态保留。

### 触发条件与复现

备份子进程与主进程共用已配对的本地 WebDAV 网关；首次发生时没有保留网关的详细错误类别，只保留了脱敏 HTTP 状态。需要保留类别后重跑验证。

### 根因与证据

目前只有 HTTP 502 证据，不足以区分暂时连接失败与并发请求缺陷。不能因为昨天存在 DNS 故障就假定根因相同。

### 解决办法与恢复操作

先完善测试失败证据，安全的只读目录请求允许界面用户重试；不自动重放写请求。重新连接进行隔离测试，保留远端清理结果。

### 验证结果与边界

调查中；不能把此前本地备份通过视为真实 Windows 备份通过。

### 防复发措施与后续

测试需保留脱敏网络诊断、实际失败阶段、清理状态。后续重跑成功只说明该轮通过，不证明间歇性网络错误永久消失。

### 2026-10-13 验证补充

已确认并修复的是读取缺少有限重试的机制：新增 `transient_read_is_retried_but_mutation_is_never_replayed` 在原实现稳定失败，修复后通过，并断言 PUT 只发出一次。真实 Windows 后续全流程通过，含备份创建/更新、暂停恢复、冲突保护与远端清理。增加网关脱敏类别日志。初次 502 的深层网络根因仍未确认，不承诺所有网络故障消失。

### 交付记录

[修复 6752762](https://github.com/LumiaBlack51/twodrive/commit/6752762381eb0e1aedea54c16ca13333f147ec44)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-04：桌面客户端代理策略不可选，托盘交互和备份入口缺失

- 日期：2026-10-13
- 状态：已验证本机原生部署、实际 Windows WebDAV 连接与隔离功能回归。
- 影响版本与环境：本次尚未提交的新 Linux 桌面前端；不涉及稳定版 OneDrive 守护进程。
- 关联历史故障：TD-20261013-03 是退出路径缺陷；本条记录不同的产品功能和托盘点击机制。

### 症状与影响

用户报告产品被命名为 DAV、网络策略只有固定说明、托盘点击没有快捷状态面板、缺少备份文件夹选择，图标仅显示对勾。固定界面不应使用特定网络环境名称。

### 触发条件与复现

检查已安装前端并对照用户提供的 OneDrive 截图。既有 AppIndicator 以菜单为主要操作，点击回调并不保证触发 Flutter 状态窗口；配置中没有网络路由字段。

### 根因与证据

前端使用协议名作为产品名，连接子进程未传递可选的 network-route；GUI 没有文件夹备份的配置与执行路径。旧托盘图标选用单独的状态 emblem，没有产品云朵主体。

### 解决办法与恢复操作

统一 TwoDrive Development 品牌；新增每连接的三种网络策略，原生 StatusNotifierItem 激活 Flutter 状态面板、云朵角标、账号与真实传输状态；加入上传式文件夹备份、持久版本记录、暂停和冲突保护。仅安装独立 Development 客户端，不更改稳定 OneDrive 配置。

### 验证结果与边界

结果见下方验证补充及桌面验证报告。新界面只显示已实现的 TwoDrive 和 WebDAV 账号，不宣称已支持 Google Drive 或新增 OneDrive 登录。文件夹备份不传播本地删除。

### 防复发措施与后续

增加状态面板、网络选择持久化、文件夹备份创建/更新/暂停与符号链接保护测试。禁止用“已连接”替代“已备份”的证据。原生检查发现主窗口最大化状态使 setSize 无效，托盘打开前须退出最大化并等待窗口状态更新；另修正 GLib 废弃 flags 构建警告。

### 2026-10-13 验证补充

7 项 Flutter 测试通过，包含代理选择保存、托盘真实进度/暂停、只读禁用和跨连接任务拒绝；原生 Linux release 构建与分析通过。三种策略逐一连接实际 Windows，分别报告物理网卡且禁止后备、系统路由、物理优先允许报告后备。备份的本地和 Windows 合成目录测试通过。原生面板使用 GTK 持久尺寸约束，避免异步 resize 后立刻锁定旧尺寸；390×570 逻辑像素在 2× 屏幕对应 780×1140。补充编译修正包括 GLib flags、严格 Clippy 嵌套条件及范围写法，不涉及实际用户数据。

### 交付记录

[修复 6752762](https://github.com/LumiaBlack51/twodrive/commit/6752762381eb0e1aedea54c16ca13333f147ec44)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-03：Linux Flutter 窗口退出触发图形上下文断言

- 日期：2026-10-13
- 状态：已验证本机 GTK 应用退出路径修复
- 影响版本与环境：新桌面前端，Flutter 3.41.8、Zorin/GNOME 的 X11 窗口，调用 window_manager 0.5.1 的 destroy。
- 关联历史故障：无相同触发条件的历史记录。

### 症状与影响

通过已安装客户端的托盘“退出”调用正常业务清理后，Flutter 进程退出码为 134。日志包含 `The implicit view cannot be removed`、无法取得 OpenGL 上下文及 libepoxy 断言。FUSE 已卸载，配对网关已退出，后续 Windows 测试可重新取得配对状态锁；没有发现文件丢失。

### 触发条件与复现

实际部署窗口通过 D-Bus 托盘菜单退出；进程 32837 报 Aborted。仅记录临时进程编号用于本次证据对应。

### 根因与证据

window_manager 的 Linux destroy 直接调用 gtk_window_close。Flutter 自带 SystemNavigator.pop 的退出路径先解除窗口的 application 引用再退出应用。当前怀疑显式关闭 GTK 窗口的销毁顺序触发上下文错误，待替代路径实测；不推断所有 GPU 都有相同表现。

### 解决办法与恢复操作

拟在引擎完成卸载、停止子进程后使用 Flutter 的应用退出 API。无需恢复配对或远端文件；本轮异常产生的临时日志和崩溃产物将在验证后定点清理。

### 验证结果与边界

原退出路径实机失败；修复验证待补充。不能以窗口消失代替进程退出码正常。

### 防复发措施与后续

保留原生启动→连接→托盘退出的实机检查，要求退出码 0、子进程退出、挂载解除。

### 2026-10-13 验证补充

使用 SystemNavigator.pop 后，同一 GNOME/X11 环境经应用 D-Bus quit 动作退出，多轮进程退出码均为 0，未再输出原先的 implicit-view/OpenGL 断言。此前窗口销毁路径的退出码为 134。确认修复的是当前 Flutter/GTK 退出顺序；不外推所有 GPU/Wayland 环境。

### 交付记录

[修复 6752762](https://github.com/LumiaBlack51/twodrive/commit/6752762381eb0e1aedea54c16ca13333f147ec44)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-02：DAV 前端的连接配置与失败传输缺少目标绑定

- 日期：2026-10-13
- 状态：已验证连接目标绑定修复
- 影响版本与环境：尚未提交的新 Linux DAV 前端；当前实机只有一个测试连接，未观察到跨服务器写入。
- 关联历史故障：TD-20260916-UI 的断连禁用测试属于相邻界面约束；本次是多连接切换后的目标归属。

### 症状与影响

审查发现，连接时仍可添加另一个配置并切换界面选中项，但引擎连接仍指向原服务器。失败传输的重试也只保存文件路径，切换服务器后可能把旧任务用于新服务器。

### 触发条件与复现

计划验证：连接中禁止改变配置选择；A 服务器失败的上传，在连接 B 后不可重试；过期文件对话框提交时仍须核对原连接。

### 根因与证据

当前代码中选中配置和活动连接分别保存，传输参数没有活动配置 ID。回归测试结果待补充。

### 解决办法与恢复操作

拟在前端任务、目录项及引擎文件请求中绑定配置 ID；编辑配置要求先断开，跨连接重试拒绝。没有用户文件恢复操作。

### 验证结果与边界

调查中。之前的单连接 Windows 往返不能覆盖此多连接场景。

### 防复发措施与后续

增加前端旧任务拒绝与后端配置修改拒绝测试；配置身份检查必须先于任何远程文件请求。

### 2026-10-13 验证补充

原前端回归实际记录到了对另一个连接调用 upload/list，测试失败；增加 profile ID/revision 绑定后相同测试通过。引擎 IPC 拒绝连接中编辑配置、错误 profile、过期 revision；真实 Windows 往返同时通过。传输重试和弹窗完成也核对原连接配置版本。

### 交付记录

[修复 6752762](https://github.com/LumiaBlack51/twodrive/commit/6752762381eb0e1aedea54c16ca13333f147ec44)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261013-01：新 DAV 桌面客户端的挂载就绪检测缺少既有挂载检查

- 日期：2026-10-13
- 状态：已验证挂载所有权修复
- 影响版本与环境：尚未提交的新 Linux DAV 桌面引擎；现有稳定 OneDrive 服务未修改。
- 关联历史故障：TD-20261006-03 的独立 DAV 挂载状态约束；本次是桌面进程管理的新路径。

### 症状与影响

代码审查发现：启动挂载子进程后只根据 `mountpoint` 判断就绪，没有先排除目标已被其他进程挂载的情况。可能误认他人的挂载为自己的挂载，并在断开时尝试卸载。尚未观察到真实用户挂载被影响。

### 触发条件与复现

计划使用两个独立的临时桌面引擎与同一个测试 FUSE 目录验证；第二个引擎必须拒绝接管已有挂载，退出后第一个挂载仍可访问。

### 根因与证据

当前证据为源码中就绪检查缺少前置所有权检查；实测结果待补充。不能仅以子进程曾启动证明挂载属于该进程。

### 解决办法与恢复操作

拟增加启动前的已有挂载拒绝、取消和失败时的子进程/挂载清理，并覆盖已退出子进程的卸载恢复。所有复现只使用临时测试目录。

### 验证结果与边界

调查中。先前本地与 Windows 正常挂载读写通过，但没有覆盖既有挂载冲突，不能作为该路径已安全的证据。

### 防复发措施与后续

增加双引擎的挂载所有权回归；保留未上传缓存，不接管或卸载无关挂载。

### 2026-10-13 验证补充

两个真实隔离 FUSE 客户端的回归在旧引擎触发 unsafe operation unexpectedly succeeded；修复后第二客户端拒绝接管已有挂载，退出后原挂载仍可读。还验证正常卸载、只释放已同步缓存、暂停中待上传写入阻止卸载。稳定 OneDrive 服务未受影响。

### 交付记录

[修复 6752762](https://github.com/LumiaBlack51/twodrive/commit/6752762381eb0e1aedea54c16ca13333f147ec44)；[桌面交付与验证](linux-dav-desktop.md)。

## TD-20261012-08：系统 DNS 后备预算与中继外层超时相同

- 日期：2026-10-12
- 状态：已验证；两平台原生回归及当前双端 TUN 实测通过。
- 影响版本与环境：至 772b3ce；raw DNS 完整耗尽三秒预算时。
- 关联历史故障：TD-20261012-05 的新后备实现；不是对本机 raw DNS 深层失败原因的推断。

### 症状与影响

raw DNS 若持续无响应，外层可能在原生后备运行前取消调用；快失败回归不会覆盖该条件。尚未把此候选传到 Windows。

### 触发条件与复现

检查锁定 iroh-relay 1.3.0：client/tls.rs 使用 defaults.rs 的三秒 DNS_TIMEOUT；新后备先给 raw DNS 三秒，再尝试原生解析。

### 根因与证据

外层总预算等于第一步预算，两步串行缺少后备时间。已确认源码预算关系，尚未以此解释原实机失败。

### 解决办法与恢复操作

raw 查询上限改为 500 ms，原生调用上限两秒，总计至多约 2.5 秒；增加 raw 不响应时实际执行后备的回归。无需实机恢复操作。

### 验证结果与边界

1a48437 的原生 CI 38256439762 两平台通过 hanging_raw_dns_leaves_time_for_native_before_relay_deadline：旧三秒 raw 步骤无法在一秒窗口完成；新后备在同一窗口完成有效解析。另通过快失败/空结果原生 TUN 地址、原生 localhost、超时调用持有并发槽三项回归。同一原生产物的双端默认、Linux 兜底、Windows 兜底三阶段均完成 2 MiB 完整哈希、MOVE、256 KiB Range、DELETE 404；两端路由未变化。

### 防复发措施与后续

后备链总预算须小于中继调用预算，保留快失败和耗尽上游预算两种触发条件。

### 交付记录

[修复 1a48437](https://github.com/LumiaBlack51/twodrive/commit/1a484376ba4cc5a45ce8c9909832482245974eac)；[两平台原生 CI 38256439762](https://github.com/LumiaBlack51/twodrive/actions/runs/38256439762)；[脱敏实机验证与清理记录](peer-windows-egress-2026-10-12.json)。

## TD-20261012-07：系统 DNS 候选被严格 Clippy 的嵌套条件检查阻止

- 日期：2026-10-12
- 状态：已验证；两平台严格 Clippy 及回归通过。
- 影响版本与环境：47837e3；CI 38255820778，两平台严格 Clippy。
- 关联历史故障：TD-20261012-06 修正后的另一项构建检查；不改变实机 DNS 根因。

### 症状与影响

构建门禁失败，候选未传送到 Windows，原服务和 agent 未受影响。

### 触发条件与复现

将 `if let Ok(values)` 与 `if !values.is_empty()` 嵌套，运行 `clippy -D warnings`。

### 根因与证据

CI 明确报告 system_dns.rs:53 的 `collapsible_if`；是源码结构未符合严格检查，没有实机网络行为证据。

### 解决办法与恢复操作

合并为 Rust 2024 let 条件链，保持非空结果行为。无需实机恢复操作。

### 验证结果与边界

47837e3 旧候选两平台失败已确认；772b3ce 的原生 CI 38256039757 与 1a48437 的原生 CI 38256439762 两平台全部通过，包含严格 Clippy。

### 防复发措施与后续

继续执行严格检查，通过完整原生 CI 后才进行候选传送。

### 交付记录

[失败 CI 38255820778](https://github.com/LumiaBlack51/twodrive/actions/runs/38255820778)；[修正 772b3ce](https://github.com/LumiaBlack51/twodrive/commit/772b3ce7c635425cd53b985e409c60c6a1c3c010)；[两平台原生 CI 38256439762](https://github.com/LumiaBlack51/twodrive/actions/runs/38256439762)。

## TD-20261012-06：系统 DNS 候选合并了两个不同的 opaque 迭代器类型

- 日期：2026-10-12
- 状态：已验证修正；两平台原生构建通过。
- 影响版本与环境：9b93c88，CI 38255613648 的 Linux Clippy 编译。
- 关联历史故障：TD-20261012-03 同属候选 API 使用失误，但本次是两个 `impl Iterator` 的类型不相同；不是 Windows 实机网络失败。

### 症状与影响

E0308 阻止候选编译，未交付或启动失败构建；原服务与 agent 保持可用。

### 触发条件与复现

if/else 直接合并 lookup_ipv4 和 lookup_ipv6 的返回结果。

### 根因与证据

锁定依赖的两方法返回各自独立的 `impl Iterator<Item=IpAddr>`，元素相同不代表 opaque 类型相同。原生 CI 在 system_dns.rs:45 确认 incompatible types。

### 解决办法与恢复操作

在每个分支内将结果收集为同一个 Vec 类型，再合并。无需实机恢复操作。

### 验证结果与边界

9b93c88 原生编译失败已确认。修正 opaque 类型后另发现严格 Clippy 条件结构问题（TD-20261012-07）；772b3ce 的 CI 38256039757、1a48437 的 CI 38256439762 两平台全部通过。本条不改变 TD-20261012-05 的实机验证状态。

### 防复发措施与后续

两平台严格 Clippy 和系统 DNS 回归通过后才传送候选；核对 opaque 返回类型，避免只比较元素类型。

### 交付记录

[失败 CI 38255613648](https://github.com/LumiaBlack51/twodrive/actions/runs/38255613648)；[类型修正 47837e3](https://github.com/LumiaBlack51/twodrive/commit/47837e363f5c9883f391862d57221755da5a2926)；[两平台原生 CI 38256439762](https://github.com/LumiaBlack51/twodrive/actions/runs/38256439762)。

## TD-20261012-05：Windows TUN 兜底没有使用可用的原生系统地址解析

- 日期：2026-10-12
- 状态：已验证修复；当前两端 TUN 实机通过，raw DNS 上游更深的失败原因仍未确认。
- 影响版本与环境：至 55b80ad；Windows 校园网 TUN，System 路由中继注册失败。
- 关联历史故障：TD-20261012-02 的持续调查；TD-20261011-06 是物理 DNS 并发答案选择，本条为另一机制；TD-20261012-04 的系统代理配置缺口不是本机本次失败的原因。

### 症状与影响

Windows 兜底约 32 秒注册超时，不能完成文件探针；默认物理路由和 Linux 兜底此前实测通过，原 serve 和配对状态仍可用。

### 触发条件与复现

签名 agent 执行经原生 CI 验证并全量校验 SHA256 的 55b80ad 候选，指定不存在物理接口。自动进入 System 路由后失败，失败 child 已自动停止并清理。

### 根因与证据

同一公共中继名字，endpoint 原解析器记录 `aps_dns_a=error=[dns,other]`、`aps_dns_aaaa=records=0`、`home_relays=0`；有界 Windows 原生地址解析返回 `records=1 ipv4=1 fake_ip=1`。System 分支没有原生地址解析后备，无法使用 TUN 提供的有效 fake-IP。原 raw DNS 上游更深的失败原因仍未确认；2026-10-12 后续实测：加入原生后备的 1a48437 已恢复中继注册及文件访问；同版本完整三阶段再次通过。系统代理读取成功但 manual/usable/pac/autodetect 均 false，排除本次候选因遗漏静态 HTTP 代理而失败。

### 解决办法与恢复操作

在 System 路由中对 A/AAAA 错误或空结果使用有界原生 OS 解析，限制等待预算（raw 500 ms、原生两秒）及未完成原生调用数量（八个，reset 共享上限）；只在已经明确提示的 System/TUN 兜底接受 fake-IP。物理 DNS 和物理 TCP 仍拒绝 fake-IP，TXT 保持原解析路径。不修改全局代理、DNS 或路由。运行恢复单独说明：agent 和原 serve 保持运行，候选通过旧 TwoDrive 传送。

### 验证结果与边界

55b80ad 的原生 CI 38253177181 两平台通过，候选传送 PUT 204（279.899 秒），签名快照确认私有执行副本 SHA256；实际 Windows 兜底仍失败。1a48437 两平台完整原生 CI 通过四项系统 DNS 回归，Linux 27/Windows 22 单元、10/9 集成、各两项控制协议测试、原生 release、LAN 和签名双通道实传 smoke，以及 Windows PowerShell 5.1/7 回归。Windows 程序实际经旧 TwoDrive PUT 204 传送（312.001 秒），签名快照校验私有执行副本全量 SHA256。Windows 兜底先独立通过，再以同版两端完整复测：默认物理 TCP 为 Linux 14/14、Windows 16/16；Linux 兜底 65 个非物理 TCP 样本；Windows 兜底 30 个非物理 TCP 样本。三阶段全部完成 2 MiB 哈希、MOVE、Range 256 KiB 和 DELETE 404，TUN 存在、全局路由未变。默认 APS RTT 90.5 ms，Linux 兜底 APS 1003.6 ms，Windows 兜底 US-west 739.8 ms。当前两端校园网，不称为独立 NAT 验证；此前热点/校园网默认验证另保留在前一日记录。

### 防复发措施与后续

保持物理路径拒绝 fake-IP 的回归；验证修复后的实际 Windows 兜底，再用同一版本复测两端默认及两端兜底。已收集 bootstrap 清理回执：agent 退出 0，原身份/授权保留、私有 runtime 删除、程序负载删除均 true。公共包删除确认 404，Windows 共享根目录零项、原 serve 仍可用。本机自有 gateway 已退出，临时二进制、ZIP、负载、私密日志、临时客户端状态均已删除，原配对状态保留；稳定 daemon active、NRestarts=0。PAC/WPAD/SOCKS 和强制 VPN/WFP 仍有实现/验证边界，不将 TCP 采样当作所有 UDP 流量绕过保证。

### 交付记录

[已核实 dev 推进提交 343b4fa](https://github.com/LumiaBlack51/twodrive/commit/343b4fa93ae9b3cd639f63e4edc4ebcc733143b3)。2026-10-13 补齐：该提交的 [WebDAV/QUIC](https://github.com/LumiaBlack51/twodrive/actions/runs/38258670629)、[Peer 原生](https://github.com/LumiaBlack51/twodrive/actions/runs/38258670478)与 [Windows 双预览](https://github.com/LumiaBlack51/twodrive/actions/runs/38258670638)均已成功完成，未因本地会话中断而取消。

[诊断候选 55b80ad](https://github.com/LumiaBlack51/twodrive/commit/55b80ad649cad7447e9e1a904b602d035d55bd88)；[原生 CI 38253177181](https://github.com/LumiaBlack51/twodrive/actions/runs/38253177181)；[修复 1a48437](https://github.com/LumiaBlack51/twodrive/commit/1a484376ba4cc5a45ce8c9909832482245974eac)；[两平台原生 CI 38256439762](https://github.com/LumiaBlack51/twodrive/actions/runs/38256439762)；[脱敏实机验证与清理记录](peer-windows-egress-2026-10-12.json)。

## TD-20261012-04：Windows 系统代理设置没有进入自动兜底配置

- 日期：2026-10-12
- 状态：已验证读取与原生回归；启用静态系统代理的实际传输未覆盖。
- 影响版本与环境：至 23ab327 的 System 路由仅调用 iroh proxy_from_env；Windows 用户配置系统 HTTP 代理、没有代理环境变量时。
- 关联历史故障：TD-20261012-02 的 DNS/注册超时调查；当前实机是否具备系统 HTTP 代理、是否为这次超时的直接原因仍待确认。

### 症状与影响

Windows 配置的系统 HTTP 代理不会被上述环境变量读取函数发现，直连失败后可能仍走不能连通的系统裸连接。实机 agent 报告代理环境变量不存在；诊断候选 A 查询失败、AAAA 无记录、home_relays 0，尚未形成完整 DNS 根因结论。

### 触发条件与复现

当前 Windows 用户设置静态 HTTP(S) 系统代理、不设置 HTTP_PROXY 等环境变量；auto 触发 System 兜底。旧代码没有读取当前用户系统代理的调用。

### 根因与证据

network::configure 的 System 分支只调用 builder.proxy_from_env；锁定 iroh 1.3.0 依赖源确认该函数只读取 HTTP(S) 环境变量，不读取 Windows 用户代理设置。此处是能力遗漏，不能据此推断当前实机设置的具体值。

### 解决办法与恢复操作

环境变量优先；没有有效环境代理时读取 Windows 当前用户静态 HTTP(S) 代理并应用于本进程。保留明确兜底提示，日志只输出来源/存在性，不输出地址、凭据或 PAC 内容。使用 [Windows 官方读取 API](https://learn.microsoft.com/en-us/windows/win32/api/winhttp/nf-winhttp-winhttpgetieproxyconfigforcurrentuser)，释放它返回的非空字符串；不修改系统代理或路由，并保留针对中继目标的静态绕过规则。PAC/WPAD/SOCKS 尚未自动处理。

### 验证结果与边界

55b80ad 的两平台原生 CI 38253177181 通过，包括环境变量优先、CGI 限制、静态协议代理解析、绕过规则与非法 URL 回归。实机 API 读取成功（read_error=None），manual/usable/bypass/pac/autodetect 均 false；此配置缺口不是本机这次注册失败的原因，原生系统 DNS 对照见 TD-20261012-05。未实测启用静态 HTTP 代理、PAC/WPAD/SOCKS。

### 防复发措施与后续

覆盖单代理、按协议配置、IPv6、拒绝 SOCKS/非法 URL、原配置不改和环境变量优先。继续取得精确 DNS 类别及原生系统解析的对照，验证默认直连与兜底后再推进 dev。

### 交付记录

[修复 55b80ad](https://github.com/LumiaBlack51/twodrive/commit/55b80ad649cad7447e9e1a904b602d035d55bd88)；[两平台原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38253177181)。

## TD-20261012-03：注册诊断候选使用了不完整的依赖 API 签名

- 日期：2026-10-12
- 状态：已验证；两平台原生构建及回归通过。
- 影响版本与环境：516dda9 诊断候选；CI 38249950281 的 Linux Clippy 构建失败。
- 关联历史故障：TD-20261012-02 的候选诊断开发；不是 Windows 网络超时的根因。

### 症状与影响

候选没有完成编译，不能传到 Windows 执行。已上线 agent 与旧 serve 未受影响。

### 触发条件与复现

编译新 registration_diagnostics；其 lookup_ipv4/lookup_ipv6 调用只传入 hostname。

### 根因与证据

当前锁定 iroh-dns 1.3.0 的这两项 API 还要求 Duration 参数，返回 IpAddr 迭代器；候选代码遗漏预算参数，并按 Ipv4Addr 使用 octets。检查已下载依赖源确认签名，Linux CI 构建失败。候选未交付，保留已通过验证的旧产物。

### 解决办法与恢复操作

传入显式三秒预算；通过 IpAddr::V4 匹配统计 fake-IP。没有需要恢复的实机候选。

### 验证结果与边界

CI 38249950281 确认 E0061（缺少预算参数）与 E0599（IpAddr 没有 octets）旧候选构建失败；修复提交 23ab327 的 CI 38250184130 两平台全部通过，含严格 Clippy、格式、既有网络/控制测试、新增诊断脱敏回归及实际双通道 2 MiB 探针。实机注册失败诊断已取回并定位 System 原生地址后备缺口，修复后的校园网 TUN 三阶段实测见 TD-20261012-05；不把原生同机测试当作校园网兜底成功。

### 防复发措施与后续

先核对锁定依赖的签名和类型，再执行两平台严格 Clippy、现有网络/控制回归及新的诊断脱敏回归；通过后交付实机。

### 交付记录

[失败候选 516dda9](https://github.com/LumiaBlack51/twodrive/commit/516dda9)；[修复 23ab327](https://github.com/LumiaBlack51/twodrive/commit/23ab32734dcf5a149d8d99ac6258aae471afc4d4)；[两平台原生 CI 38250184130](https://github.com/LumiaBlack51/twodrive/actions/runs/38250184130)。

## TD-20261012-02：Windows 系统/代理兜底再次因中继注册超时退出

- 日期：2026-10-12
- 状态：已验证原生 DNS 后备修复；raw DNS 上游更深失败原因仍未确认。
- 影响版本与环境：3dddc00 原生 Windows 核心，f1657e5 编排；两端校园网，FlClash/TUN 存在。
- 关联历史故障：TD-20261011-10；旧记录因日志丢失没有根因结论，本轮通过 TD-20261011-11 控制端保留了签名错误证据。

### 症状与影响

双端 auto 和 Linux 强制缺失网卡兜底均完成 2 MiB 哈希、MOVE、256 KiB Range、DELETE 404；Windows 缺失网卡触发系统/代理兜底后，在约 31 秒退出，无法完成该阶段数据验证。旧控制传输和 agent 保持可用，dev 没有推进。

### 触发条件与复现

使用独立临时身份启动 Windows auto child，指定固定不存在接口。签名快照记录了明确 fallback 提示及 `Error: relay registration timed out; check outbound connectivity or use --no-relay for LAN testing`，退出码 1。

### 根因与证据

已确认 endpoint.online 的 30 秒预算触发退出。尚未确认系统 DNS、代理环境、中继选择、TCP/TLS 或其他网络策略中的失败环节，不把预算到期本身当作完整根因。默认 Windows 物理出口实际 14/14、30/30 TCP 样本为物理源，路由未变、fake-IP 存在；失败兜底阶段无已建立公网 443 TCP 样本。

2026-10-12 追加：55b80ad 对照确认 endpoint raw A 失败而原生 Windows DNS 可返回 TUN fake-IP；系统静态代理未配置。已定位 System 解析后备缺口，修复及三阶段实测、两端清理结果见 TD-20261012-05。原 raw DNS 深层错误仍不作猜测，旧 TD-20261011-10 丢失日志不能追溯认定同一根因。

### 解决办法与恢复操作

调查中。固定 aps1 relay 的诊断重试仍在约 31 秒退出，不能据此把故障只归因于默认 relay 列表。增加注册失败后的有限错误类别/OS 错误码、home relay 状态及相同 resolver 的 A/AAAA 计数诊断，不输出上游任意错误文本或代理 URL。23ab327 候选通过原生 CI 后实际上传成功（289.013 秒），签名快照确认私有程序全量 SHA256 与原生产物一致；实际 home_relays=0、aps_dns_a=error=[dns,other]、aps_dns_aaaa=records=0。下一候选区分 NXDOMAIN/无可用响应，并增加有界原生 OS 解析对照；Windows 静态代理读取缺口另记 TD-20261012-04。编排已自动收集签名 stderr 并停止、删除失败 child 私有 runtime；保留 agent 与原 serve，用于远程重试和传送经过原生验证的候选构建。上述为调查阶段保留措施；后续完整验证通过后已发送 finish、取得正常退出/身份保留回执并完成双端清理。

### 验证结果与边界

Windows/Linux CI 38248783869 全部通过，包括新版实际传输探针；这不覆盖校园网 TUN 中的 Windows 系统出口失败。旧 TD-20261011-10 的具体退出机制无法追溯确认，保留其原结论。本轮同在校园网，不称为独立 NAT 验证。

### 防复发措施与后续

增加系统出口故障的脱敏诊断，取得对应网络错误后补充有意义的回归和真实 Windows 兜底验证。agent 有绝对租期，修复和清理须在租期内完成或记录未完成范围。

### 交付记录

[实机编排 f1657e5](https://github.com/LumiaBlack51/twodrive/commit/f1657e5070be4d0568a950177bab641d40209e8d)；[两平台原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38248783869)；[修复 1a48437](https://github.com/LumiaBlack51/twodrive/commit/1a484376ba4cc5a45ce8c9909832482245974eac)；[脱敏实机验证与清理记录](peer-windows-egress-2026-10-12.json)。

## TD-20261012-01：实机控制编排只保存在临时目录，重启后不能直接续测

- 日期：2026-10-12
- 状态：入口恢复、原生回归及双端默认/Linux 兜底实测通过；Windows 兜底另记 TD-20261012-02。
- 影响版本与环境：902c9e5 验证分支；Linux 于本次续测前重启。
- 关联历史故障：TD-20261011-11 的自主诊断工具；不是 Windows 兜底退出根因的新结论。

### 症状与影响

续测时，旧的 gateway、watcher 和实机编排脚本均不在；没有收集到 Windows 实机 agent 结果，不能据此推进 dev。原 Linux 配对状态仍在，稳定 daemon active、NRestarts 0。

### 触发条件与复现

上一轮实机 watcher 和编排只放在 /tmp；本机重启后该自有目录消失。仓库中的签名控制客户端和原生构建记录仍在。

### 根因与证据

临时运行数据的生命周期短于跨天维护任务。控制协议、原生 CI 和交付信息已持久保存，但组合实机验证入口没有保存到仓库。恢复旧配对连接成功；Windows response.json 与 bootstrap-cleanup.json 均为 404，启动脚本和程序 HEAD 为 200，脚本逐字节与仓库版本一致。恢复时尚无首次启用成功证据，不能把缺少回执归因为 agent 崩溃；后续签名就绪和正常退出已确认，完整结果见 TD-20261012-05。

### 解决办法与恢复操作

将 experiments/twodrive-dev/scripts/verify-live-test-agent.py 保存为实机编排入口，使用独立临时 child/client 身份和已签名控制指令；阶段结果原子保存，失败时保留已收集诊断、回收 child，控制端继续支持远程重试。本地恢复仅下载核对已通过 CI 的 Linux 产物并重启自有 loopback gateway，没有修改配对或稳定服务。

### 验证结果与边界

Linux 原生产物归档及二进制 SHA256 与 CI 38143055331 记录一致。旧配对实连 US-west relay、初始 RTT 782.8 ms。当前本机为校园网，不将本轮称为手机热点与校园网跨 NAT。本机实际 QUIC 控制双通道 smoke 已执行新版实机入口使用的完整 2 MiB 哈希、MOVE、256 KiB Range、DELETE 404 探针，原服务继续可用、临时 child 和 client 清理、原身份授权字节保留通过；两个既有签名响应回归通过。后续 Windows 首次启用成功、双端默认及 Linux 兜底实测通过；Windows 再次超时另记 TD-20261012-02。2026-10-12 后续完整三阶段均通过，Windows bootstrap 身份授权保留回执与双端自有临时文件清理确认完成，见 TD-20261012-05。

### 防复发措施与后续

实机编排入口纳入版本控制；阶段结果及时保存到指定报告，继续使用已有签名核验，不把过期响应当作在线状态。完成真实双端验证及清理后再推进 dev。

### 交付记录

[入口 f1657e5](https://github.com/LumiaBlack51/twodrive/commit/f1657e5070be4d0568a950177bab641d40209e8d)；[已核实 dev 推进提交 343b4fa](https://github.com/LumiaBlack51/twodrive/commit/343b4fa93ae9b3cd639f63e4edc4ebcc733143b3)；完整实机结果见 TD-20261012-05。

## TD-20261011-17：临时交付恢复助手按大小写查找 HTTP 头失败

- 日期：2026-10-11
- 状态：临时工具恢复已验证。
- 影响版本与环境：临时 restore-agent-delivery.py；不影响原生 TwoDrive 协议。
- 关联历史故障：TD-20261011-16 的恢复步骤；不是文件写入失败。

### 症状与影响

重新 PUT exe 返回 201，随后 HEAD 返回 200，但临时助手查找 dict['Content-Length'] 抛 KeyError，未继续发送启动脚本。

### 触发条件与复现

urllib HTTPMessage 转为普通 dict 后，HTTP 头的大小写无关语义丢失，助手以固定大小写索引。

### 根因与证据

KeyError 发生在本机恢复脚本第 15 行，写入已成功。需按无关大小写读取，并核对真实响应，不能为恢复检查异常盲目重新 PUT。

### 解决办法与恢复操作

将头字段键统一 lower，先核对已存在的程序 HEAD/元数据，再继续发送尚未存在的小文件。已有 exe 保留。

### 验证结果与边界

实际响应字段为小写 content-length，值 16131072；统一键大小写后 HEAD、前缀与小脚本完整回读通过，交付继续完成。临时助手旧 headers[Content-Length] 在该响应上 KeyError，统一 lower 后读取正确。没有重复写 exe，不把 HEAD 大小或前缀说成完整哈希证明。

### 防复发措施与后续

记录写入成功与后续检查状态，HTTP 头查找保留大小写无关语义。

### 交付记录

临时工具恢复，无产品修复提交。

## TD-20261011-16：交付恢复未先核对完成标志，误清已校验的自有控制包

- 日期：2026-10-11
- 状态：恢复交付及签名实机启动已确认；最终双端验证和清理通过。
- 影响版本与环境：本次临时 Python 操作流程；Windows 原程序、身份/授权未改变。
- 关联历史故障：TD-20261011-15 的并发 502 观察。不是已确认的持久用户数据丢失。

### 症状与影响

执行恢复删除自有 exe/script/manifest 后才核对交付结果，发现结果 complete=true，exe 全量回读 SHA256 已匹配、用时 348.569 秒。有效临时控制包被误清，首次启用推迟。

### 触发条件与复现

长耗时 exe 回读期间独立 PROPFIND 返回 502。恢复据此假定原交付未完成，没有在 DELETE 前重新检查完成标志/文件状态。

### 根因与证据

操作判断将辅助请求失败等同于主传输未完成。已保存结果明确三项负载完成、exe 与脚本 SHA256 一致；目录清理只涉及本次自有控制包。

### 解决办法与恢复操作

重新送同一目录，逐项检查 PUT/HEAD 与小文件回读；执行前 bootstrap 核对 Windows 原生程序 SHA256，启用后再核对服务端签名的实际运行 SHA。不把 HEAD 大小说成全量回读证明。

本地恢复：本机 gateway 已重连，已配对 state 保留；Windows 原 serve 和 state 不重启、不删除。重复传输程序是恢复操作。

### 验证结果与边界

旧交付原本全量回读通过。恢复重新 PUT 201、HEAD 16131072 字节及 1024 字节前缀匹配；启动脚本完整回读 SHA256 匹配，manifest 成功写入，同一目录恢复完成。恢复轮未再次全量回读 exe；bootstrap 将核对 Windows 私有执行副本 SHA，签名 ready 再核对实际运行 SHA。实机启动待执行，没有修改原 Windows 私密 state。

### 防复发措施与后续

不自动重放状态不明的写入；发生传输失败先读取结果/核对自有路径，保留完整交付记录后决定继续或清理。当前回读阶段和执行前本地哈希校验分别记录。

### 交付记录

恢复后的首次签名就绪已确认，控制运行、候选全量执行校验及最终清理见 TD-20261012-05。

## TD-20261011-15：原有控制传输在 Windows 程序交付期间返回 502

- 日期：2026-10-11
- 状态：调查中 / 根因未确认。
- 影响版本与环境：e3dc0b7 本机持久旧 gateway、原 Windows serve，16 MiB 新控制程序传输。
- 关联历史故障：TD-20261011-01/02 的临时工具超时及 TD-20261011-10 的 Windows 启动退出；当前 502 不能仅据症状归因于这些故障。

### 症状与影响

传输前原共享 OPTIONS 200、可见项 0；交付时请求没有完成，独立 PROPFIND 返回 502。新控制端尚未在 Windows 启用。

### 触发条件与复现

旧 gateway 长时间连接后传送新 exe，原交付随后成功，exe 全量回读 SHA256 匹配，348.569 秒；发生的是同时辅助 PROPFIND 的 502。新 gateway 重新连接成功，实际 relay usw1/RTT 783.1 ms。

### 根因与证据

未确认，需区分已连接通道失效、对端/网络暂不可达和传输超时。不会把 502 当作磁盘或代理核心修复结论。

### 解决办法与恢复操作

先停止自有未完成交付请求，重新建立已授权通道，再核对/清理自有 bundle 或继续交付。身份/授权保留，不操作稳定服务。

### 验证结果与边界

原生最终 CI 两平台成功；Windows 控制包本轮交付尚未成功，不能让用户运行未核对的程序。

### 防复发措施与后续

保存交付回读 SHA256，控制客户端需检测传输失效并自主重连；诊断当前 502 后记录结果。

### 交付记录

[最终构建 CI 38143055331](https://github.com/LumiaBlack51/twodrive/actions/runs/38143055331)；传输结果待补充。

## TD-20261011-14：新控制客户端未绑定当前请求的签名响应，租期末回执易失效

- 日期：2026-10-11
- 状态：已修复并通过两平台原生验证；审查发现，未发生实机攻击。
- 影响版本与环境：c3940ea 的尚未实机启用 test-agent 控制协议。
- 关联历史故障：TD-20261011-11 的控制工具边界审查；与 Windows 路径错误不同。

### 症状与影响

客户端只以会话与序号判断完成，新客户端若先读到旧的有效签名响应，重用其序号后可能把该旧序号的另一操作回执当作当前结果。报告有效期等于进程租期，收集租期结束后清理回执时会被判过期。

### 触发条件与复现

合成控制响应保留同一会话/序号，替换为旧指令的签名回执；不是实机入侵证据。租期退出需要先停止/回收 child，写回执时可能已超过原到期秒。

### 根因与证据

响应未回显所执行请求的设备签名；序号在单一 agent 内防执行重放，但不能独自证明回执属于当前指令。清理回执有效期与执行授权租期未分离。

### 解决办法与恢复操作

签名报告回显实际请求的 signature，客户端校验请求与回执严格相同；报告有效期增加五分钟收集窗口，绝对 agent/child 租期不延长。原服务/实机未更新，无恢复操作。

### 验证结果与边界

旧 c3940ea 客户端在合成回执用例中实际失败（未抛拒绝错误）；修复后的拒绝与正确回执两项 Python 回归通过。真实 QUIC 控制 smoke 和本机完整生命周期通过，请求签名回显已端到端匹配；lease_expires 与报告 expires 相差 300 秒的集成断言通过。最终 CI 38143055331 两平台严格检查、单元/集成、release 与真实 QUIC 控制均通过；Windows 实际 bootstrap 清理、请求签名回显、租期/回执窗口断言通过。不会把协议审查发现说成实际入侵。

### 防复发措施与后续

保留同会话/序号旧回执拒绝用例，真实 QUIC 控制 smoke 必须匹配当前请求；租期与回执收集窗口分别验证。

### 交付记录

[修正 3dddc00](https://github.com/LumiaBlack51/twodrive/commit/3dddc00eed7bbe73f34c06a705ae542c86933851)；[原生最终 CI 38143055331](https://github.com/LumiaBlack51/twodrive/actions/runs/38143055331)。

## TD-20261011-13：修正路径后的 Windows 控制生命周期仍未通过

- 日期：2026-10-11
- 状态：已修复并通过两平台原生完整流程。
- 影响版本与环境：53e4b24，原生 Windows CI 38141818742。
- 关联历史故障：TD-20261011-12 的路径赋值原生回归已通过；完整生命周期另一次失败，需按新阶段核实。

### 症状与影响

Linux 原生完整流程通过；Windows cargo test 仍失败，未交付实机控制包、未推进 dev。

### 触发条件与复现

完成 ProviderPath 修正后再次执行完整 Windows bootstrap 测试，等待具体日志。

### 根因与证据

已确认相邻机制遗漏：ProviderPath 返回的原生 verbatim 路径能供 Rust/.NET 使用，但 PowerShell 5.1 Join-Path 解析该路径时没有 PSDrive，抛 PSArgumentNullException/drive null。CI 精确定位 bootstrap:16，在读取身份/授权 hash 前失败；尚未进入 Rust ready。不是 TD-20261011-12 原生路径字符串问题复发。

### 解决办法与恢复操作

bootstrap 改用 .NET Path.Combine、File/Directory 与流式 SHA256，避免把有效原生路径再次交给 PowerShell provider 路径拼接；原生回归扩展到实际 hash 函数、旧 Join-Path 失败。实机未更新，没有实机恢复。

### 验证结果与边界

旧 53e4b24 原生生命周期在 Join-Path 处失败。c3940ea 的原生 5.1 实际赋值/流式 hash 回归通过（38142327437）；CI 38142329825 两平台整体成功；Windows 13 项单元、9 项既有集成、2 项控制 CLI 完整 bootstrap/child 回归、严格检查、release 和真实 QUIC 控制 smoke 均通过。

### 防复发措施与后续

按具体阶段保留失败证据，修复和原生验证后再交付。

### 交付记录

[失败 CI 38141818742](https://github.com/LumiaBlack51/twodrive/actions/runs/38141818742)；[修正 c3940ea](https://github.com/LumiaBlack51/twodrive/commit/c3940ea51ea979bc3878f085fc8ad4681d26a45e)、[原生完整 CI 38142329825](https://github.com/LumiaBlack51/twodrive/actions/runs/38142329825)。

## TD-20261011-12：远程控制首次 Windows 原生生命周期回归失败

- 日期：2026-10-11
- 状态：路径机制与相邻拼接修正已完成原生验证。
- 影响版本与环境：ee7bdd9/e7f3bbc，原生 Windows CI 38141236619/38141388630 的 cargo test。
- 关联历史故障：TD-20261011-11 新控制工具验证；不能仅以测试失败认定与 TD-20261011-10 网络退出相同根因。

### 症状与影响

Windows 严格编译检查通过，但 cargo test 失败；控制端尚未交付 Windows 实机，dev 未推进。

### 触发条件与复现

第一次执行原生 Windows bootstrap/child 完整生命周期回归。待读取具体失败日志。

### 根因与证据

原生日志显示首次 ready 前退出：`The filename, directory name, or volume label syntax is incorrect. (os error 123)`，35 秒后等待报告断言失败。CI 38141820760 的 Windows PowerShell 5.1 回归确认：Resolve-Path.Path 将 Rust 的 verbatim 状态路径表示为 provider-qualified 字符串，原生 IO 拒绝该字符串；ProviderPath 返回可被原生 IO 使用的真实路径，执行 bootstrap 中的实际赋值语句通过。

### 解决办法与恢复操作

改用 ProviderPath，给状态、queue 和签名报告写入补充阶段上下文；保留原生 verbatim 路径回归与完整 bootstrap 测试。没有实机恢复操作。

### 验证结果与边界

本机完整单元/集成与 QUIC 控制通道通过。旧原生完整生命周期在 ready 前失败；ProviderPath 原生回归通过。随后 Join-Path 的相邻遗漏见 TD-20261011-13。c3940ea 的 CI 38142329825 两平台严格检查、单元/集成、Windows 实际 bootstrap、release 与真实 QUIC 控制 smoke 全部成功。

### 防复发措施与后续

保留首次 bootstrap 完整原生测试，修复后重新验证再交付。

### 交付记录

[失败 CI 38141236619](https://github.com/LumiaBlack51/twodrive/actions/runs/38141236619)、[同源码机制验证 CI 38141388630](https://github.com/LumiaBlack51/twodrive/actions/runs/38141388630)；[修正 53e4b24](https://github.com/LumiaBlack51/twodrive/commit/53e4b24fc8220360ed906deb9ab63f410238b4ab)、[原生路径回归 CI 38141820760](https://github.com/LumiaBlack51/twodrive/actions/runs/38141820760)。

## TD-20261011-11：跨端验证切换失败后未保留 child 退出证据，需反复人工操作

- 日期：2026-10-11
- 状态：已完成两平台原生及 Windows 实机控制验证、远程候选重试和最终清理。
- 影响版本与环境：至 7337283 的临时 Windows 验证脚本；旧 serve 仅提供文件传输。
- 关联历史故障：TD-20261011-07/09/10；07/09 根因已分别验证，10 的核心退出原因仍未确认。

### 症状与影响

用户多次执行脚本和复制日志；最后的 Windows 兜底启动失败没有留下 stderr，阻断自主定位和完整验证。没有推进 dev。

### 触发条件与复现

脚本重启唯一传输端执行分阶段测试；child 退出后 catch 只保存异常类型，finally 删除 runtime 日志。实际 TD-20261011-10 报告未提供核心错误。

### 根因与证据

测试进程与控制传输生命周期耦合；退出错误和日志未进入报告。脚本没有通用的远程重试/诊断机制。这是测试工具缺陷，不据此断言核心网络退出根因。

### 解决办法与恢复操作

用户明确授权实现远程控制。增加 opt-in、绝对租期的 test-agent，通过旧配对共享接收设备签名的 start/snapshot/stop/finish 请求；响应由原 Windows 身份签名，随机会话、序号和有效期防重放。旧 serve 不重启，child 使用独立临时身份、共享与签名指定的临时客户端；保存有限长度 stdout/stderr 和退出码。候选构建复制到私有 runtime，核对签名请求的 SHA256 后执行。Windows 使用独立进程 Job，关闭控制句柄时终止自有子树。

本地恢复：此前三轮 bundle 已删除，原配对状态及旧 serve 保留；首次 bootstrap 是旧版没有执行 API 的技术边界。

### 验证结果与边界

本机 18 项单元、10 项既有集成与 2 项控制 CLI 集成全部通过，严格 Clippy/格式检查通过。实际 child 退出错误保留、失败后重启、重放拒绝、finish 清理及原身份/授权字节不变通过。实际 QUIC/WebDAV 双通道 smoke 也通过：签名指令经过旧服务通道，临时客户端与 child 独立身份读写，停止 child 后旧传输仍可用。最终 CI 38143055331 两平台通过；Windows 13 单元/9 既有集成/2 控制 CLI 集成、实际 PowerShell 5.1 bootstrap、release 真实 QUIC 控制传输/临时客户端 1 MiB 校验与清理全部通过。Windows 实机首次启用待执行。本机自有 3.9 GiB target 已清理，保留完成实机验证所需的原生产物；原稳定 daemon 仍 active/PID 2404/NRestarts 0。提供测试进程管理，不开放监听任意 shell；签名 owner 可授权运行候选构建，候选仍具有启动者的系统权限。没有 Windows 核心兜底成功结论。

### 防复发措施与后续

保留签名/篡改/会话/过期/重放/未知指令回归、实际 child 失败日志捕获与失败后重试、退出清理和原身份/授权字节不变测试。实机取回 TD-20261011-10 退出原因再修复、验证并推进 dev。

2026-10-12 补充：Windows 首次 bootstrap 成功，已收到设备签名就绪回执；测试 child 失败后日志保留、远程重试和候选构建传送均已实际执行，旧 serve 持续可用。复发退出为注册超时，后续调查见 TD-20261012-02；2026-10-12 后续三阶段完整通过，finish 已执行，bootstrap 退出 0、身份授权字节保留、私有 runtime/公开负载删除的回执均已收集；Windows 共享根目录零项、本机测试产物清理完成。脱敏证据见 TD-20261012-05。

### 交付记录

[控制端提交 ee7bdd9](https://github.com/LumiaBlack51/twodrive/commit/ee7bdd9267fb52d4a04d05f1f2c7fadf03b3cc51)；[原生最终 CI 38143055331](https://github.com/LumiaBlack51/twodrive/actions/runs/38143055331)；实机控制结果见 TD-20261012-05 与 [脱敏控制记录](peer-test-control-2026-10-11.json)。

## TD-20261011-10：Windows 默认出口通过后兜底阶段启动失败

- 日期：2026-10-11
- 状态：调查中 / 根因未确认。
- 影响版本与环境：e3dc0b7 原生核心、7337283 Windows 脚本；用户校园网 TUN 与 Linux 热点 TUN。
- 关联历史故障：TD-20261011-07/09 的脚本序列化与读取已通过该次实机默认阶段；本条记录后续 Windows 兜底阶段失败。

### 症状与影响

Windows auto_ready 报告正常；双端默认 auto 与 Linux 人为无网卡兜底各完成 2 MiB 全量哈希、MOVE、Range、DELETE。随后 Windows 兜底阶段报告 RuntimeException，尚未完成 Windows 兜底数据验证，dev 未推进。

### 触发条件与复现

同一脚本先停止默认 serve，再以不存在网卡参数启动 auto serve，预期系统出口兜底。固定 next 信号已发送；用户确认具体警告为 `Test server exited during fallback startup.`。

### 根因与证据

根因未确认。启动检查检测到 child 已退出，报告却只保留 error_type，finally 删除了 stderr，缺少核心退出原因。不能以 RuntimeException 或 30 秒注册预算猜测根因。

### 解决办法与恢复操作

待定位。失败报告已收集，四项恢复/清理标志为 true；Linux 删除自有 bundle 后 GET 404、共享根目录可见项为 0，持久身份和配对保留。默认阶段实际路由选择 aps1、初始 RTT 115.1 ms；用户授权增加远程控制，采用旧 serve 保持传输、独立子进程状态与带设备签名的测试指令，取回退出日志后再定位。

### 验证结果与边界

默认和 Linux 兜底阶段已实际通过，Windows 默认公网 TCP 实际采样 94/94 为物理源地址、TUN fake-IP 存在、路由未变化。Windows 兜底尚未通过，不报告整轮成功。

### 防复发措施与后续

补充启动阶段诊断与对应原生回归，获得实际 Windows 兜底结果后才能推进 dev。

### 交付记录

待补充修正和整轮实机记录。

## TD-20261011-09：修正报告字符串后读取正在重定向的 Windows 日志失败

- 日期：2026-10-11
- 状态：已验证原生活跃日志共享机制，后续实机默认阶段读取与报告成功。
- 影响版本与环境：验证分支 e3dc0b7 的 verify-network.ps1，用户 Windows PowerShell 实机。
- 关联历史故障：TD-20261011-07 的报告修正遗漏实际 Start-Process 日志写入共享条件；不是 JSON 内存异常复发。

### 症状与影响

新脚本在 ReadAllLines(auto.stderr) 处报告文件正由另一进程使用；随后 Validation ended 并返回提示符。新版 serve 和网络验证未完成，dev 未推进。

### 触发条件与复现

Start-Process -RedirectStandardError 的日志 writer 仍保持打开时，报告采样读取同一文件。原生 CI 的合成日志写完即关闭，没有覆盖持续写入句柄。

### 根因与证据

已确认：ReadAllLines 的默认共享方式没有允许已经存在的写入访问，导致 Windows sharing violation。原生 CI 用实际活跃 Start-Process 重定向 writer 复现旧函数 IOException/HRESULT 32，新 FileStream 共享方式通过。用户提供原生 MethodInvocationException，明确路径为本轮 auto.stderr；需用实际 Start-Process 活跃重定向的原生对照验证。

### 解决办法与恢复操作

已用显式允许 ReadWrite/Delete 共享的 FileStream/StreamReader 读取纯字符串，保持报告不带 provider 属性。新增活跃 Start-Process 原生回归，待执行。实机失败报告已收集：identity_and_grants_preserved、original_server_restarted、bundle_payloads_removed、temporary_runtime_removed 全为 true；Linux 删除自有报告/control bundle 后 GET 404，root 可见项为 0。身份与授权目录保留。

### 验证结果与边界

此前静态日志原生回归已通过、实机活跃日志条件失败。CI 38138971808 对旧 ReadAllLines 实际复现共享冲突；修复后的 PowerShell 5.1 与 7 在子进程仍存活、重定向 writer 仍打开时，读取与实际 Wait-Stage/Save-Report 均通过，fixture 子进程和目录已清理。Rust 源码与 e3dc0b7 完全相同，沿用 CI 38137498647 原生 binaries，不将脚本专用 CI 说成新的核心构建；校园网实机默认阶段 47 次采样/94 条物理 TCP，报告正常；后续兜底 child 退出另记 TD-20261011-10。

### 防复发措施与后续

原生测试应使用仍在运行的 Start-Process 重定向 child，验证旧读取失败、新读取成功，并停止 child、删除自有测试目录；实机完成后才能推进 dev。

### 交付记录

[修正提交 7337283](https://github.com/LumiaBlack51/twodrive/commit/733728343cb6b99b70a56f668265b00c0ea2cbfa)；[原生活跃 writer CI 38138971808](https://github.com/LumiaBlack51/twodrive/actions/runs/38138971808)；核心构建 [CI 38137498647](https://github.com/LumiaBlack51/twodrive/actions/runs/38137498647)。

## TD-20261011-08：DNS 并发回归新增后原生 CI 检查失败

- 日期：2026-10-11
- 状态：已验证两平台严格 Clippy、回归与 release CI。
- 影响版本与环境：验证分支 254918b，原生 CI 38137377190。
- 关联历史故障：TD-20261011-06/07 的修复验证；不是用户 serve 内存故障。

### 症状与影响

Linux job 在严格 Clippy 阶段失败，尚未进入 release/实机复测，dev 未推进。

### 触发条件与复现

对验证分支执行双平台原生 CI，具体诊断待读取。

### 根因与证据

提取 prefer_records 后，Backend::query 仅向 FuturesUnordered::push 提供共享引用，原来的 let mut 已不再需要。严格 -D warnings 将 unused_mut 当作错误；Linux 原生日志定位 src/network/dns.rs:59。不是运行期内存或地址发现故障。

### 解决办法与恢复操作

移除多余 mut，保持严格检查；用户设备未重启，无数据恢复操作。

### 验证结果与边界

CI 38137377190 的两平台原先严格 Clippy 失败；修正后的 CI 38137498647 两平台全部通过。Linux 15 单元/10 集成，Windows 10 单元/9 集成，严格检查、release 和 LAN smoke 均通过；原生报告 5.1/7 回归通过。校园网/TUN 实机仍待复测。

### 防复发措施与后续

修正具体检查失败后运行对应双平台检查。

### 交付记录

[失败 CI 38137377190](https://github.com/LumiaBlack51/twodrive/actions/runs/38137377190)；[修正提交 e3dc0b7](https://github.com/LumiaBlack51/twodrive/commit/e3dc0b76a5b5ad274b6ac809003762146d359842)；[通过 CI 38137498647](https://github.com/LumiaBlack51/twodrive/actions/runs/38137498647)。

## TD-20261011-07：Windows PowerShell 5.1 验证报告写入内存耗尽

- 日期：2026-10-11
- 状态：已验证原生 5.1 报告机制；校园网/TUN 实机复测待执行。
- 影响版本与环境：验证分支 bdab13e 的 verify-network.ps1；用户以 powershell.exe 启动，原生 CI 此前仅用 pwsh 解析语法。
- 关联历史故障：TD-20261011-06 同轮切换不可达；本条记录独立的测试脚本故障，不将其当成 Rust serve 内存耗尽。

### 症状与影响

用户报告验证窗口警告 System.OutOfMemoryException。auto.stdout 已出现 Serving device，auto.stderr 已出现物理网卡选择；windows-report.json 未生成。脚本最终恢复和清理状态尚未确认，dev 未推进。

### 触发条件与复现

脚本将 Get-Content 返回的网络日志行存入报告，再调用 ConvertTo-Json -Depth 10。用户原生系统运行一次失败，尚未取得完整异常栈。

### 根因与证据

已确认脚本的报告数据边界缺陷：PowerShell 5.1 的日志字符串带 PSDrive/PSProvider 等扩展属性，深层 JSON 序列化递归展开文件系统对象。[PowerShell #6388](https://github.com/PowerShell/PowerShell/issues/6388)记录相同 Get-Content / Depth 10 路径；[官方文档](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.utility/convertto-json?view=powershell-7.6)说明新版字符串序列化与旧版不同。原生 5.1 已执行旧 Wait-Stage/Save-Report（将 JSON 深度限到 3，避免 OOM），稳定违反“network_messages 只能为纯字符串”的断言；新实际函数以原深度 10 成功写入紧凑报告。

### 解决办法与恢复操作

已改用纯 .NET 日志字符串生成报告，并增加 powershell.exe 实际 Wait-Stage/Save-Report 运行测试，原生 CI 待执行。Linux 本轮验证客户端已停止。用户后续提供异常栈，明确 Save-Report 第 49 行 ConvertTo-Json 失败且已返回提示符；fresh 已配对 Linux 网关重新连通 Windows，原 bundle 的 exe、脚本与报告均 GET 404，root 仅有本轮 bundle。Windows 用户只读复查确认 TemporaryRuntimePresent=false、ServerCount=1；Linux 已删除空的自有 bundle，GET 404、root 可见项为 0，持久身份目录保留。

### 验证结果与边界

CI 38137498647 的 Windows 原生运行步骤已通过：旧 5.1 报告有界失败，新 5.1 与 pwsh 实际 Wait-Stage/Save-Report 测试成功，验证单行、多行、空与不存在日志。网络 API 使用隔离 fixture，未操作真实代理或服务；该测试证明序列化机制，校园网/TUN 实机复测待执行。

### 防复发措施与后续

用隔离合成日志验证实际 Wait-Stage/Save-Report 的紧凑 JSON 与纯字符串字段，包含缺少/单行/多行日志；原生 CI 同时使用 5.1 与 pwsh。报告不保存 provider 对象或凭据。

### 交付记录

[修复提交 254918b](https://github.com/LumiaBlack51/twodrive/commit/254918b5c53e427335c5ae1f743ab54787e7ad4d)；[通过原生 CI 38137498647](https://github.com/LumiaBlack51/twodrive/actions/runs/38137498647)。未推进 dev、未发布。

## TD-20261011-06：Windows 实机切换新版 serve 后已配对客户端无法访问共享

- 日期：2026-10-11
- 状态：调查中 / 根因未确认。
- 影响版本与环境：验证分支 `bdab13e` 的原生 release；Windows 校园网 TUN、Linux 热点 FlClash TUN。
- 关联历史故障：TD-20261011-03 的 Windows 实机验证；TD-20261011-04 涉及地址发现负响应，但尚不能认为根因相同。

### 症状与影响

通过旧 Windows serve 完成 15 MiB 新程序和脚本 PUT/GET 哈希校验后，用户确认启动临时 Windows 验证脚本。Linux 网关先无邀请连通旧 serve，切换期间随后 GET windows-report.json 持续返回 502，尚未取得新版 serve 的报告。没有推进 dev。

### 触发条件与复现

同一持久身份和授权，从旧 Windows serve 切换到自动物理出口的新 serve，未更新客户端已有地址提示。当前只观察到该次真实切换；用户日志确认新版已出现 Serving device，物理接口已选定。随后脚本发生内存异常，另记 TD-20261011-07。

### 根因与证据

已发现 DNS 响应选择缺陷，待回归与实机确认其修复效果。2026-10-11 19:44 从热点物理源地址查询同一公共设备 TXT：1.1.1.1 在约 25 ms 返回 NXDOMAIN（SOA MINIMUM=0），8.8.8.8 在约 315 ms 返回新加坡 relay 的 NOERROR/TXT、TTL 30。已有提示为美西 relay。Backend::query 接受第一个有效结果，快速空响应会取消尚未完成的有效记录查询，无法发现切换后的新 relay。TD-20261011-04 正确区分负响应与出口故障，但遗漏并发正/负响应冲突；不能以重新触发系统兜底代替修正有效 DNS 结果选择。脚本报告故障另记 TD-20261011-07，两者均需处理。

### 解决办法与恢复操作

DNS 并发选择改为优先非空的有效答案；空响应等待其余有界候选，均无记录时仍返回正常负响应，TTL 取最小值；全部请求失败仍报出口失败。新增确定顺序的 fast_negative_does_not_hide_later_positive_dns_record 与 negative_dns_remains_valid_when_no_server_has_records，原生两平台已通过；同一回归临时还原旧 first-valid 选择循环时，两平台均因漏掉有效记录稳定失败，退出后自动恢复源码。临时 Windows 脚本退出后 fresh 客户端已重新连通；脚本修复另记 TD-20261011-07。未改全局路由或代理。

### 验证结果与边界

修复后的原生 CI 38137498647 已通过（Linux 15 单元/10 集成、Windows 10 单元/9 集成及 release/LAN smoke），旧 DNS 选择实际失败与新选择通过对照成立。旧 serve 的程序传送通过；新版 Windows 双端 TUN 实机验证尚未通过，不将原生 CI 当作这次网络验证。

### 防复发措施与后续

确定机制后补充相关回归与实机复测；保留失败阶段和恢复结果。

### 交付记录

[构建提交 bdab13e](https://github.com/LumiaBlack51/twodrive/commit/bdab13ec0d3a2804b93c55498c73385075ed5df7)；[原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38135147276)。待补充实机记录。

## TD-20261011-05：自动物理出口的 Windows 原生 CI 未通过

- 日期：2026-10-11
- 状态：已验证修正后的 Windows 原生测试、release 和双进程 smoke；用户 Windows TUN 实机待执行。
- 影响版本与环境：验证分支 `1ae7b8f`；GitHub Windows 原生 CI 38134766370。
- 关联历史故障：TD-20261011-03 的 Windows 验证；已阅读 TD-20260916-03 的原生 CLI 历史和 TD-20261011-04，不凭相同 CI 失败认定同一根因。

### 症状与影响

Windows matrix job 的两项新网络测试失败；编译和严格 Clippy 成功。未交付新版程序、未重启用户 Windows serve，也未推进 dev。

### 触发条件与复现

对验证分支 workflow_dispatch 原生构建，cargo test 的 8 项单元测试中 6 项通过、2 项失败：Winsock 接口读取和连接器受控回退。

### 根因与证据

原生读取实得索引 14，测试错误期望 network-order 的 234881024。[Microsoft 文档](https://learn.microsoft.com/en-us/windows/win32/winsock/ipproto-ip-socket-options)说明 IP_UNICAST_IF 的设置参数为 network-order，而读取返回 host-order；有效设置的生产代码无需改字节序。另一测试把 u32::MAX 当作不存在接口索引，Windows 的 IPv4 接口索引实际只编码 24 位；原生 loopback 连接成功，不能将这个越界设置当作稳定故障注入。该证据不证明校园网或用户 Windows 的实际出口缺陷。

### 解决办法与恢复操作

读取测试改为检查 host-order 的真实索引；Windows 约束函数明确拒绝 0（未指定）和 IPv4 大于 24 位的非法索引，使同一回退测试在进入 OS 之前得到确定失败，并覆盖两种非法索引。正常 IPv4 设置继续采用 network-order。用户设备和稳定服务未进行恢复操作。

### 验证结果与边界

修正前 Windows 原生测试已实际失败；[修正后的原生 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38135147276) 完整通过，Windows 8 项单元、9 项集成、严格 Clippy、PowerShell 脚本解析、static-CRT release 构建以及独立两进程 2,097,105 字节 GET/PUT/MOVE/DELETE smoke 通过。Linux 的 13 项单元、10 项集成、严格 Clippy、release/LAN smoke 同轮通过。原生 CI 没有用户的校园网和 TUN，不能替代用户实机结果。

### 防复发措施与后续

定位实际失败机制后补充回归，原生 CI 与真实两端验证通过后才能推进 dev。

### 交付记录

[验证提交 1ae7b8f](https://github.com/LumiaBlack51/twodrive/commit/1ae7b8f064511d67c1e88be2ee82ef9b0bfa783c)；[失败 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38134766370)。尚未提交到 dev，未发布。

[修正提交 bdab13e](https://github.com/LumiaBlack51/twodrive/commit/bdab13ec0d3a2804b93c55498c73385075ed5df7)；[通过 CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38135147276)。验证分支尚未推进 dev。

## TD-20261011-04：物理 DNS 将 NXDOMAIN 错当出口失败

- 日期：2026-10-11
- 状态：已验证回归与真实 Linux/Windows 默认模式复测。
- 影响版本与环境：TD-20261011-03 尚未提交的自动物理出口实现，Linux 热点与 FlClash TUN。
- 关联历史故障：TD-20261011-03；这是新 DNS 实现中的响应分类缺陷。

### 症状与影响

默认 auto 的真实双机 2 MiB PUT/GET/MOVE/Range/DELETE 校验全部通过，观察到公网 TCP 使用物理网卡，但设备发现多次报告 DNS TXT 的 system/proxy fallback。该轮不能报告为所有组件均绕过代理。

### 触发条件与复现

公共递归 DNS 对设备发现 TXT 返回 NXDOMAIN。物理源地址上的 dig 查询两个独立公共 nameserver 均得到正常 NXDOMAIN 响应，耗时约 20/329 ms；未泄露私钥或邀请。

### 根因与证据

network/dns.rs 的 validate_response 仅接受 RCODE::NoError，将匹配事务、问题的 NameError 当作无响应，Resolver 因此启动系统 DNS 兜底。有效的无记录结果不代表物理出口失败。对端 TXT 尚无记录的具体原因未确认；已有邀请地址提示仍可建立 relay 连接。

### 解决办法与恢复操作

匹配事务/问题的 NXDOMAIN 与 NODATA 返回空记录，按 [RFC 2308](https://www.rfc-editor.org/rfc/rfc2308) 取 SOA TTL/MINIMUM 较小值作负缓存，缺少 SOA 或 MINIMUM=0 不缓存。空地址结果只在某个地址查询确实失败时允许 DNS 兜底；超时、SERVFAIL、非法响应仍可失败。未调整全局代理或路由。

### 验证结果与边界

同一新增 valid_nxdomain_is_not_a_physical_route_failure 在修复前实际失败（DNS response unusable），修复后通过，同时验证错误事务/问题、SERVFAIL 仍失败，以及负缓存缺少 SOA/MINIMUM=0 时 TTL=0。实验默认与 features mount 的 13 项单元、9 项集成测试均通过，两种配置的严格 Clippy 通过。旧实现的真实 auto 结果保存在[出口验证记录](peer-direct-egress-2026-10-11.json)，保留该轮 TXT 兜底证据。修正后的真实默认 auto 再次完成 2 MiB PUT/GET 哈希、MOVE、256 KiB Range、DELETE，61 次网络/socket 采样未见变化，所见公网 TCP 均约束 wlo1，整轮没有 system/proxy 兜底提示；共享目录为空，两个本轮文件名均 GET 404。该验证不建立对端 TXT 发布成功或 UDP 直连的结论。

### 防复发措施与后续

验证有效 NXDOMAIN 返回空记录，事务/问题错误及 SERVFAIL 继续失败；必须保留失败/修复后的对照与真实复测。

### 交付记录

尚未提交、未发布。

## TD-20261011-03：dev 在 TUN 代理下未统一选择物理出口

- 日期：2026-10-11
- 状态：已验证 Linux 物理出口、真实双机数据与自动兜底回归；Windows 原生出口未验证。
- 影响版本与环境：dev `355287f` 的 WebDAV/QUIC 实验；Linux FlClash TUN 与真实 Windows 对端。
- 关联历史故障：TD-20261011-02 的路由补查确认代理与美西 relay 路径；本次是用户要求修正出口选择，不是其测试脚本超时复发。

### 症状与影响

TwoDrive 可传输并校验数据，但本机策略路由将其 relay 流量送入 TUN，当前代理规则进一步经 VPS 转发；实际所选 relay 在 NA-west，RTT 中位数约 502 ms。用户要求在虚拟网卡存在时自动绕过代理，避免将该路径误当原生 NAT 打洞。

### 触发条件与复现

物理 Wi-Fi 已连接手机热点，同时 FlClash 的自动路由和 fake-IP DNS 开启。现有 dev endpoint 使用默认系统 DNS 与 socket 出口；此前只读路由检查及 15 次真实对端 OPTIONS 已确认所选 relay。

### 根因与证据

已确认应用缺少覆盖 DNS、UDP 和 relay TCP 的统一直连出口策略。实验的 WebDAV 客户端已使用 no_proxy，但该设置只关闭显式 HTTP 代理，不改变系统策略路由。锁定 iroh 1.3.0 支持 UDP 地址绑定，relay TCP 内部仍调用未绑定的 TcpStream::connect。采用进程内带随机认证的回环 CONNECT 连接器，将 relay TLS 透明转发到物理接口绑定的 TCP socket；定制 Resolver 通过物理 DNS socket 查询 A/AAAA/TXT，UDP 绑定物理源地址，地址发布使用相同连接器及原设备签名。物理接口判断依据 OS 与 netdev 元数据，排除 TUN、回环、down 和 fake-IP 地址。不能将源地址绑定等同于绕过所有系统透明拦截。

### 解决办法与恢复操作

已实现仅作用于 dev serve/connect 的 --network-route auto/direct/system，默认 auto；用户明确选择允许代理兜底并提示。DNS、TCP、签名发布和 endpoint 初始化/注册失败可报告 system/proxy fallback；首次 peer 传输失败先关闭 endpoint、以同一持久身份重建系统出口，在发送认证前重试。邀请失败或授权拒绝不触发这项重试。direct 关闭自动兜底，Linux 若接口约束被拒绝则失败；auto 可退为源地址约束并明确提示限制。TLS 证书验证、QUIC 设备认证、单次邀请协议保持。

只在本机启动了隔离 dev 网关，保留既有配对身份；未调整全局代理、路由或稳定服务，也未替换 Windows exe。所有测试文件仅为本轮拥有的合成文件，操作结束删去，Windows 对外可见共享列表为空。本机 dev 进程退出，配对状态仍仅 594 字节；清理独立 experiments/twodrive-dev/target 的全部可再生构建缓存（包括既有缓存），cargo clean 实际删除 15997 项、11.7 GiB。稳定 daemon 仍 active、PID 2404、NRestarts=0；保留源码与脱敏报告。

### 验证结果与边界

新增 12 项网络回归覆盖：物理网卡筛选、fake-IP 拒绝、TCP/DNS 源地址、IPv6-only 的回环访问、DNS 事务/问题/CNAME/负响应及 TCP 截断重试、连接器认证/关闭、auto 与 direct 回退差异、地址发布签名/防篡改和 TLS 必需。受控同一 TCP listener 的旧未绑定连接产生 127.0.0.1，新连接产生指定的 127.0.0.2，证明旧源地址行为违反新出口约束。12 项网络与原 model 共 13 项单元测试、9 项集成（包括一次性邀请、真实本机 QUIC、撤销既有连接）通过，fmt、默认与 features mount 严格 Clippy 均通过；两种配置的同一 22 项测试均通过，不含真实 OneDrive。

真实 Linux→Windows 验证：保留 FlClash TUN 与热点连接，direct 禁止兜底时完成 2 MiB PUT/GET 全量 SHA-256、MOVE、256 KiB Range、DELETE；22 次网络/socket 采样未见路由变化，所见公网 TCP 均带物理 wlo1 接口约束。auto 首轮传输也通过，但 TXT 负响应被误判为出口失败，已另记 TD-20261011-04，不将此轮称为全组件绕过代理。修正后最终默认 auto 再次完成相同校验和清理，61 次采样未见网络变化，所见公网 TCP 均约束 wlo1，无系统/代理兜底提示。初始所选 US-west relay RTT 为 760.9 ms，2 MiB 上传 48.606 秒、读回 9.150 秒，不能据此宣称绕过代理改善速度；[脱敏实测记录](peer-direct-egress-2026-10-11.json)保留全部轮次。

边界：两轮配对时所选路径均为加密 relay，未持续观察 QUIC 路径或建立 UDP peer 直连证据；Windows 使用旧 exe，未验证新增 Windows 原生构建/IP_UNICAST_IF/TUN 行为。iroh UDP 只绑定物理源地址，物理接口按启动时快照选择；网络变化后的重新选择需重启 dev。SO_BINDTODEVICE/IP_UNICAST_IF 无法保证绕过强制 VPN、WFP、透明网关；未保证所有虚拟适配器识别或任意 SOCKS 兜底。策略不覆盖外部 WebDAV 挂载和稳定 OneDrive。周期采样也不能排除采样间的变化。

### 防复发措施与后续

保留物理出口与受控回退回归，保证所有相关 HTTP/TLS 组件接入相同出口，凭据仅留内存、Debug 脱敏、连接器随 endpoint 退出关闭。保留旧代理路径证据与每轮实际结果；后续需 Windows 原生编译和 Windows TUN 实测，不能仅凭 Linux 成功扩大平台承诺。

### 交付记录

Windows 原生验证准备（2026-10-11）：增加 Winsock 接口约束读取回归、Windows/Linux 共用的自动回退回归，以及指定物理网卡的诊断参数。实机脚本沿用原身份与授权，受控重启测试 serve，并在结束或超时后恢复旧程序、清理自身临时负载；脚本不接受任意远程命令。原生 CI 和 Windows 实机验证结果待补充。先在临时验证分支构建，验证完成前不推进 dev。

尚未提交、未发布；[脱敏实测记录](peer-direct-egress-2026-10-11.json)。

## TD-20261011-02：热点复测的 64 MiB 上传触发临时脚本超时

- 日期：2026-10-11
- 状态：已验证临时脚本的发送期限修正；未确认生产客户端传输缺陷。
- 影响版本与环境：dev `355287f` 的 CI release 客户端与临时 Python HTTP 测试脚本；真实 Windows/Linux 双机。
- 关联历史故障：TD-20261007-02 同为验证期限不足，但该次涉及 FUSE 60 秒恢复周期，本次是 Python 大请求体发送预算，根因不同；TD-20261011-01 是另一项临时诊断工具缺陷。

### 症状与影响

用户重新连接热点后，15 次认证 OPTIONS 成功且观察路径为 relay，无邀请重连成功；随后 64 MiB 临时文件上传报 URLError，未完成读回校验。网络监测采样 189 次，未发现活动 Wi-Fi 连接或路由变化。测试脚本 finally 删除本次临时文件，复查共享目录为空，未触碰服务器私有身份状态。

### 触发条件与复现

临时 Python 脚本以 `urllib.request.urlopen(..., timeout=40)` 发送 64 MiB 合成文件，第一次复测在发送阶段报 URLError。之前 16 MiB 上传在约 32 秒完成，该预算对大文件不足。

### 根因与证据

已确认是临时脚本的 40 秒请求体发送预算不足。Python 标准库 `HTTPConnection.send` 对 bytes 请求体调用一次 socket.sendall；`AbstractHTTPHandler.do_open` 仅将 request 阶段的 OSError 包装为 URLError，随后 getresponse 在该内层捕获之外。实测错误为 `URLError: <urlopen error timed out>`。将测试预算提高至 240 秒并保持 64 MiB 请求体与生产代码不变后，记录到单次请求体发送实际耗时 64.587 秒，完整上传耗时 68.943 秒，读回哈希一致。这个实际发送耗时超过原预算，解释了第一次失败；两轮网络监测均未发现本机连接/路由变化。

### 解决办法与恢复操作

仅提高临时验证脚本的 HTTP socket 期限至 240 秒，并记录大请求体发送耗时；没有改变校验条件或生产程序。失败与成功两轮均在 finally 删除自己创建的文件并停止本机测试网关。服务器原子上传对象的 Drop 清理未提交临时路径；WebDAV 目录列表隐藏其保留前缀，故列表为空的直接证据范围是对外可见共享项。

### 验证结果与边界

旧 40 秒预算的真实双机复测失败；240 秒预算的真实双机复测成功完成 64 MiB PUT/GET 全量 SHA-256 比对、MOVE、1 MiB Range 读回及 DELETE，两个临时文件名均 GET 404，共享目录 PROPFIND 返回 0 个可见子项。复测约 226.847 秒，420 次采样未发现活动 Wi-Fi 连接标识、链路状态、全部路由表或策略路由规则变化；15 次独立认证 OPTIONS 的路径观察仍为 relay。详细实际结果见[脱敏验证记录](peer-cross-network-2026-10-11.json)。

这是临时工具的旧预算失败、新预算通过对照，未改动生产程序；不表示 UDP 直连通过，也不证明所有网络吞吐下 240 秒足够。此前数据校验保留，历史固定网络条件仍不确定。按用户要求追加只读路由检查：本机 FlClash TUN 策略路由接管 IPv4，当前代理配置与已保存选项将 relay 送往 Hysteria2 VPS；独立 15 次请求实际所选 relay 位于 NA-west，RTT 中位数约 502 ms。因此本轮不属于绕过代理的原生 NAT 穿透验证；没有修改网络配置。

### 防复发措施与后续

跨网络大文件验证要根据文件大小与已测吞吐设置有界发送预算，并区分请求体发送、响应和后台提交的期限；记录实际发送耗时及 finally 清理结果。保留本次失败/成功对照，测试完成删除临时工具和数据。没有已确认的生产缺陷，未新增生产回归用例。

### 交付记录

本次尚未提交、未发布；[脱敏验证记录](peer-cross-network-2026-10-11.json)包含两次真实尝试及清理范围。

## TD-20261011-01：跨网络验证的临时路径探针收到空 HTTP 响应

- 日期：2026-10-11
- 状态：已验证临时诊断程序的请求生命周期修正；未确认 TwoDrive 生产代码缺陷。
- 影响版本与环境：dev `355287f` 的协议库与独立临时 Rust 诊断辅助程序；真实 Linux/Windows 双机。用户随后报告热点曾断开并自动回落校园网，因此此前整段测试的固定网络条件无法确认。
- 关联历史故障：TD-20261007-02 同为验证工具边界问题，但本次是手工 QUIC/HTTP 探针，不认定同源。

### 症状与影响

现有 release 客户端已通过 64 MiB 读取、真实 FUSE 字节比对，以及 16 MiB 临时文件上传、读回、移动和删除。随后独立路径探针的设备身份认证成功、初始路径为 relay，但第一条 OPTIONS 的 HTTP 响应为空，探针退出，未完成持续路径观察。

### 触发条件与复现

共享测试数据清理后，使用保留的本机设备身份新建 QUIC 连接，认证后手工写入完整 OPTIONS 请求、调用发送流 finish，再读取响应。旧辅助程序在身份认证成功后得到空响应；移除提前 finish 后，同一设备、相同 OPTIONS 请求连续 15 次成功。

### 根因与证据

已定位为辅助程序在等待 HTTP 响应前过早关闭 QUIC 发送流。认证帧返回 accepted 后，旧程序调用 finish，产生 EOF；实验的 `peer::serve` 使用 Hyper HTTP/1 默认的 `half_close(false)`。锁定的 Hyper 1.11.1 在 `mid_message_detect_eof` 中发现响应处理中 EOF 时关闭读端并返回 incomplete，符合空响应现象。仅移除辅助程序的提前 finish 后，15 次 OPTIONS 全部成功，观察持续约 48 秒且所选路径均为 relay。没有证据把该诊断工具问题归因于 NAT 或设备授权失效。

### 解决办法与恢复操作

辅助程序保持请求发送端打开直到收到响应，沿用服务端 HTTP 策略；未修改 TwoDrive 生产代码。

本地恢复操作另述：原本机测试 FUSE 挂载已正常卸载，原 release 客户端已退出。Windows 经用户确认的 7 项合成测试数据已删除，共享目录当时复查为空；私有设备身份与授权状态按用户要求保留。用户重新连接热点后进行受网络监测的独立复测，后续临时数据及工具的最终清理结果见脱敏记录。

### 验证结果与边界

真实 Windows/Linux release 客户端此前完成 64 MiB 远程下载与真实 FUSE 哈希比对、16 MiB 上传/读回/移动/删除；这些结果保留为双机数据正确性证据。因用户报告网络回落，不把此前整段测试称为稳定的热点与校园网跨 NAT 验证。旧临时探针失败，修正后实际 15 次认证 OPTIONS 通过；属于真实双机诊断工具对照，没有新增生产代码回归测试。

重新连接后的持续路径观察、64 MiB 读写复测、网络稳定监测与最终清理详见[脱敏验证记录](peer-cross-network-2026-10-11.json)。relay 可达不等于 UDP 穿透成功；没有真实 OneDrive 测试，没有修改原有用户服务或共享目录以外的 Windows 状态。

### 防复发措施与后续

手工 HTTP/QUIC 探针应遵循服务端的半关闭策略，先验证请求/响应生命周期再统计路径变化；本条记录保留旧程序失败与修正后通过的实际对照。固定网络测试要在操作期间监测活动 Wi-Fi 连接标识及路由，事后单次查看不能排除中途回落。测试结束清理辅助程序、下载包及挂载缓存，仅保留必要的设备配对状态与脱敏记录。

### 交付记录

本次尚未提交、未发布。临时诊断程序不安装到稳定 TwoDrive，也不进入 Windows 开发包；[脱敏验证记录](peer-cross-network-2026-10-11.json)保存实际范围与结果。

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


2026-10-13 交付补记：该条固定下载反馈的三项既有提交已通过[PR #11](https://github.com/LumiaBlack51/twodrive/pull/11) 合入 dev，并包含于 [0.2.12-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.12-1)。包内 Nautilus 25项回归通过；旧调查结论和验证边界保留。

## TD-20261007-PR-WSL：提交前 fetch 的宿主调用超时

- 状态：重试成功；一次 WSL 调用返回 Wsl/Service/WSAETIMEDOUT，根因未确认。
- 影响：首次远端基线检查未完成，不计通过；无应用/凭据/云端数据变化。
- 处理：重新通过 WSL 执行有界 git fetch，仅更新远端跟踪引用，成功取得 main bdacfb9。
- 验证：随后 Git 查询、workspace 测试 127 passed/6 ignored、Clippy 均成功。
- 边界/后续：不归因为 TwoDrive 缺陷；保留超时事实，网络操作继续使用有界等待。

提交检查补记：git diff --cached --check 发现此前未跟踪证据中的 CRLF、终端尾部空格及 EOF 空行；仅规范文本空白后重跑，保留日志内容与所有成功/失败结论。旧 git diff --check 未包含未跟踪文件，不能代表这些文件此前已通过暂存检查。

TD-20261007-PR-WSL 补记：创建 PR 后一次新 WSL 调用再次在启动阶段超时；后续核对工作树干净、HEAD 174a12b，确认该次没有文档写入或提交。重试读取 PR 状态成功；宿主间歇启动超时根因仍未定位，不影响已创建 PR。
## TD-20261007-01：仅云端文件选择始终保留后缺少下载等待标志

- 日期：2026-10-07
- 状态：已验证本地状态图标与原生异步命令机制；真实 Nautilus GUI 与 OneDrive 端到端未验证。
- 影响版本与环境：本次调查基于 main 源码 `bdacfb9`；用户当前运行版本未核实。修复包版本为 0.2.11-2，Rust 二进制版本继续为 0.2.11。
- 关联历史故障：[目录浏览与延迟释放图标](directory-browsing-2026-09-08.md)、[固定策略可靠性](reliability-fix-2026-07-24.md)、[后台下载与恢复](responsiveness-recovery-2026-09-08.md)。本次补齐现有图标映射对“已固定但尚无缓存”的展示遗漏；未证明是历史修复回归，也未把它等同于 TD-20260924 的 Nautilus 卡顿。

### 症状与影响

用户报告对只有云图标的文件选择 Always keep on this device 后没有可见反馈。期望策略生效、下载尚未完成时显示云和 pin，下载完成后仅显示 pin。未确认下载本身失败或数据损坏。

### 触发条件与复现

仅云端文件，选择始终保留，观察下载期间及完成后的 Nautilus 图标。隔离 SQLite 回归复现了显式/继承固定、hydrating 与 pinned 尚无缓存的阶段；原生 Gio 冒烟在临时 mock 数据库中人为延迟 350 ms，再执行实际 CLI 的 MockBackend 下载，验证菜单请求到缓存落盘的转换。不使用真实账户。

### 根因与证据

- `run_action` 原先只启动 Python 工作线程，没有在菜单接受请求时设置图标反馈；扩展环境已有原生主循环可能持有 GIL 的历史限制，状态查询已改用 Gio 子进程，菜单命令却仍依赖线程。本次移除这项线程依赖；未取得用户现场调用栈，不能断言其原操作一定因 GIL 完全未启动。
- Nautilus 查询只读取 state/release_pending，未结合显式或继承 pin 与 cache_path。`recompute_pin_inheritance` 对固定无缓存文件设为 hydrating，而 `begin_hydration` 又会在下载完成前设为 pinned 并清空 cache_path。因此旧展示可能只有同步图标，或过早只显示 pin，无法区分固定策略与缓存完成。
- 新回归在未修改的 `bdacfb9` 扩展上失败，固定无缓存阶段得到单独 cloud/syncing/pin；修复后通过。并未证明下载失败、数据损坏或整个 Nautilus GUI 卡住。

### 解决办法与恢复操作

- 菜单选择仅云端文件时立即提交临时 `pin_pending` 展示，顺序添加 cloud、pin 两个已有图标。持久状态查询使用有效固定策略与缓存字段；在缓存未发布期间持续双图标，发布后只显示 pin，不依赖临时标记的 60 秒期限。
- 数据库状态确认工作或完成后接管临时反馈；每个命令完成/失败时单独清除其临时状态，重新读取真实状态，不把失败请求永久当作已固定。多选仍按顺序执行，单项失败后继续处理其余项。
- 菜单命令改用原生 Gio 异步子进程与完成回调，保留输出、失败通知及一小时超时；上传、错误/冲突与延迟释放的优先级保留。目录汇总尚无缓存的固定子项；新发现的未固定云端子项不被误认为等待固定。
- 中英文首页与使用指南补充双图标含义。未改动数据库迁移、固定策略、下载算法或缓存发布机制。

**本地恢复操作**：只使用临时 SQLite、临时命令包装器及 MockBackend 下载；未安装扩展或新包，未重启 Nautilus/用户服务，未修改用户配置或云端文件。实际会话加载新扩展仍需更新安装并重启 Nautilus。

### 验证结果与边界

- 25 项 Nautilus 回归通过，覆盖点击立即反馈、固定无缓存的显式/继承策略、缓存完成、目录与相邻路径边界、错误/上传/释放优先级、单项失败反馈清除、多选顺序与逐项清除，以及 sync/status/unpin/release 命令参数和通知。新增 pin 回归放入临时目录搭配旧扩展运行，确认旧代码失败；文件信息与菜单回调使用替身，不能称为完整 GUI 验证。
- 真实 Gio/GLib + Nautilus 4 模块冒烟通过：接受菜单请求后立即双图标；模拟延迟阶段原生主循环继续响应；实际 mock CLI 发布缓存后只剩 pin，缓存字节与公开 mock 样本一致。另验证子进程非零退出及 stderr 合并、启动失败、强制超时均只回调一次。超时测试将一小时定时器缩短到 50 ms，未实际等待一小时。
- 101 项 Rust 默认测试、4 项 Settings 回归通过；cargo fmt、Clippy 警告视错误、git diff --check 通过。6 项环境相关 FUSE 测试默认忽略，本次未额外执行（未修改文件系统实现）。
- 0.2.11-2 amd64 deb 从源码提交 `a749e430acde26a4364c5ee2671b472281b17f9a` 的干净 Git 导出构建；解包 CLI/daemon 与 release 产物、扩展/托盘/Settings 与该提交源码字节一致。包内 CLI 版本为 0.2.11；扩展的 25 项回归重新通过。核对软件包无运行令牌、用户配置、数据库或私人构建路径；原有未提交调查不进入包内记录。
- 使用包内 CLI/daemon 运行隔离真实 FUSE + MockBackend 冒烟，挂载、读取、持久写入、模拟上传、移动/删除、pin/unpin/release 均通过并卸载临时挂载。此项是包内容验证，不是 6 项默认忽略测试的补跑，也不代表真实 OneDrive。
- 本机已安装 CLI、daemon、Nautilus 扩展摘要与构建前一致；用户服务保持 MainPID=3051、active、NRestarts=0。未安装 deb 或重启实际服务/Nautilus。
- **边界**：未验证实际 Nautilus 窗口中图标布局、完整右键操作、多扩展交互、用户原会话升级或真实 OneDrive 下载。原生冒烟使用文件信息替身、合成等待状态和 MockBackend，不代表真实 Graph 端到端，也不能证明用户全部“没反应”现象来自同一根因。

### 防复发措施与后续

保留 `PinEmblemTests`、`ActionSequenceTests` 回归，特别是不能仅凭 state=pinned 就宣布本地内容下载完成。菜单动作和状态查询均避免依赖扩展中的 Python 工作线程。后续若再次出现无反馈，需核对已加载扩展版本、持久策略/缓存状态及原生命令执行结果；实际 GUI 和真实云端验收仍待执行。

### 交付记录

- 用户授权将修复与 deb 提交到新分支，明确不更新本机。已推送 [codex/pin-download-feedback](https://github.com/LumiaBlack51/twodrive/tree/codex/pin-download-feedback)；修复源码提交为 [a749e43](https://github.com/LumiaBlack51/twodrive/commit/a749e430acde26a4364c5ee2671b472281b17f9a)，软件包及验证记录提交为 [18120fc](https://github.com/LumiaBlack51/twodrive/commit/18120fc6286e797f57e487c58b5c57a019b54d1d)。远端分支提交及 deb/校验文件的 Git blob 与本地一致；未合并或修改 main。
- 已构建并验证 [0.2.11-2 amd64 deb](../dist/twodrive_0.2.11-2_amd64.deb) 与 [SHA-256 校验文件](../dist/twodrive_0.2.11-2_amd64.deb.sha256)，随本分支提交，不创建 Release，也不安装本机。deb 为 6,419,576 字节，SHA-256：`e2da4946333cdecdd7f67cb7b1481acb8de93a0675c807cd0a682f7c412a230f`。
- 包内故障记录为源码构建时快照；本次包验证与交付补记不更改包内二进制或扩展。原有未提交调查记录和无关文件保留在工作区，不纳入本次提交或软件包。

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

## TD-20260924-02：Nautilus 偶发等待对话框的 48 小时监测

- 日期：2026-09-24
- 状态：调查中 / 根因未确认，已启用本机滚动诊断。
- 影响版本与环境：本机 Nautilus 46.4、TwoDrive 0.2.10-2；本次未发现具体卡顿时间点，运行 daemon 与 TD-20260924-01 所核实版本相同。
- 关联历史故障：TD-20260924-01、[Nautilus 启动](nautilus-startup-2026-09-11.md)、[目录浏览](directory-browsing-2026-09-08.md)。症状相近，不推断是旧修复回归。

### 症状与影响

用户报告 Nautilus 偶发无响应，桌面显示“等待或退出”。影响目录浏览；本次没有确认进程退出或数据损坏。

### 触发条件与复现

发生时间、所在目录、操作顺序均未确定。当前主动启动的 Nautilus 服务 D-Bus Ping 正常，未复现卡顿。

### 根因与证据

根因未确认。TD-20260924-01 未取得卡顿时 Nautilus 堆栈；本次也尚无现场证据，不能仅凭相同提示认定 FUSE、扩展、GIO 或网络为根因。

### 解决办法与恢复操作

新增本机用户级 `twodrive-nautilus-watch.service`，每五秒在 Nautilus 存在时测量 D-Bus Ping。响应达到 700 ms、失败或超时，记录 Nautilus 和 TwoDrive daemon 线程状态与内核等待点；持续异常每 30 秒再采样，并记录恢复时刻。私有 SQLite 逐次删除超过 48 小时的记录，不收集私人路径、文件名、令牌或原始 journal 消息。脚本、查看方式及解释边界见 [Nautilus 卡顿监测说明](nautilus-hang-monitor-2026-09-24.md)。未修改同步或 Nautilus 扩展机制，未重启 TwoDrive 服务。

### 验证结果与边界

- 临时数据库分别测到 Nautilus 不存在和存在时正常 Ping（约 8 ms）。将阈值临时设为 1 ms 后成功生成 Nautilus/daemon 线程快照；注入超过 48 小时的记录后，下一次探测将其删除。
- 用户服务已启用并运行；实际数据库目录为 0700、文件为 0600，已写入 Nautilus 存在时 17 ms 的正常响应记录。原 TwoDrive daemon 保持 MainPID 2455、active、NRestarts=0。
- **边界**：尚未观察到真实卡顿；内核等待点只能缩小调查范围，不能单凭它确认应用层根因。未对真实 OneDrive 做端到端验证，也未证明卡顿已修复。

### 防复发措施与后续

卡顿时尽量选择“等待”并记下时间、目录类型；根据监测时间戳关联脱敏系统日志，再针对确认机制补有意义的回归测试。当前缺乏稳定复现，不能证明旧代码失败与修复后通过。

### 交付记录

本地诊断脚本及说明已保存于仓库工作区，服务安装在当前用户目录；尚未提交、发布或生成安装包。

2026-10-13 提交前补记：按用户要求将此前未提交的调查、监测脚本与说明纳入本仓库。脚本原样保留，本轮8项隔离检查通过：SQLite目录/文件权限、拒绝公开目录/数据库、48小时清理与进程不存在状态、注入慢响应后的恢复记录、30秒异常采样限流、非法间隔在创建状态前拒绝，以及只写临时数据库的本机只读探测。慢响应/错误/恢复用注入结果验证，未复现实际卡顿；不改原有“根因未确认”结论。本轮未安装或重启监测服务、未打开用户诊断数据库、未操作真实云端。已提交 [34dbb83](https://github.com/LumiaBlack51/twodrive/commit/34dbb838ca623a95f2401a7e9777d307e0c040a2) 并通过 [PR #12](https://github.com/LumiaBlack51/twodrive/pull/12) 合入 dev；合并提交 [2e9e523](https://github.com/LumiaBlack51/twodrive/commit/2e9e5239a77f8c84c29a70542576e0556eebac98)。

## TD-20260924-01：弱网时 Nautilus 无响应及 PDF 保存结果不明确

- 日期：2026-09-24
- 状态：调查中 / Nautilus 卡顿及此前 PDF 保存延迟根因未确认；用户确认 PDF 目前显示已保存。
- 影响版本与环境：本机 Linux，Nautilus 46.4；运行 TwoDrive daemon SHA-256 与本地已发布 0.2.10-2 deb 提取产物一致（`2500d0922ede9ffa736ccaa4611c3d670859d4cced59a14bf2533b8de1312332`）；Papers 50.2 inplace-save，安装提交 `16573ca27c3e72932f9d7190c3686711c5d489d8c63f80658e6a74267e77e95e`。
- 关联历史故障：TD-20260922-01、[PDF 保存可靠性](pdf-save-reliability-2026-09-09.md)、[Nautilus 启动](nautilus-startup-2026-09-11.md)、[目录浏览](directory-browsing-2026-09-08.md)。未证实为其中任何机制的复发。

### 症状与影响

用户初报网络不佳时进入 TwoDrive 导致 Nautilus “崩溃”；后续明确为点击目录后无响应，桌面提示“等待或退出”，因此更正为卡顿，不能记录为已确认进程崩溃。已打开课程 PDF 有批注，Ctrl+S 和另存下载目录一度看不到成功结果。调查期间用户确认 PDF 已显示保存。

### 触发条件与复现

原现场未保留卡住的 Nautilus。当前打开 TwoDrive 父目录、挂载根目录、目标课程目录，各执行五次 D-Bus Ping，响应约 3–13 ms，未复现挂起。已请求用户重现时保留窗口并选择等待，以便捕获阻塞位置。

### 根因与证据

**根因未确认，不将弱网与两个症状直接等同为因果。**

- TwoDrive 服务自本次开机连续运行，active、NRestarts=0；Papers 原进程仍在，无本次断言退出证据。
- 日志确有 PDF 上传重复超时，09:53:53、09:56:54、09:59:56、10:02:57（Asia/Shanghai）均报告 `dirty upload remains queued for <document>: <remote request>: operation timed out`。说明云端请求失败并保留重试，不证明本地保存失败或 Nautilus 主线程正在等待该上传。
- 本地下载目录已有 10:47 写入的 PDF。它与从当前 Papers 文件描述符复制出的备份 SHA-256 相同（不公开私人文档摘要），大小均为 14,668,221 字节。只读数据库查询对应文件为 uploading、cache 存在、release_pending=0；本地持久化与云端完成应分开说明。
- Papers 仍持有三个已被原子替换的旧文件描述符，fstat 可读到各自旧属性；通过 /proc 重新打开旧描述符因旧 FUSE inode 不再支持 open 而返回 ENOENT。此操作不等价于原进程原描述符 read 失败，不能据此推断批注丢失或认定原子替换修复回归。
- Nautilus 当前进程的调试器附加被 ptrace 限制拒绝；未取得原故障堆栈，也未修改系统 ptrace 安全设置。

### 解决办法与恢复操作

没有足够证据支持特定程序修复，尚未修改 TwoDrive/Papers 代码。未重启挂载或关闭用户 PDF 窗口。

本地保护操作：将当前 PDF 只读复制到用户私有状态目录 `~/.local/state/twodrive/recovery-20260924/papers-open-fd-47.pdf`，目录权限 0700；该副本仅包含已落盘内容，不宣称提取了未保存的内存编辑。下载目录已有副本不是调查程序生成的。为诊断打开过 Nautilus；首次命令使用 15 秒 timeout，会终止该次诊断进程，该退出不算故障复现。随后无 timeout 启动并完成目录响应检查。

### 验证结果与边界

- pypdf strict=True 解析全部 92 页并解码内容流，统计到 3 个 FreeText 和 8 个 Ink 批注；仅有对象头多余空白警告。pdftotext 全文到 /dev/null 成功。未读取或记录批注正文，未逐页渲染核对，也不能证明所有预期批注均在。
- 当前源码隔离真实 FUSE + MockBackend：`mounted_large_listing_and_copy_remain_responsive_during_download` 通过；后台下载延迟 3 秒时，10,000 条目录枚举与 16 MiB 写入约 140 ms。
- `gio_directory_metadata_does_not_download_unknown_large_files` 通过：30 个虚拟 4 GB 未知格式文件的完整 GIO 元数据查询约 28 ms、零下载；显式复制仍可读取正确字节。
- 19 项 Nautilus Python 回归通过。
- **边界**：上述测试未重现原 GUI 主线程卡顿，未覆盖所有扩展、桌面和网络状态；未进行真实 OneDrive 上传完成后重新下载比对。PDF 当前能保存不是已证明修复原机制。没有确认任何新代码修复，也不承诺再次弱网时不会卡顿。

### 防复发措施与后续

下一步需要故障仍存在时的 Nautilus 主线程及 TwoDrive 请求阻塞证据，区分 FUSE、扩展、GIO 和桌面自身阻塞。若附加仍受限，可在受控复现场景从调试器启动新的 Nautilus；不能仅靠缩短网络超时或重启挂载来宣称根治。确认可复现缺陷后补充旧失败/新通过回归。

### 交付记录

仅保存本次本地调查记录；尚未确认 TwoDrive 新缺陷，因此未生成或发布声称修复本问题的 deb、未推送代码。此前 TD-20260922-01 的本地未提交补记保持原样，未纳入无关提交。

2026-10-13 归档补记：本条与 TD-20260922-01 的既有调查现按用户要求一并提交留档；保留原证据、Papers 独立项目的修复归属及全部验证边界。本轮只归档，不重新执行此前安装、文件恢复或云端操作。

## TD-20260922-01：PDF 保存时 Papers 批注处理断言退出

- 日期：2026-09-22
- 状态：已修复并安装 Papers 批注生命周期与中文字体回退问题；已验证隔离回归和本地保存，用户完整 GUI 操作链及真实云端端到端尚未验证。
- 影响版本与环境：本机默认阅读器 org.gnome.Papers，Flatpak 用户安装，版本 50.2、分支 inplace-save、提交 `8f269aca6d702f7d38685903ea47936222ae5af7a749ec73097a4fe24944f7de`；本地 CLI 报告 TwoDrive 0.2.10，运行 daemon 位于用户安装目录，未核实其构建提交。文件实际位于 TwoDrive FUSE 挂载。
- 关联历史故障：[PDF 保存可靠性调查](pdf-save-reliability-2026-09-09.md)。此前为上传完成修改 mtime 导致 Poppler 拒绝保存；本次明确出现 Papers 批注代码断言，未证明与旧机制有关，不能认定旧修复回归。

### 症状与影响

用户报告保存某课程 PDF 时阅读器疑似退出。日志确认阅读器于 20:27:04（Asia/Shanghai）断言退出。当前文件可解析，但不能确认崩溃前所有编辑均已保存。私人路径不录入此记录。

### 触发条件与复现

用户日常编辑保存；具体点击顺序及稳定复现条件待核实。未在原文件上重演编辑或删除批注操作。

### 根因与证据

直接退出机制已确认：Papers 在批注移除函数中到达不可达分支，主动中止。20:27:04 的脱敏日志：

```text
papers: Duplicate custom ID: palette
PapersView:ERROR:../libview/context/pps-annotations-context.c:792:pps_annotations_context_remove_annotation: code should not be reached
Bail out! PapersView:ERROR:../libview/context/pps-annotations-context.c:792:pps_annotations_context_remove_annotation: code should not be reached
```

第一条警告与后续断言的因果关系未确认。没有堆栈或稳定复现，不能从函数名推断用户一定主动删除了批注，也不能确定具体哪种批注触发。

实际服务为 `twodrive-daemon.service`，自 18:58:53 连续运行，检查时 active、NRestarts=0；20:15–20:28 日志可见正常 delta，20:26–20:29 未见 error/fail/panic/upload 匹配。最初查询的 `twodrive.service` 不是本机运行单元，其 inactive 不代表守护进程停止。未发现同期 TwoDrive 错误证据，但日志缺失错误不等价于排除所有文件系统交互因素。

目标文件 mtime 为 20:27:04，大小 2,074,594 字节；只读数据库查询状态为 cached，存在缓存路径及云端身份。这不是重新下载验证云端内容的证据。

### 解决办法与恢复操作

未修改 TwoDrive 或 Papers 程序，未重启服务，未更改原 PDF 或云端数据。当前优先调查 Papers 批注处理及 inplace-save 构建；无证据支持通过重启 TwoDrive 修复本次断言。

### 验证结果与边界

- pypdf strict=True 成功解析当前 PDF 的全部 23 页，所有页面内容流解码成功，存在 2 个 FreeText 批注；未读取或记录批注正文。
- 系统 pdftotext 全文解析到 /dev/null，退出码 0、无诊断。
- 初次 Python GI Poppler 检查因本机缺少 Poppler typelib 未执行，随后采用上述独立解析检查。
- 未进行逐页视觉检查、阅读器保存复现、旧新代码回归对比或真实 OneDrive 重新下载比对；不能保证最后一次编辑全部保存，也不能将文件可解析等同于内容完全正确。
- /var/crash 未见此次报告，未取得调用栈。

### 防复发措施与后续

下一步需在隔离副本上记录具体批注操作并捕获 Papers 调用栈，对比普通本地目录与挂载路径；确认触发条件后为对应项目增加回归测试。本次无代码修改，不新增推测性的 TwoDrive 回归测试。保留日志中的函数及行号供后续定位。

### 交付记录

仅新增本调查记录；提交及发布未产生。

### 2026-09-22 后续定位、修复与验证

用户补充：输入文字后点击右下角编辑区域的关闭按钮，文字消失；重新进入编辑模式文字出现，随后 Ctrl+S 闪退。用户明确授权修改 Papers，并要求保留此前 Ctrl+S 原地保存改动。

确认两个可独立复现的机制：

1. 在普通本地 PDF 上创建批注、切换为重新加载的文档后删除旧批注，旧 Papers 源码在与日志相同的函数第 792 行断言退出，不需要 TwoDrive。重复删除也失败。空 FreeText 失焦时原代码将控件裸指针排队到 idle 回调，控件销毁、已删除或重新输入文字时没有撤销或重检。原故障的完整 GUI 调用栈未取得，因此只确认这些机制与日志匹配，不声称已经证明用户每一步的内部调用链。
2. 使用相同 PDF 后端在独立空白测试页上绘制中文：Liberation Sans 得到 0 个非白像素，换为覆盖中文的字体后得到可见文字。GTK 编辑框具有字体回退，而 Poppler FreeText 使用单一字体，解释了编辑可见而普通阅读不可见的现象；源 PDF 只读检查发现中文 FreeText。检查过程中用户仍可能编辑原文件，早期两项与后期一项批注的读数不能据此判断丢失原因。

Papers 修复：在查询确认批注属于当前列表后才修改后端/撤销栈；取消已销毁控件的延迟删除，执行前重新核对内容和已删除状态；若当前字体缺字且 Pango 回退字体覆盖全文，则把回退字体写入批注字体属性，保持字号。新输入及已有文字进入编辑均执行检查；没有单一字体覆盖全文时保留原字体，不保证所有混合文字脚本。

验证：

- 8 个生命周期场景中正常删除/撤销/重做、空框自动清理旧新均通过；旧代码在重载后旧对象删除、重复删除、idle 前销毁、重新输入、先删除、重载这 6 个场景退出码 134，修复均为 0。
- 中文渲染/编辑/保存再打开回归旧失败、修复通过，确认英文继续保留 Liberation Sans、字号保持，保存重开中文可见；人工检查了独立生成的中文测试 PNG。
- Meson 共 3 个测试程序通过，包含上述 8 项、三轮原地 JobSave/重开、中文渲染保存。用户 PDF 仅复制到临时普通目录，再验证三轮原地保存与批注计数；原文件未被测试修改。
- 安装后通过运行时库搜索路径指定 /app/lib，再执行三轮保存重开成功；安装库 SHA-256 与测试产物一致，导出符号无增减，运行时无缺失库/未定义符号，Papers --version 正常。
- **验证边界**：未执行完整 GUI 点击/快捷键端到端、所有字体和排版验证、真实 OneDrive 上传再下载对比。当前用户已打开的旧进程未强制退出。不能保证最后一次未保存编辑可恢复。

交付与本地操作：

- 源码、原地保存补丁、修复补丁、测试及回滚包保存在 `/home/lumia/software/papers-fix`，详细说明见该目录 README.md。三个 Rust 源码改动从旧 Debug 扩展恢复，shell 二进制保持原样；仅重建 libppsview，保留原 PDF 后端。
- 安装 Flatpak 提交 `16573ca27c3e72932f9d7190c3686711c5d489d8c63f80658e6a74267e77e95e`；libppsview SHA-256 `cfaa998944d324bcd13fb2e916e3642501fe470d84d5b471cbd41d017e66eaca`。
- 恢复原本缺失的 papers-origin 配置，指向本地 fixed-repo；重新导出 desktop 原始模板，修正复制已部署导出文件造成的 @@u 重复包装，安装成功。备份包已生成，未实际回滚覆盖修复。
- 未修改 TwoDrive 程序或重启服务；无 Git 修复提交、无上游发布。后续完全退出旧 Papers 并重新打开后使用修复版本。

### GitHub 交付补记（2026-09-22）

用户授权建立独立 Papers 修复仓库，并发布“deb 安装器部署修复版 Flatpak”。源码与公开脱敏故障记录已提交至 [papers-inplace-save](https://github.com/LumiaBlack51/papers-inplace-save)，发布源码提交为 [058765d](https://github.com/LumiaBlack51/papers-inplace-save/commit/058765d2b8959c8b490787beb5764b4fb44de896)。

[Release v50.2-fix20260922-1](https://github.com/LumiaBlack51/papers-inplace-save/releases/tag/v50.2-fix20260922-1) 已发布 amd64 deb、Flatpak、完整 Papers 源码归档及 SHA256SUMS；四个远端资产 SHA-256 与本地一致。deb 为当前用户 Flatpak 安装器，非原生 GTK deb；保留 Ctrl+S 原地保存。

新增打包验证：5 项安装器检查通过；独立临时 Flatpak 安装成功从原始备份包升级到修复包并核实提交（该测试使用 --no-deps，不代表首次运行时下载验证）。解包 deb 的 Flatpak 与安装器字节匹配；当前用户已是目标提交时安装器跳过重复安装。完整公开验证与边界见 [Papers 故障记录](https://github.com/LumiaBlack51/papers-inplace-save/blob/main/doc/incident-20260922.md)。本 TwoDrive 条目仍为本地调查记录，未修改其程序或发布 TwoDrive 安装包。

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

2026-10-13 原型归档补记：原始 relay-lab 的15个文件（98,438字节）以原字节保存到 [TwoDrive-relay-lab](../TwoDrive-relay-lab/README.md)，并随[PR #12](https://github.com/LumiaBlack51/twodrive/pull/12) 合入 dev。另加归档说明和本地状态忽略规则，未启用历史CI模板/发布脚本，未接入工作区成员或生产依赖。该操作保存历史审计参考，不修复原型的信任、重放或并发配额缺口，也不扩大原有验证结论。

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
