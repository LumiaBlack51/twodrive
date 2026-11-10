# PR #46 工具修复验证

2026-11-09，B Windows，Python 3.11.9。基线为 `9965f67`（工具与 `4a7b77f` 相同）。未整合新版 dev，未运行 A、真实云端、QUIC 或双机传输，也未合并 PR。

## 旧失败 / 新通过

最终回归文件通过 `TWODRIVE_TEST_RUNNER` 指定未修改的旧 runner，三个指定用例均 FAIL，退出码 1；相同用例在修复 runner 上通过，全部 12 项回归通过，退出码 0。旧 runner 来自修改前的本地副本，SHA-256 为 `adfbba2d6179e3d243bc57159740f1e91b3cf1433103ab3a91f7f3c80caec1ef`。完整脱敏输出见本目录 old-three-tests.txt、new-tests.txt。

| 用例 | 旧代码观察 | 新代码 |
| --- | --- | --- |
| test_delayed_partial_commit_is_rejected | 首次 404 后约 250ms 才提交的受控 HTTP 服务，未触发失败 | 后续 GET 发现 200，拒绝通过 |
| test_residual_shared_file_fails_attempt | 注入一个真实临时共享文件，计数 1 仍 passed | failed，并记录 cleanup_error |
| test_lost_mkcol_response_records_pending_without_delete | 模拟创建响应丢失，无清理记录 | 记录随机集合定位信息、unconfirmed/pending，不探测或删除 |

额外覆盖正常 0/50/99% 中断的连续缺失观察、确认创建后的主体失败清理、清理失败导致失败，以及主体和清理同时失败时保留主体错误。原有五项 HTTP 期限/截断/摘要回归保留。

## 复现

在私有临时目录导出旧版本 `git show 4a7b77f:experiments/twodrive-dev/scripts/reliability.py`（保留实际换行）。设置 `TWODRIVE_TEST_RUNNER` 为导出路径，再运行：

```powershell
py -3.11 experiments/twodrive-dev/scripts/test-reliability.py -v ReviewRegressions.test_delayed_partial_commit_is_rejected ReviewRegressions.test_residual_shared_file_fails_attempt ReviewRegressions.test_lost_mkcol_response_records_pending_without_delete
Remove-Item Env:TWODRIVE_TEST_RUNNER
py -3.11 experiments/twodrive-dev/scripts/test-reliability.py -v
```

`TWODRIVE_TEST_RUNNER` 只用于测试导入可信的本地 Python 源码，不是生产 runner 选项。CI 原有 test-reliability.py 入口自动包含新增回归；本轮没有新 hosted CI 结果。

## 范围

中断后至少观察 1 秒，目标采样间隔 0.1 秒，实际间隔还包含请求耗时，单请求仍受 gateway timeout 限制。报告保留检查次数、实际观测时长和 `server_completion_confirmed=false`；不保证任意更晚时刻都不会提交，不等于服务端处理完成屏障或真实断网恢复。

只有收到创建成功响应的随机实验集合才自动清理。响应丢失/拒绝时即使服务端实际已创建，也只能报告待人工确认；随机名字或随后 GET 存在均不作为归属证据。定位信息仅为工具生成的相对集合名，不含主机地址、个人路径或凭据。不自动重放写入。

本机进程模式在退出进程后检查残留文件，非零使该轮失败；临时夹仍按既有策略回收，报告保留计数而不保留文件内容。远程网关模式无法枚举服务端隐藏临时文件，不声称具备相同覆盖。

旧 A/B 离线包中的脚本没有自动替换，不能将旧包视为本次修复产物；正式双机验收前还需整合新版 dev、路径遥测并重新交付。历史 1 GiB 超时根因未确认，原证据保留。
