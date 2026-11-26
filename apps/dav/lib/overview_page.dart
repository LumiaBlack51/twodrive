import 'language.dart';
import 'dart:async';
import 'package:flutter/material.dart';
import 'client.dart';
import 'workspace_navigation.dart';
import 'storage_usage.dart';
import 'dashboard_metrics.dart';

class OverviewPage extends StatefulWidget {
  const OverviewPage({
    super.key,
    required this.client,
    required this.onNavigate,
    required this.onOpenMount,
  });
  final StorageClient client;
  final ValueChanged<int> onNavigate;
  final Future<void> Function(String) onOpenMount;
  @override
  State<OverviewPage> createState() => _OverviewPageState();
}

class _OverviewPageState extends State<OverviewPage> {
  Timer? timer;
  bool refreshing = false;
  int ticks = 0;
  int visibilityEpoch = 0;
  final clock = Stopwatch()..start();
  final metrics = DashboardMetrics();
  StorageClient get c => widget.client;
  bool get enabled => c.available && !c.busy && !c.quitting;
  List<Json> get activity => [
    ...c.syncActivity,
    for (final t in c.runningTransfers)
      {
        'id': t.id,
        'profile': t.args['profile'],
        'revision': t.args['revision'],
        'path': t.args['path'],
        'name': t.name,
        'kind': t.operation,
        'bytes_done': t.done,
        'bytes_total': t.total,
        'state': t.state,
      },
  ];

  @override
  void initState() {
    super.initState();
    visibilityEpoch = c.visibilityEpoch;
    if (c.viewVisible) metrics.sample(activity, clock.elapsed);
    timer = Timer.periodic(const Duration(seconds: 1), (_) async {
      if (!mounted) return;
      if (visibilityEpoch != c.visibilityEpoch) {
        metrics.suspend();
        visibilityEpoch = c.visibilityEpoch;
      }
      if (!c.viewVisible || !c.available || c.quitting) {
        metrics.suspend();
        return;
      }
      setState(() => metrics.sample(activity, clock.elapsed));
      ticks++;
      if (ticks % 10 != 0 || refreshing || c.busy) return;
      refreshing = true;
      try {
        await c.refreshStatus();
      } catch (_) {
        // The shared status reader owns the visible error.
      } finally {
        refreshing = false;
      }
    });
  }

