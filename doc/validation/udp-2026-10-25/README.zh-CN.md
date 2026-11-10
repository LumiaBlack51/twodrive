# TwoDrive CLI 可靠性测试交付

执行日：2026-10-25；中断后整理日：2026-10-26（Asia/Shanghai）。JSON 时间为 UTC。
本轮仅一台真实 Windows 11 设备，同机启动两个真实 CLI 进程。用户确认暂时没有第二台设备。
**本轮未完成双机跨 NAT 验收；未验证整次传输始终直连；没有修复或宣称修复生产传输故障。**

## 版本与环境

- 基线 `dev`：`0fc9e1203433144fcd8a69ce0580123880b50d1d`，包版本 0.1.7。
- Windows 11 家庭版，10.0.26200，x64；Python 3.14（MSYS2）。两端实为同一操作系统实例。
- 使用实验后端 `experiments/twodrive-dev`，未使用 OneDrive Full/Lite。
- 原生 CI 工作流 [39712667148](https://github.com/LumiaBlack51/twodrive/actions/runs/39712667148)，Windows 资产 12397747913；该工作流与基线提交相同。
- 下载 ZIP SHA-256：`9c9bedf6c963e1809165269e2e2d823e12d214be2b7d5bcce7031edd834a02cb`，与 GitHub 资产摘要一致。
- 执行文件 SHA-256：`b1185b615eb63b266cc30baf3cfb7dbf8c5b1746e68117dcece92b1af034de17`。
- 活跃物理接口为 Intel Wi-Fi；aTrust Tunnel 为 Disconnected，aTrust VNIC 为 Not Present。
- 默认 IPv4 路由接口为物理 Wi-Fi（索引 7）；公网实际出口、上游 NAT 和 CGNAT **未核实**。
- Domain/Private/Public 防火墙均开启；未新增规则或关闭防护。
- WinHTTP 为 Direct；系统静态代理原开关为 1、无显式 PAC URL。前期测试临时将开关设为 0，测试子进程删除 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY；结束恢复开关为 1。
- 后续检查确认 PulseVPN/PulseVPNCore 仍运行（原进程启动时间早于测试）。两次停止后检查仍检测到相关进程；原因未定位，不能断言服务或看门狗导致。失败后恢复启动 VPN 程序，保留原代理配置。
- 因此，**所有已有网络结果均带代理软件未完全关闭的前置限制**，不作为满足用户网络环境要求的验收通过。最后的 final-runner-check 在恢复后的个人网络配置下运行，仅用于检查脚本。
- 未找到本地 Rust/Flutter 工具链；未重新构建 Rust、未执行 Flutter 测试或桌面界面验收。既有 CI 成功仅用于确认下载产物出处。

## 测试矩阵

“通过（局部）”表示表内限定行为得到观察；整体网络验收仍为环境阻塞。可机读清单见 [matrix.csv](matrix.csv)。

| 项目 | 状态 | 实际证据与边界 |
| --- | --- | --- |
| 同机无中继，独立重建连接 5 次 | 通过（局部） | [local-lan.json](local-lan.json)，5/5；不是两台设备的 LAN 证据 |
| 同机自动 public 模式，独立重建 5 次 | 通过（局部） | [local-public.json](local-public.json)，5/5；不能证明跨 NAT |
| 同机强制 relay-only，独立重建 5 次 | 通过（局部） | [local-relay.json](local-relay.json)，5/5；初始路径均 relay |
| 双机 LAN、独立出口、双向交换角色 | 环境阻塞 | 缺第二台设备、拓扑及出口证据 |
| 全程路径变化、direct/relay 时长 | 未测 | CLI 在 peer.rs 配对时输出一次；脚本明确标记 initial-only / unverified |
| 关闭所有代理/VPN 后的网络验收 | 环境阻塞 | VPN 进程仍被检测到，未产生 clean-* 成功报告 |
| 空文件、小文件、中文、空格、长文件名、多层目录 | 通过（局部） | 多份 JSON；生成路径包含 129 字符文件名；不是极限路径长度测试 |
| 1000 个小文件，100 MiB、1 GiB | 通过（局部） | [local-corpus-retry.json](local-corpus-retry.json)，1005 文件全部完成；小文件逐个传输，非千文件并发或千文件同时目录枚举 |
| 上传、下载、Range、MOVE、删除、重名 | 通过（局部） | 每次完整 GET 比对字节数与 SHA-256，MOVE 后再次校验；创建重名 412，移除原路径/删除后 404 |
| 目录浏览 | 通过（局部） | PROPFIND 207 / multistatus；未验证大目录完整枚举 |
| 首轮大文件超时 | 失败 | [local-corpus-first.json](local-corpus-first.json)，600 秒操作期限触发；未捕获具体阶段；后两次通过不等于修复 |
| 中断 PUT：0%、50%、99% | 通过（局部） | [local-faults.json](local-faults.json)，关闭请求连接后目标 GET 均 404；模拟请求中断，不是真实断网 |
| 服务端停止后的错误反馈 | 通过（局部） | 同上，GET 约 15.038 秒得到 HTTP 502；未验证传输中停止与恢复 |
| 只读服务拒绝写入 | 通过（局部） | 原 smoke --read-only，PUT 403；无 GUI |
| 续传 | 未测 | 文档明确不支持断点 PUT、PATCH、Content-Range；本脚本每次完整 PUT，不自动重放写入 |
| 中途/接近完成退出客户端、停止服务端并恢复 | 未测 | 与关闭单个 PUT 请求不同，不能互相替代 |
| UDP 受限、中继不可用、受控丢包/延迟/乱序 | 未测 | 无注入环境；不把 relay-only 当 UDP 阻断实验 |
| 磁盘不足、OS 权限不足、并发任务 | 未测 | 只读 capability 拒绝不能代替 OS 权限不足 |
| 两小时循环、长期内存/连接泄漏、性能退化 | 未测 | 最长一次通过轮次约 180 秒；未运行两小时 soak |
| 失败后临时文件/进程清理 | 通过（局部） | 报告 shared_files_remaining=0、owned_processes_stopped=true；未逐个检查 UDP 端口或系统句柄泄漏 |
| CPU、内存、有效吞吐 | 通过（局部） | 后续报告含 1 秒 Windows 后端采样；首轮和 LAN 早期报告未采样；不能推断长期趋势 |
| 桌面进度/取消/暂停/重连/关窗任务 | 环境阻塞 | 无 Flutter 工具链或 apps/dav 桌面运行产物 |
| 挂载适配器与 Windows 虚拟盘 | 未测 | 本轮无挂载，不把 Windows 未实现能力列为现成功能 |

## 校验和性能摘录

完整序列复测（1000 小文件 + 基本文件 + 大文件）耗时 179.814 秒：

| 文件 | 大小（字节） | SHA-256（上传源与完整下载相同） | PUT 秒 | GET 秒 | GET MiB/s |
| --- | ---: | --- | ---: | ---: | ---: |
| 100MiB.bin | 104857600 | 4cbf988462cc3ba2e10e3aae9f5268546aa79016359fb45be7dd199c073125c0 | 2.311 | 1.476 | 67.738 |
| 1GiB.bin | 1073741824 | 2c06ade942ee3f17a048dd1064b2fab046a4bb95386d8bb41b68dc6711ac2af3 | 23.035 | 16.208 | 63.179 |

单独大文件复测中，采到服务端最大工作集 24,567,808 字节、客户端 24,473,600 字节；累计 CPU 时间分别约 78.25 / 86.50 秒（多核累计，可超过墙钟时间）。原始时间序列在 local-large-retry.json。
吞吐为同机、确定性重复字节数据及散列开销下的观测，不代表真实互联网吞吐；上传、下载、MOVE 后重新下载均验证完整内容。

## 复现脚本

先手动完成两端代理/TUN/VPN 关闭、清除测试进程代理环境、实际出口和防火墙记录；脚本本身不修改全局代理或停止个人程序。
使用临时可丢弃目录，程序自动生成新状态、随机共享子目录；报告采用允许字段，不保存邀请、身份、凭据、IP、网卡 GUID 或原始 CLI 日志。

```powershell
python experiments/twodrive-dev/scripts/test-reliability.py
python experiments/twodrive-dev/scripts/reliability.py --binary <twodrive-dev.exe> --network lan --attempts 5 --small-count 0 --report <private-output>/lan.json
python experiments/twodrive-dev/scripts/reliability.py --binary <twodrive-dev.exe> --network relay-only --attempts 5 --small-count 0 --report <private-output>/relay.json
python experiments/twodrive-dev/scripts/reliability.py --binary <twodrive-dev.exe> --attempts 1 --small-count 1000 --full --faults --timeout 120 --report <private-output>/files.json
```

真实双机先使用 README 的 serve/connect 建立独立状态和可信配对，再运行已配对回环网关模式：

```powershell
python experiments/twodrive-dev/scripts/reliability.py --gateway-port <local-port> --credentials <private-client-state>/credentials.json --attempts 1 --full --report <private-output>/device-A-to-B.json
```

在另一端交换 serve/connect 角色后再运行。网关模式**不会**重新配对五次，也不会自动建立独立 NAT 证据或采集远端资源；须另行记录两端拓扑、出口、每次建连与路径时间序列。
`--soak-seconds 7200` 可循环文件轮次；本机模式每轮重建连接，网关模式使用既有网关。该选项未在本轮运行，且不能代替连接泄漏监控与同一会话恢复测试。
错误停止后不会自动重放 PUT；失败报告保留已完成文件与失败阶段。普通请求有整体期限，慢响应头/响应体都不能无限延长；期限触发不能证明后端有同样的超时策略。
仅随机新建的 `td-reliability-*` 远端集合会被删除；清理失败会记失败，需在可丢弃共享中人工检查，不能盲删其他目录。

## 前后端边界与问题清单

阅读了实验 CLI 指南、provider-boundaries-2026-10-13.md、peer-windows-egress-2026-10-12.json 和 incidents.md。
Flutter StorageClient 通过 JSON IPC 发起操作，desktop.rs 管生命周期，providers/paired.rs 组合 peer 后端；这是源码边界检查，未证明桌面运行行为。
本 PR 仅增加测试脚本、脚本回归与报告；未修改生产传输、Flutter、IPC schema 或存储适配器，没有接口兼容变化。

1. 首次 1 GiB 序列超时，根因未确认，保留失败和两次成功报告。需重现并保留失败操作、进度、资源和双方脱敏诊断。
2. CLI 缺全程路径遥测；现有打印不能满足全程 direct-udp 证明。须增加后端可观测性并验证之后再做直连验收。
3. VPN 软件无法确认完全退出，实际出口未验证；当前网络前置验收阻塞。
4. 第二设备、两小时 soak、异常恢复组合及桌面运行验收待后续补测。

关联故障条目：TD-20261025-01、TD-20261026-01，见 [incidents.md](../../incidents.md)。尚未确认生产代码修复。
