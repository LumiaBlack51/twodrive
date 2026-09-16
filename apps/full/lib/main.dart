import 'dart:io';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:window_manager/window_manager.dart';
import 'engine_client.dart';

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  final index = args.indexOf('--state');
  if (index < 0 || index + 1 >= args.length) {
    runApp(const FluentApp(home: Center(child: Text('需要显式指定隔离状态目录 --state'))));
    return;
  }
  final compact = args.contains('--tray');
  await windowManager.ensureInitialized();
  await windowManager.waitUntilReadyToShow(
    WindowOptions(size: compact ? const Size(392, 620) : const Size(1020, 740),
      minimumSize: compact ? const Size(392, 620) : const Size(850, 650),
      title: 'TwoDrive · Full Preview',
      titleBarStyle: compact ? TitleBarStyle.hidden : TitleBarStyle.normal,
      skipTaskbar: compact),
    () async {
      if (compact) await windowManager.setAlignment(Alignment.bottomRight);
      await windowManager.show();
      await windowManager.focus();
    });
  final appDir = File(Platform.resolvedExecutable).parent.parent.path;
  final engine = args.indexOf('--engine');
  final client = EngineClient(
    engine >= 0 && engine + 1 < args.length
      ? args[engine + 1] : '$appDir\\twodrive-engine.exe', args[index + 1]);
  runApp(TwoDrive(client: client, compact: compact));
  client.start();
}