  @override
  void dispose() {
    timer?.cancel();
    clock.stop();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final problem = c.error != null || c.syncError;
    final state = !c.available
        ? tr('正在连接引擎')
        : problem
        ? tr('有状态需要处理')
        : c.paused
        ? tr('同步已暂停')
        : c.syncing
        ? tr('正在同步')
        : tr('当前无传输活动');
    final active = activity;
    final overviewHeader = SliverList.list(
      children: [
        LayoutBuilder(
          builder: (context, constraints) {
            final status = panel(
              Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      const Icon(Icons.cloud_done_outlined, size: 18),
                      const SizedBox(width: 8),
                      Text(tr('同步状态'), style: heading),
                      const SizedBox(width: 10),
                      Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 9,
                          vertical: 4,
                        ),
                        decoration: BoxDecoration(
                          color: problem
                              ? const Color(0xffffe8df)
                              : c.connectedProfiles.isEmpty
                              ? const Color(0xffe5e9f0)
                              : const Color(0xffc5f4d3),
                          borderRadius: BorderRadius.circular(20),
                        ),
                        child: Text(
                          c.connectedProfiles.isEmpty ? tr('未连接') : tr('• 已连接'),
                          style: TextStyle(
                            color: problem
                                ? Colors.deepOrange
                                : c.connectedProfiles.isEmpty
                                ? const Color(0xff778295)
                                : const Color(0xff2e8553),
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                      const Spacer(),
                      IconButton.filled(
                        tooltip: c.paused ? tr('恢复挂载与备份') : tr('暂停挂载与备份'),
                        onPressed: enabled && c.connectedProfiles.isNotEmpty
                            ? c.pauseToggle
                            : null,
                        icon: Icon(
                          c.paused
                              ? Icons.play_arrow_rounded
                              : Icons.pause_rounded,
                        ),
                      ),
                    ],
                  ),
                  const SizedBox(height: 16),
                  Text(
                    state,
                    style: const TextStyle(
                      fontSize: 28,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                  const SizedBox(height: 8),
                  Text(
                    tr('{arg0} 个网盘已连接 · {arg1} 项活动传输', [
                      c.connectedProfiles.length,
                      active.length,
                    ]),
                    style: muted,
                  ),
                  const SizedBox(height: 18),
                  Row(
                    children: [
                      Expanded(
                        child: detail(
                          tr('已挂载'),
                          tr('{arg0} 个网盘', [c.mounts.length]),
                        ),
                      ),
                      Expanded(
                        child: detail(
                          tr('同步控制'),
                          c.paused ? tr('已暂停') : tr('已启用'),
                        ),
                      ),
                    ],
                  ),
                ],
              ),
              height: 236,
            );
            final rates = Row(
              children: [
                Expanded(
                  child: speedCard(
                    tr('上传'),
                    metrics.uploadRate,
                    metrics.uploadPeak,
                    metrics.uploaded,
                    Icons.arrow_upward,
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: speedCard(
                    tr('下载'),
                    metrics.downloadRate,
                    metrics.downloadPeak,
                    metrics.downloaded,
                    Icons.arrow_downward,
                  ),
                ),
              ],
            );
            if (constraints.maxWidth >= fourColumnWidth) {
              return Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Expanded(child: status),
                  const SizedBox(width: 12),
                  Expanded(child: rates),
                ],
              );
            }
            return Column(
              children: [status, const SizedBox(height: 12), rates],
            );
          },
        ),
        const SizedBox(height: 12),
        LayoutBuilder(
          builder: (context, constraints) {
            final control = panel(
              Row(
                children: [
                  const Icon(Icons.sync, size: 20, color: blue),
                  const SizedBox(width: 9),
                  Expanded(
                    child: Tooltip(
                      message: tr('暂停或继续挂载与备份'),
                      child: Text(tr('挂载与备份自动同步'), style: heading),
                    ),
                  ),
                  Switch(
                    value: !c.paused,
                    onChanged: enabled && c.connectedProfiles.isNotEmpty
                        ? (_) => c.pauseToggle()
                        : null,
                  ),
                ],
              ),
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 2),
            );
            final mounts = panel(
              Row(
                children: [
                  const Icon(Icons.folder_outlined, size: 20, color: blue),
                  const SizedBox(width: 9),
                  Expanded(
                    child: Text(
                      tr('{arg0} 个网盘已挂载', [c.mounts.length]),
                      style: heading,
                    ),
                  ),
                  TextButton(
                    onPressed: () => go(WorkspacePage.connections),
                    child: Text(tr('管理挂载 ›')),
                  ),
                ],
              ),
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 3),
            );
            return constraints.maxWidth >= 760
                ? Row(
                    children: [
                      Expanded(child: control),
                      const SizedBox(width: 12),
                      Expanded(child: mounts),
                    ],
                  )
                : Column(
                    children: [control, const SizedBox(height: 10), mounts],
                  );
          },
        ),
        const SizedBox(height: 14),
        section(
          tr('我的云盘 {arg0}', [c.profiles.length]),
          tr('管理连接'),
          () => go(WorkspacePage.connections),
        ),
        LayoutBuilder(
          builder: (context, constraints) {
            final columns = constraints.maxWidth >= fourColumnWidth
                ? 4
                : constraints.maxWidth >= 540
                ? 2
                : 1;
            return Wrap(
              spacing: 12,
              runSpacing: 12,
              children: [
                for (final profile in c.profiles)
                  SizedBox(
                    width:
                        (constraints.maxWidth - 12 * (columns - 1)) / columns,
                    child: accountCard(profile),
                  ),
                if (c.profiles.isEmpty) panel(Text(tr('添加存储账号或已配对设备，即可开始使用。'))),
              ],
            );
          },
        ),
        const SizedBox(height: 14),
      ],
    );
    final previews = SliverLayoutBuilder(
      builder: (context, constraints) {
        final sideBySide = constraints.crossAxisExtent >= fourColumnWidth;
        // Leave room for the footer's button row and its vertical padding.
        final double? minHeight = sideBySide
            ? (constraints.viewportMainAxisExtent -
                      constraints.precedingScrollExtent -
                      82)
                  .clamp(0.0, 224.0)
            : null;
        final transfers = panel(
          Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              section(
                tr('传输概览'),
                tr('查看全部'),
                () => go(WorkspacePage.transfers),
              ),
              if (active.isEmpty)
                Padding(
                  padding: EdgeInsets.symmetric(vertical: 18),
                  child: Text(tr('当前无传输活动'), style: muted),
                ),
              for (final item in active.take(3)) transferRow(item),
            ],
          ),
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
          height: minHeight,
        );
        final backup = panel(
          Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              section(tr('备份概览'), tr('管理备份'), () => go(WorkspacePage.backup)),
              ...backupRows(),
            ],
          ),
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
          height: minHeight,
        );
        return SliverToBoxAdapter(
          child: sideBySide
              ? IntrinsicHeight(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Expanded(child: transfers),
                      const SizedBox(width: 12),
                      Expanded(child: backup),
                    ],
                  ),
                )
              : Column(
                  children: [transfers, const SizedBox(height: 12), backup],
                ),
        );
      },
    );
    return CustomScrollView(
      key: const ValueKey('sync-overview'),
      slivers: [
        overviewHeader,
        previews,
        SliverToBoxAdapter(
          child: Padding(
            padding: const EdgeInsets.only(top: 26, bottom: 8),
            child: Wrap(
              spacing: 12,
              runSpacing: 8,
              children: [
                TextButton.icon(
                  onPressed: () => go(WorkspacePage.settings),
                  icon: const Icon(Icons.tune, size: 18),
                  label: Text(tr('缓存与排除规则')),
                ),
                TextButton.icon(
                  onPressed: () => go(WorkspacePage.backup),
                  icon: const Icon(Icons.backup_outlined, size: 18),
                  label: Text(tr('备份与上传来源')),
                ),
                Text(
                  tr('已连接 {arg0} / {arg1} · 当前账户缓存 {arg2}', [
                    c.connectedProfiles.length,
                    c.profiles.length,
                    size((c.status['cache']?['bytes'] as num?) ?? 0),
                  ]),
                  style: muted,
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }

  static const double fourColumnWidth = 960;
  static const blue = Color(0xff2472df);
  static const heading = TextStyle(fontSize: 16, fontWeight: FontWeight.w600);
  static const muted = TextStyle(fontSize: 12, color: Color(0xff778295));
  void go(WorkspacePage page) => widget.onNavigate(page.index);
  Widget panel(
    Widget child, {
    EdgeInsets padding = const EdgeInsets.all(18),
    double? height,
  }) => Container(
    width: double.infinity,
    constraints: height == null ? null : BoxConstraints(minHeight: height),
    padding: padding,
    decoration: BoxDecoration(
      color: const Color(0xfffafbff),
      borderRadius: BorderRadius.circular(20),
    ),
    child: child,
  );
  Widget detail(String label, String value) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Text(label, style: muted),
      const SizedBox(height: 5),
      Text(value, style: heading),
    ],
  );
  Widget section(String label, String action, VoidCallback tap) => Row(
    children: [
      Expanded(child: Text(label, style: heading)),
      TextButton(onPressed: tap, child: Text('$action ›')),
    ],
  );
  Widget speedCard(
    String label,
    double rate,
    double peak,
    int total,
    IconData icon,
  ) => panel(
    Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Icon(icon, size: 17, color: blue),
            const SizedBox(width: 7),
            Text(label, style: heading),
            const Spacer(),
            Tooltip(
              message: tr('近期传输速度'),
              child: Icon(
                Icons.info_outline,
                size: 14,
                color: Color(0xff778295),
              ),
            ),
          ],
        ),
        const SizedBox(height: 22),
        FittedBox(
          fit: BoxFit.scaleDown,
          alignment: Alignment.centerLeft,
          child: Text(
            '${capacityBytes(rate)}/s',
            style: const TextStyle(fontSize: 29, fontWeight: FontWeight.w700),
          ),
        ),
        const SizedBox(height: 18),
        Text(tr('观察峰值 {arg0}/s', [capacityBytes(peak)]), style: muted),
        const SizedBox(height: 5),
        Text(tr('本次观察 {arg0}', [capacityBytes(total)]), style: muted),
      ],
    ),
    height: 236,
  );

  Widget accountCard(Json profile) {
    final id = profile['id'] as String;
    final mount = c.mountForProfile(id);
    final online = c.isProfileConnected(id);
    final quota = c.quotas[id];
    final usage = quota?['usage'] as Map?;
    final used = usage?['used'] as num?;
    final total = usage?['total'] as num?;
    final free = usage?['free'] as num?;
    final valid =
        used != null &&
        used.isFinite &&
        used >= 0 &&
        total != null &&
        total.isFinite &&
        total > 0;
    final (icon, color) = switch (profile['kind']) {
      'peer' => (Icons.devices, blue),
      'smb' => (Icons.storage, blue),
      'icloud' => (Icons.apple, const Color(0xff68768d)),
      'baidu' => (Icons.pets, const Color(0xffef7185)),
      'google_drive' ||
      'gdrive' ||
      'google' => (Icons.add_to_drive_outlined, const Color(0xff46a98c)),
      _ => (Icons.cloud, blue),
    };
    final name =
        profile['name'] as String? ??
        profile['provider_label'] as String? ??
        tr('存储账号');
    final account = profile['account_name'] as String?;
    return panel(
      Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Container(
                padding: const EdgeInsets.all(5),
                decoration: BoxDecoration(
                  color: color.withValues(alpha: .09),
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Icon(icon, size: 20, color: color),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  account != null && account != name
                      ? '$name · $account'
                      : name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: heading,
                ),
              ),
            ],
          ),
          const SizedBox(height: 16),
          Text(
            used != null
                ? capacityBytes(used)
                : free != null
                ? tr('剩余 {arg0}', [capacityBytes(free)])
                : '—',
            style: const TextStyle(fontSize: 24, fontWeight: FontWeight.w700),
          ),
          const SizedBox(height: 6),
          Text(
            total != null
                ? tr(profile['kind'] == 'peer' ? '卷容量 {arg0}' : '总量 {arg0}', [
                    capacityBytes(total),
                  ])
                : usage != null
                ? tr('未提供总容量')
                : !online
                ? tr('连接后获取容量')
                : quota?['state'] == 'unsupported'
                ? tr(profile['kind'] == 'peer' ? '共享端未报告容量' : '此网盘暂不提供容量')
                : quota?['state'] == 'unavailable'
                ? tr('容量暂时无法获取')
                : tr('容量尚未获取'),
            style: muted,
          ),
          const SizedBox(height: 9),
          if (valid)
            ClipRRect(
              borderRadius: BorderRadius.circular(4),
              child: LinearProgressIndicator(
                value: (used / total).clamp(0.0, 1.0),
                minHeight: 3,
                color: color,
                backgroundColor: const Color(0xffe7ecfb),
              ),
            ),
          if (quota?['stale'] == true) Text(tr('更新未完成，显示上次结果'), style: muted),
          const SizedBox(height: 6),
          Row(
            key: ValueKey('overview-account-$id'),
            children: [
              Icon(
                mount?['failed'] == true
                    ? Icons.error_outline
                    : mount != null
                    ? Icons.check_circle
                    : Icons.circle_outlined,
                size: 12,
                color: mount?['failed'] == true
                    ? Colors.deepOrange
                    : const Color(0xff388957),
              ),
              const SizedBox(width: 5),
              Expanded(
                child: Text(
                  mount?['failed'] == true
                      ? tr('挂载需要处理')
                      : mount != null
                      ? tr('已挂载')
                      : online
                      ? tr('已连接')
                      : tr('未连接'),
                  style: muted,
                ),
              ),
              if (mount != null)
                TextButton(
                  onPressed: enabled ? () => widget.onOpenMount(id) : null,
                  child: Text(tr('打开目录')),
                )
              else
                TextButton(
                  onPressed: () => go(WorkspacePage.connections),
                  child: Text(tr('管理')),
                ),
            ],
          ),
        ],
      ),
    );
  }

  Widget transferRow(Json item) {
    final total = item['bytes_total'] as num? ?? 0;
    final done = item['bytes_done'] as num? ?? 0;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Row(
        children: [
          Icon(
            item['kind'] == 'download'
                ? Icons.arrow_downward
                : Icons.arrow_upward,
            size: 19,
            color: blue,
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  item['name'] as String? ??
                      item['path'] as String? ??
                      tr('传输任务'),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                Text(
                  item['account'] as String? ??
                      item['state'] as String? ??
                      tr('正在传输'),
                  style: muted,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                const SizedBox(height: 7),
                LinearProgressIndicator(
                  value: total > 0 ? (done / total).clamp(0.0, 1.0) : null,
                  minHeight: 3,
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  List<Widget> backupRows() {
    final accounts = (c.status['accounts'] as List?) ?? [];
    final rows = <Map>[
      for (final a in accounts)
        if ((a['backup']?['folders'] as num? ?? 0) > 0 ||
            a['backup_failed'] == true)
          {
            'name': a['name'],
            ...?a['backup'] as Map?,
            'failed': a['backup_failed'],
          },
      if (accounts.isEmpty && (c.status['backup']?['folders'] as num? ?? 0) > 0)
        {'name': c.profile?['name'], ...c.status['backup'] as Map},
    ];
    if (rows.isEmpty) {
      return [
        Padding(
          padding: EdgeInsets.symmetric(vertical: 18),
          child: Text(tr('尚无备份状态，请在备份页管理文件夹。'), style: muted),
        ),
      ];
    }
    return [
      for (final row in rows.take(3))
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 9),
          child: Row(
            children: [
              const Icon(Icons.folder_outlined, size: 19, color: blue),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      row['name'] as String? ?? tr('备份'),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: heading,
                    ),
                    Text(
                      tr('{arg0} 个文件夹 · {arg1}', [
                        row['folders'] ?? '—',
                        row['failed'] == true || row['state'] == 'error'
                            ? tr('需要处理')
                            : row['state'] == 'scanning'
                            ? tr('正在扫描')
                            : (row['active'] as List? ?? []).isNotEmpty
                            ? tr('正在上传')
                            : row['completed_scan'] == true
                            ? tr('本轮扫描已完成')
                            : tr('等待状态'),
                      ]),
                      style: muted,
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
    ];
  }

  String size(num value) => value < 1048576
      ? '${(value / 1024).toStringAsFixed(1)} KB'
      : value < 1073741824
      ? '${(value / 1048576).toStringAsFixed(1)} MB'
      : '${(value / 1073741824).toStringAsFixed(1)} GB';
}
