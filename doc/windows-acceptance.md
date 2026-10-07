# Windows 预览验收清单

## 可自动重跑（全部使用临时根与 MockBackend）

- cargo test -p twodrive-windows --locked：协议 framing、调度暂停、请求版本/重放、
  字节回环、模式隔离、OS 文件锁、持久设置。
- python scripts/test-windows-ipc.py --engine PATH：实际进程、真实本地 IPC、
  并发第二引擎拒绝、断连与重启。
- python scripts/test-windows-editions.py --full FULL_PACKAGE --lite LITE_PACKAGE：
  两版二进制相同、Lite 无 Flutter/Dart、单托盘、Full 关闭仍存活的引擎由
  Lite 复用（核对 PID）、暂停保留、托盘关闭后引擎存活。
- scripts/build-windows.ps1 -OutputDirectory NEW_DIRECTORY：同版本双包、原生测试、
  Flutter analyze/build、文件清单/SHA256、无签名预览标签；-Offline 使用锁定缓存。
- Linux：cargo fmt / clippy / test --workspace --locked，Nautilus 19 项；
  FUSE ignored 测试单独记录，不能冒称已验收。

## 实机操作（只在一次性 state 根）

- [ ] 原生托盘左击打开 392×620 活动弹层，右击 Win32 菜单；Lite 无 Flutter 进程。
- [ ] 弹层失焦关闭，后台 PID 不变；管理中心关闭也不影响后台。
- [ ] UI 暂停后通过独立 IPC 核对 paused；排队任务停止启动，继续后获得完成确认。
- [ ] 关闭测试后台，UI 在轮询/超时期限内变为断连、禁用操作，无“已同步”。
- [ ] 正式未配置模式：无样例账户、无样例文件、不可用能力明确说明。
- [ ] Full/Lite 切换不丢设置、同一状态根只有一个后台与托盘。
- [ ] 多 DPI、多显示器、任务栏不同位置、键盘/屏幕阅读器、Explorer 重启。
- [ ] 原型对应截图注明模式、构建、真实状态来源，不能拿原 HTML 冒充成品。

勾选结果及截图只记入 windows-progress.md 中的实测补记，本模板不预先标通过。

## 正式原生同步发行门槛（当前均未通过）

- [ ] CFAPI 同步根注册/卸载；placeholder 枚举无内容下载。
- [ ] 浏览/图标/缩略图/索引不自动水化；显式打开按范围取数据、可取消。
- [ ] 文件新增/修改/rename/delete 与云端 delta 双向闭环，代际和冲突保护。
- [ ] 脏、正在写入、被占用、未确认上传的文件绝不因释放缓存丢失。
- [ ] 网络断开、限流、令牌过期、磁盘满、进程崩溃、重启恢复。
- [ ] 专用测试账户 OAuth、切换账户、解绑保留本地脏内容；令牌不进 GUI/日志。
- [ ] Full/Lite 升降级与状态迁移、安装卸载、单实例后台、正式签名供应链。
- [ ] Linux 回归与原生 Windows 行为全部满足发布门槛。

未通过这些门槛时，产物只能叫 preview，不能叫可用的 OneDrive 同步发行版。
