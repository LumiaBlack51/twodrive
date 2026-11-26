import 'language.dart';
import 'package:flutter/material.dart';
import 'workspace_navigation.dart';
import 'client.dart';
import 'storage_usage.dart';

class TrayPanel extends StatelessWidget {
  const TrayPanel({
    super.key,
    required this.client,
    required this.onManage,
    this.onRefresh,
    required this.onOpenFolder,
    required this.onQuit,
    this.onSignOut,
  });
  final VoidCallback? onRefresh;
  final StorageClient client;
  final Future<void> Function([int page]) onManage;
  final Future<void> Function() onOpenFolder;
  final Future<void> Function() onQuit;
  final Future<void> Function()? onSignOut;
  @override
  Widget build(BuildContext context) {
    final c = client;
    final provider = c.providerLabel;
    final label = c.account;
    final selected = c.selected;
    final revision = c.profile?['revision'];
    bool owns(Map row, {bool accountScoped = false}) =>
        selected != null &&
        (row['profile'] == selected ||
            (accountScoped && row['profile'] == null)) &&
        (row['revision'] == null || row['revision'] == revision);

    final overall = c.status['overall'] as Map?;
    final account = (c.status['accounts'] as List?)
        ?.whereType<Map>()
        .where(owns)
        .firstOrNull;
    final snapshot =
        account ??
        (selected != null && c.status['profile'] == selected ? c.status : null);
    final backup = snapshot?['backup'] as Map? ?? {};
    final cache = snapshot?['cache'] as Map? ?? {};
    final issues = (overall?['issues'] as List?)?.whereType<Map>() ?? [];
    final fallback = c.proxyFallback;
    final otherFallbacks = ((c.status['accounts'] as List?) ?? [])
        .whereType<Map>()
        .where(
          (a) =>
              a['profile'] != selected &&
              a['connected'] == true &&
              a['route_fallback'] == true,
        )
        .map((a) => a['name'])
        .whereType<String>()
        .join(', ');
    final issue =
        c.error ??
        (issues.isEmpty
            ? null
            : issues
                  .map(
                    (row) =>
                        '${row['account']} · ${tr(row['message'].toString())}',
                  )
                  .join('\n')) ??
        (snapshot?['backup_failed'] == true
            ? tr('备份进程已退出，请重新连接')
            : backup['state'] == 'error'
            ? backup['error']?.toString()
            : null);

    Iterable<Map> rows(String key, Iterable<Object?> local) {
      final global = overall?[key] as List?;
      return global == null
          ? local.whereType<Map>().where(
              (row) => owns(row, accountScoped: true),
            )
          : global.whereType<Map>().where(owns);
    }

    final manual = c.transfers.where((t) => owns(t.args));
    final active = rows('active', [
      ...(backup['active'] as List? ?? []),
      ...(snapshot?['mount_activity']?['active'] as List? ?? []),
    ]).toList();
    final activePaths = active
        .map((row) => row['path'])
        .whereType<String>()
        .toSet();
    final waiting = rows('waiting', cache['waiting'] as List? ?? []);
    final all = <Map>[
      for (final t in manual)
        {
          'name': t.name,
          'kind': t.operation == 'view' ? 'download' : t.operation,
          'bytes_done': t.done,
          'bytes_total': t.total,
          'state': t.state,
          'error': t.error,
          'account': c.profile?['name'],
        },
      ...active,
      ...waiting.where((row) => !activePaths.contains(row['path'])),
      ...rows('recent', backup['recent'] as List? ?? []),
    ];
    final (status, statusIcon, color) = switch ((
      c.connected,
      c.paused,
      issue != null,
      fallback,
    )) {
      (false, _, _, _) => ('未连接', Icons.cloud_off, Colors.blueGrey),
      (_, true, _, _) => ('同步已暂停', Icons.pause_circle_filled, Colors.orange),
      (_, _, true, _) => ('需要处理', Icons.error, Colors.deepOrange),
      (_, _, _, true) => (
        '已连接 · 使用系统／代理兜底',
        Icons.warning_amber_rounded,
        const Color(0xffe0a000),
      ),
      _
          when active.isNotEmpty ||
              manual.any((t) => t.phase == TransferPhase.running) =>
        ('正在同步', Icons.sync, Colors.green.shade700),
      _ when (cache['pending'] as num? ?? 0) > 0 || waiting.isNotEmpty => (
        '等待同步',
        Icons.schedule,
        Colors.blueGrey,
      ),
      _ when const ['starting', 'scanning'].contains(backup['state']) => (
        '正在扫描备份',
        Icons.manage_search,
        Colors.blueGrey,
      ),
      _
          when (backup['folders'] as num? ?? 0) > 0 &&
              backup['completed_scan'] == true =>
        ('已备份和同步', Icons.check_circle, Colors.green.shade700),
      _ => ('文件已就绪', Icons.check_circle, Colors.green.shade700),
    };

    final notices = [
      if (fallback) tr('此设备连接已切换到系统网络。'),
      if (otherFallbacks.isNotEmpty)
        tr('以下设备正在使用系统网络：{arg0}', [otherFallbacks]),
      if (issue != null) tr(issue),
      if (c.paused) tr('新同步请求已暂停；正在执行的请求可能完成。'),
    ].join('\n');
    final compact = MediaQuery.sizeOf(context).height <= 360;
    return Scaffold(
      backgroundColor: const Color(0xfffafbfe),
      body: Column(
        children: [
          Container(
            padding: EdgeInsets.fromLTRB(
              20,
              compact ? 8 : 18,
              10,
              compact ? 6 : 12,
            ),
            decoration: const BoxDecoration(
              color: Colors.white,
              border: Border(bottom: BorderSide(color: Color(0xffe5e9f0))),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(
                      c.profile?['icon'] == 'devices'
                          ? Icons.cloud_rounded
                          : Icons.cloud_queue_rounded,
                      color: const Color(0xff2877dc),
                      size: 30,
                    ),
                    const SizedBox(width: 10),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          PopupMenuButton<String>(
                            key: const ValueKey('cloud-switcher'),
                            constraints: const BoxConstraints(
                              minWidth: 260,
                              maxWidth: 360,
                            ),
                            tooltip: tr('切换网盘'),
                            enabled: !c.busy && c.available,
                            onSelected: (id) async {
                              final profile = c.profiles
                                  .where((p) => p['id'] == id)
                                  .firstOrNull;
                              if (profile?['login_required'] == true) {
                                await onManage(2);
                              } else {
                                await c.connect(id);
                              }
                            },
                            itemBuilder: (_) => [
                              for (final p in c.profiles)
                                PopupMenuItem(
                                  value: p['id'] as String,
                                  child: Row(
                                    children: [
                                      Icon(
                                        c.isProfileConnected(p['id'] as String)
                                            ? Icons.cloud_done_outlined
                                            : Icons.cloud_off_outlined,
                                        size: 18,
                                      ),
                                      const SizedBox(width: 8),
                                      Expanded(
                                        child: Text(
                                          p['name'] as String,
                                          maxLines: 1,
                                          overflow: TextOverflow.ellipsis,
                                        ),
                                      ),
                                      const SizedBox(width: 10),
                                      Text(
                                        p['id'] == c.selected
                                            ? tr('当前浏览')
                                            : c.isProfileConnected(
                                                p['id'] as String,
                                              )
                                            ? tr('已连接')
                                            : tr('未连接'),
                                        style: const TextStyle(
                                          fontSize: 11,
                                          color: Colors.blueGrey,
                                        ),
                                      ),
                                      if (p['id'] == c.selected)
                                        const Padding(
                                          padding: EdgeInsets.only(left: 8),
                                          child: Icon(Icons.check, size: 18),
                                        ),
                                    ],
                                  ),
                                ),
                            ],
                            child: Row(
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                Flexible(
                                  child: Text(
                                    provider,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: const TextStyle(
                                      fontWeight: FontWeight.w700,
                                      fontSize: 16,
                                    ),
                                  ),
                                ),
                                const Icon(Icons.expand_more, size: 19),
                              ],
                            ),
                          ),
                          Text(
                            '${c.accountLabel} · $label',
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(
                              fontSize: 12,
                              color: Colors.blueGrey,
                            ),
                          ),
                        ],
                      ),
                    ),
                    PopupMenuButton<String>(
                      tooltip: tr('设置'),
                      popUpAnimationStyle: AnimationStyle.noAnimation,
                      icon: const Icon(Icons.settings_outlined),
                      onSelected: (value) async {
                        switch (value) {
                          case 'dashboard':
                            await onManage(WorkspacePage.overview.index);
                          case 'pause':
                            await c.pauseToggle();
                          case 'signout':
                            if (onSignOut != null) {
                              await onSignOut!();
                            } else {
                              await onManage(2);
                            }
                          case 'quit':
                            await onQuit();
                        }
                      },
                      itemBuilder: (_) => [
                        const PopupMenuItem(
                          value: 'dashboard',
                          child: Row(
                            children: [
                              Icon(Icons.dashboard_outlined, size: 20),
                              SizedBox(width: 12),
                              Text('Dashboard'),
                            ],
                          ),
                        ),
                        const PopupMenuDivider(),
                        PopupMenuItem(
                          value: 'pause',
                          enabled: c.connected && !c.busy,
                          child: Text(c.paused ? tr('恢复同步') : tr('暂停同步')),
                        ),
                        if (c.connected)
                          PopupMenuItem(
                            value: 'signout',
                            child: Text(tr('退出登录…')),
                          ),
                        const PopupMenuDivider(),
                        PopupMenuItem(
                          value: 'quit',
                          child: Text(tr('退出 TwoDrive')),
                        ),
                      ],
                    ),
                  ],
                ),
                SizedBox(height: compact ? 6 : 15),
                Row(
                  children: [
                    Icon(statusIcon, size: 19, color: color),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Tooltip(
                        message: compact && notices.isNotEmpty
                            ? notices
                            : tr(status),
                        child: Text(
                          tr(status),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            color: color,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                    if (onRefresh != null)
                      IconButton(
                        tooltip: tr('刷新状态'),
                        onPressed: c.available && !c.busy && !c.quitting
                            ? onRefresh
                            : null,
                        icon: const Icon(Icons.refresh, size: 16),
                        visualDensity: VisualDensity.compact,
                      ),
                  ],
                ),
                const SizedBox(height: 8),
                StorageUsageView(
                  ring: true,
                  compact: compact,
                  shared: c.profile?['kind'] == 'peer',
                  value: c.quotas[c.selected],
                  connected: c.connected,
                ),
              ],
            ),
          ),
          if (!compact && notices.isNotEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              child: Text(
                notices,
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  color: issue == null ? Colors.blueGrey : Colors.deepOrange,
                  fontSize: 12,
                ),
              ),
            ),
          Expanded(
            child: CustomScrollView(
              key: ValueKey((selected, revision)),
              slivers: [
                if (all.isEmpty)
                  SliverFillRemaining(
                    hasScrollBody: false,
                    child: Center(
                      child: Text(
                        c.connected ? tr('暂无传输记录') : tr('连接账号后查看文件与备份状态'),
                        style: const TextStyle(
                          color: Colors.blueGrey,
                          fontSize: 13,
                        ),
                      ),
                    ),
                  )
                else
                  SliverPadding(
                    padding: const EdgeInsets.symmetric(vertical: 10),
                    sliver: SliverList.separated(
                      itemCount: all.length,
                      separatorBuilder: (_, index) =>
                          const Divider(height: 1, indent: 58),
                      itemBuilder: (_, index) {
                        final item = all[index];
                        final total = (item['bytes_total'] as num?) ?? 0;
                        final done = (item['bytes_done'] as num?) ?? 0;
                        final rawState = item['state']?.toString();
                        final state = rawState == null ? null : tr(rawState);
                        final active =
                            state == null ||
                            state == tr('传输中') ||
                            state == tr('正在备份');
                        final complete =
                            state == tr('已完成') || state == tr('已备份');
                        return Padding(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 18,
                            vertical: 11,
                          ),
                          child: Row(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Icon(
                                item['kind'].toString().contains('download')
                                    ? Icons.file_download_outlined
                                    : Icons.file_upload_outlined,
                                color: Colors.blueGrey,
                              ),
                              const SizedBox(width: 14),
                              Expanded(
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      item['name']?.toString() ?? tr('文件'),
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: const TextStyle(
                                        fontSize: 13,
                                        fontWeight: FontWeight.w500,
                                      ),
                                    ),
                                    if (item['account'] != null)
                                      Text(
                                        item['account'] as String,
                                        maxLines: 1,
                                        overflow: TextOverflow.ellipsis,
                                        style: const TextStyle(
                                          fontSize: 11,
                                          color: Colors.blueGrey,
                                        ),
                                      ),
                                    const SizedBox(height: 5),
                                    Text(
                                      item['error'] != null
                                          ? tr('未完成')
                                          : state ??
                                                (item['kind']
                                                        .toString()
                                                        .contains('download')
                                                    ? tr('正在下载')
                                                    : tr('正在上传')),
                                      style: const TextStyle(
                                        fontSize: 11,
                                        color: Colors.blueGrey,
                                      ),
                                    ),
                                    if (active) ...[
                                      const SizedBox(height: 7),
                                      LinearProgressIndicator(
                                        value: total > 0
                                            ? (done / total).clamp(0, 1)
                                            : null,
                                        minHeight: 3,
                                      ),
                                      const SizedBox(height: 4),
                                      Text(
                                        total > 0
                                            ? '${(done / total * 100).clamp(0, 100).toStringAsFixed(0)}%'
                                            : tr('准备中'),
                                        style: const TextStyle(
                                          fontSize: 10,
                                          color: Colors.blueGrey,
                                        ),
                                      ),
                                    ],
                                  ],
                                ),
                              ),
                              if (complete && item['error'] == null)
                                const Icon(
                                  Icons.check,
                                  size: 16,
                                  color: Colors.green,
                                ),
                            ],
                          ),
                        );
                      },
                    ),
                  ),
              ],
            ),
          ),
          const Divider(height: 1),
          Padding(
            padding: EdgeInsets.symmetric(
              vertical: compact ? 6 : 12,
              horizontal: 14,
            ),
            child: Wrap(
              key: const ValueKey('tray-actions'),
              alignment: WrapAlignment.spaceAround,
              spacing: 8,
              runSpacing: 4,
              children: [
                TextButton.icon(
                  onPressed: c.connected && !c.busy ? onOpenFolder : null,
                  icon: const Icon(Icons.folder_open, size: 20),
                  label: Text(tr('打开文件夹')),
                ),
                TextButton.icon(
                  onPressed: () => onManage(0),
                  icon: const Icon(Icons.grid_view, size: 18),
                  label: Text(tr('查看文件')),
                ),
              ],
            ),
          ),
          if (!compact)
            const RepaintBoundary(
              child: ColoredBox(
                color: Color(0xfffafbfe),
                child: SizedBox(
                  width: double.infinity,
                  child: Padding(
                    padding: EdgeInsets.only(bottom: 8),
                    child: Text(
                      'TwoDrive',
                      textAlign: TextAlign.center,
                      style: TextStyle(fontSize: 10, color: Colors.blueGrey),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}