class TwoDrive extends StatefulWidget {
  const TwoDrive({super.key, required this.client, required this.compact});
  final EngineClient client;
  final bool compact;
  @override
  State<TwoDrive> createState() => _TwoDriveState();
}
class _TwoDriveState extends State<TwoDrive> with WindowListener {
  late bool compact = widget.compact;
  int page = 0;
  EngineClient get client => widget.client;
  Map<String, dynamic>? get data => client.snapshot;
  @override
  void initState() { super.initState(); windowManager.addListener(this); }
  @override
  void dispose() { windowManager.removeListener(this); super.dispose(); }
  @override
  void onWindowBlur() { if (compact) windowManager.close(); }
  Future<void> center() async {
    setState(() { compact = false; page = 0; });
    await windowManager.setSkipTaskbar(false);
    await windowManager.setTitleBarStyle(TitleBarStyle.normal);
    await windowManager.setMinimumSize(const Size(850, 650));
    await windowManager.setSize(const Size(1020, 740));
    await windowManager.center();
  }
  @override
  Widget build(BuildContext context) => FluentApp(
    debugShowCheckedModeBanner: false,
    theme: FluentThemeData(brightness: Brightness.light, accentColor: Colors.blue,
      fontFamily: 'Segoe UI', scaffoldBackgroundColor: const Color(0xfff7f9fc)),
    home: AnimatedBuilder(animation: client, builder: (context, _) => ScaffoldPage(
      padding: EdgeInsets.zero,
      content: compact ? tray() : Row(children: [
        SizedBox(width: 190, child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
          const Padding(padding: EdgeInsets.all(24), child: Text('TwoDrive', style: TextStyle(fontSize: 23, fontWeight: FontWeight.w600))),
          for (final entry in ['概览', '文件', '账户', '同步设置', '存储与缓存', '设备', '帮助与诊断'].asMap().entries)
            Padding(padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 3), child: Button(
              onPressed: () => setState(() => page = entry.key),
              child: Align(alignment: Alignment.centerLeft, child: Padding(padding: const EdgeInsets.all(8), child: Text(entry.value,
                style: TextStyle(color: page == entry.key ? const Color(0xff1769d2) : null)))))),
          const Spacer(),
          const Padding(padding: EdgeInsets.all(20), child: Text('Full · 0.2.10 预览\nCFAPI 尚未启用', style: TextStyle(fontSize: 12, color: Color(0xff657387)))),
        ])),
        const Divider(direction: Axis.vertical),
        Expanded(child: Padding(padding: const EdgeInsets.all(28), child: management())),
      ]),
    )),
  );
  String get stateLabel {
    if (data == null) return '后台连接中断';
    return switch (data!['status']) {
      'paused' => '同步已暂停',
      'draining' => '正在暂停 · 等待当前传输结束',
      'transferring' => '正在传输',
      'queued' => '等待同步',
      'mock_idle' => '隔离测试 · 当前无传输',
      'signed_out' => '尚未登录',
      _ => '后台状态未知',
    };
  }
  Widget header() => Padding(padding: const EdgeInsets.fromLTRB(20, 20, 16, 16), child: Row(children: [
    const Icon(FluentIcons.cloud, color: Color(0xff1769d2), size: 23),
    const SizedBox(width: 12),
    Expanded(child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      const Text('TwoDrive · 预览', style: TextStyle(fontWeight: FontWeight.w600)),
      const SizedBox(height: 5),
      Text(data?['mode'] == 'isolated_mock' ? '隔离 Mock · 非真实账户' : '未连接 Microsoft 账户',
        style: const TextStyle(fontSize: 12, color: Color(0xff657387))),
    ])),
    IconButton(icon: const Icon(FluentIcons.settings, size: 16),
      onPressed: () async { await center(); setState(() => page = 3); }),
  ]));
  Widget status() {
    final active = data?['active'] as Map<String, dynamic>?;
    return Padding(padding: const EdgeInsets.symmetric(horizontal: 20), child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      Row(children: [const Icon(FluentIcons.sync, color: Color(0xff1769d2), size: 17),
        const SizedBox(width: 8), Expanded(child: Text(stateLabel, style: const TextStyle(fontWeight: FontWeight.w600))),
        Button(onPressed: client.can('pause') ? () => client.send({'type': 'set_paused', 'paused': !(data!['paused'] as bool)}) : null,
          child: Text(data?['paused'] == true ? '继续' : '暂停')),
      ]),
      const SizedBox(height: 10),
      if (active != null) ...[
        ProgressBar(value: (active['total'] as num) == 0 ? null : 100 * (active['done'] as num) / (active['total'] as num)),
        const SizedBox(height: 6),
        Text('${active['done']} / ${active['total']} 字节 · 等待后台确认完成', style: const TextStyle(fontSize: 11, color: Color(0xff657387))),
      ] else Text(data == null ? '离线状态不能视为已同步' : 'CFAPI 未接入 · 此版本不是原生同步发行版',
        style: const TextStyle(fontSize: 11, color: Color(0xff657387))),
      if (client.pendingMutation) const Padding(padding: EdgeInsets.only(top: 6), child: Text('正在等待后台确认…', style: TextStyle(fontSize: 11))),
      if (client.error != null) Padding(padding: const EdgeInsets.only(top: 8), child: Text(client.error!, style: TextStyle(fontSize: 11, color: Colors.red))),
      const SizedBox(height: 14),
    ]));
  }
  Widget sectionTitle(String title, {Widget? action}) => Padding(
    padding: const EdgeInsets.fromLTRB(20, 16, 16, 12),
    child: Row(children: [Text(title, style: const TextStyle(fontSize: 12, fontWeight: FontWeight.w600)), const Spacer(), ?action]));
  Widget transfer(Map<String, dynamic> item) => Padding(
    padding: const EdgeInsets.fromLTRB(20, 0, 20, 16),
    child: Row(children: [fileIcon(item['name'] as String), const SizedBox(width: 12),
      Expanded(child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Text(item['name'] as String, overflow: TextOverflow.ellipsis, style: const TextStyle(fontSize: 13)),
        const SizedBox(height: 5),
        Text('${item['direction'] == 'upload' ? '上传' : '下载'} · ${item['done']} / ${item['total']} B · ${item['outcome'] == 'completed' ? '后台已确认' : item['outcome'] == 'failed' ? '失败 · 内容保留' : '进行中'}',
          style: const TextStyle(fontSize: 11, color: Color(0xff657387))),
        if (item['outcome'] == 'running') Padding(padding: const EdgeInsets.only(top: 6),
          child: ProgressBar(value: (item['total'] as num) == 0 ? null : 100 * (item['done'] as num) / (item['total'] as num))),
      ])),
    ]));
  Widget fileIcon(String name) {
    final ext = name.contains('.') ? name.split('.').last.toUpperCase() : 'FILE';
    final color = switch (ext) { 'PDF' => const Color(0xffda4f62), 'XLSX' => const Color(0xff279470),
      'ZIP' => const Color(0xff875acd), 'PPTX' => const Color(0xffd57032), _ => const Color(0xff1769d2) };
    // Extension-only glyph: never open cloud content or request a thumbnail.
    return Container(width: 30, height: 36, alignment: Alignment.center,
      decoration: BoxDecoration(color: color.withValues(alpha: .09), border: Border.all(color: color.withValues(alpha: .25)), borderRadius: BorderRadius.circular(3)),
      child: Text(ext.length > 4 ? 'FILE' : ext, style: TextStyle(fontSize: 8, fontWeight: FontWeight.w600, color: color)));
  }
  Widget tray() => Container(color: Colors.white, child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
    header(), status(), const Divider(),
    sectionTitle('正在进行', action: HyperlinkButton(onPressed: center, child: const Text('查看全部'))),
    Expanded(child: ListView(children: [
      if (data?['active'] != null) transfer(data!['active'] as Map<String, dynamic>)
      else const Padding(padding: EdgeInsets.fromLTRB(20, 8, 20, 30), child: Text('没有正在传输的文件', style: TextStyle(color: Color(0xff657387), fontSize: 12))),
      const Divider(), sectionTitle('最近活动'),
      for (final item in (data?['recent'] as List? ?? []).take(5)) transfer(item as Map<String, dynamic>),
      if ((data?['recent'] as List? ?? []).isEmpty) const Padding(padding: EdgeInsets.all(20), child: Text('暂无后台确认记录', style: TextStyle(color: Color(0xff657387), fontSize: 12))),
    ])),
    const Divider(),
    Padding(padding: const EdgeInsets.all(12), child: Row(mainAxisAlignment: MainAxisAlignment.spaceAround, children: [
      const Tooltip(message: 'CFAPI 同步根未实现', child: Button(onPressed: null, child: Column(children: [Icon(FluentIcons.folder_open), Text('打开文件夹', style: TextStyle(fontSize: 11))]))),
      const Tooltip(message: '尚未登录', child: Button(onPressed: null, child: Column(children: [Icon(FluentIcons.globe), Text('网页版', style: TextStyle(fontSize: 11))]))),
      Button(onPressed: center, child: const Column(children: [Icon(FluentIcons.home), Text('管理中心', style: TextStyle(fontSize: 11))])),
    ])),
  ]));
  Widget management() {
    final titles = ['概览', '文件', '账户', '同步设置', '存储与缓存', '设备', '帮助与诊断'];
    return Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
      Text(titles[page], style: const TextStyle(fontSize: 28, fontWeight: FontWeight.w600)),
      const SizedBox(height: 8),
      const Text('所有状态与操作结果由共用 Rust 后台返回', style: TextStyle(fontSize: 12, color: Color(0xff657387))),
      const SizedBox(height: 16),
      if (page != 0) ...[
        Text('$stateLabel · 等待任务：${data?['queued'] ?? '未知'}', style: const TextStyle(fontSize: 12)),
        if (client.error != null) Padding(padding: const EdgeInsets.only(top: 8),
          child: InfoBar(title: const Text('后台状态'), content: Text(client.error!), severity: InfoBarSeverity.warning)),
        const SizedBox(height: 16),
      ],
      Expanded(child: switch (page) {
        0 => ListView(children: [Card(child: Column(children: [header(), status()])),
          sectionTitle('近期活动'), for (final item in data?['recent'] as List? ?? []) transfer(item as Map<String, dynamic>)]),
        1 || 4 => files(),
        2 => unavailable('浏览器登录与账户切换尚未接入', '当前没有真实账户。安全令牌存储已有平台适配；完整 OAuth 和账户隔离验收前，登录入口保持禁用。'),
        3 => ListView(children: [Card(child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          const Text('后台调度器'), const SizedBox(height: 16), status(),
          const Text('暂停设置由后台保存。恢复应用后仍然有效；当前已开始的传输允许完成。'),
        ])), const SizedBox(height: 20), unavailable('更多设置尚未实现', '带宽限制、电池策略、开机启动和缓存预算尚未接入，不提供虚假的保存按钮。')]),
        5 => unavailable('设备控制通道尚未接入', 'Windows peer 只提供控制能力。设备在线或 ping/pong 不代表文件已同步。'),
        _ => ListView(children: [Card(child: Text('Full · 0.2.10 预览\nIPC v1 · 后台 ${data?['engine_version'] ?? '不可用'}\n模式：${data?['mode'] ?? '断连'}\n关闭界面不会停止后台。\n此构建没有正式签名，尚未通过 CFAPI 原生同步验收。')),
          const SizedBox(height: 20), Button(onPressed: client.busy ? null : () => client.send({'type':'snapshot'}), child: const Text('重新连接后台'))]),
      }),
    ]);
  }
  Widget unavailable(String title, String detail) => Card(child: Padding(padding: const EdgeInsets.all(24),
    child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
      const Icon(FluentIcons.info, color: Color(0xff1769d2)), const SizedBox(height: 16),
      Text(title, style: const TextStyle(fontSize: 18)), const SizedBox(height: 12), Text(detail),
      const SizedBox(height: 20), const Button(onPressed: null, child: Text('暂不可用')),
    ])));
  Widget files() {
    final entries = data?['files'] as List? ?? [];
    if (data == null) return unavailable('后台连接中断', '无法读取文件状态，操作已禁用。重新连接后从后台重新获取状态。');
    if (entries.isEmpty) return unavailable('没有可显示的文件', '正式模式不会填入设计样例。登录和原生同步根尚未接入。');
    return ListView(children: [
      const InfoBar(title: Text('隔离测试文件'), content: Text('列表及图标不会读取文件内容。下载由按钮显式触发，释放缓存由后台检查。')),
      if ((data?['file_count'] as int? ?? 0) > 200) const Text('预览仅显示前 200 项；分页待实现。'),
      const SizedBox(height: 12),
      for (final raw in entries) Card(child: Row(children: [
        fileIcon(raw['name'] as String), const SizedBox(width: 12),
        Expanded(child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(raw['name'] as String), Text('${raw['state']} · ${raw['size']} B', style: const TextStyle(fontSize: 11, color: Color(0xff657387))),
        ])),
        Button(onPressed: client.can('download') && raw['state'] == 'online_only' ? () => client.send({'type':'download', 'id':raw['id']}) : null, child: const Text('下载')),
        const SizedBox(width: 8),
        Button(onPressed: client.can('release') && raw['state'] == 'cached' ? () => client.send({'type':'release','id':raw['id']}) : null, child: const Text('释放缓存')),
      ])),
    ]);
  }
}
