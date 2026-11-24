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
    final backup = c.status['backup'] as Map? ?? {};
    final overall = c.status['overall'] as Map?;
    final issues = (overall?['issues'] as List?) ?? [];
    final endpointFallback = c.proxyFallback;
    final fallback =
        endpointFallback ||
        (c.connected && c.status['system_tunnel_active'] == true);
    final otherFallbacks = ((c.status['accounts'] as List?) ?? [])
        .whereType<Map>()
        .where(
          (a) =>
              a['profile'] != c.selected &&
              a['connected'] == true &&
              a['route_fallback'] == true,
        )
        .map((a) => a['name'])
        .whereType<String>()
        .join(', ');
    final issue =
        c.error ??
        (issues.isNotEmpty
            ? issues
                  .map(
                    (v) => '${v['account']} · ${tr(v['message'].toString())}',
                  )
                  .join('\n')
            : null) ??
        (c.status['backup_failed'] == true
            ? tr('备份进程已退出，请重新连接')
            : backup['state'] == 'error'
            ? backup['error']?.toString()
            : null);
    final all = <Json>[
      for (final t in c.transfers)
        {
          'name': t.name,
          'kind': t.operation,
          'bytes_done': t.done,
          'bytes_total': t.total,
          'state': t.state,
          'error': t.error,
          'account': c.profiles
              .where((p) => p['id'] == t.args['profile'])
              .firstOrNull?['name'],
        },
      ...c.syncActivity,
      ...((overall?['waiting'] as List?) ??
              (c.status['cache']?['waiting'] as List?) ??
              [])
          .where(
            (v) => !c.syncActivity.any(
              (a) => a['path'] == v['path'] && a['profile'] == v['profile'],
            ),
          )
          .map((v) => Map<String, dynamic>.from(v as Map)),
      ...((overall?['recent'] as List?) ?? (backup['recent'] as List?) ?? [])
          .map((v) => Map<String, dynamic>.from(v as Map)),
    ];
    final status = !c.connected
        ? tr('未连接')
        : c.paused
        ? tr('同步已暂停')
        : issue != null
        ? tr('需要处理')
        : fallback
        ? tr(endpointFallback ? '已连接 · 使用系统／代理兜底' : '部分请求正在使用系统网络')
        : c.syncing
        ? tr('正在同步')
        : (backup['folders'] as num? ?? 0) > 0 &&
              backup['completed_scan'] == true
        ? tr('已备份和同步')
        : tr('文件已就绪');
    final color = issue != null
        ? Colors.deepOrange
        : fallback
        ? const Color(0xffe0a000)
        : c.paused
        ? Colors.orange
        : c.connected
        ? Colors.green.shade700
        : Colors.blueGrey;
    return Scaffold(
      backgroundColor: const Color(0xfffafbfe),
      body: Column(
        children: [
          Expanded(
            child: CustomScrollView(
              slivers: [
                SliverToBoxAdapter(
                  child: Column(
                    children: [
                      Container(
                        padding: const EdgeInsets.fromLTRB(20, 18, 10, 12),
                        decoration: const BoxDecoration(
                          color: Colors.white,
                          border: Border(
                            bottom: BorderSide(color: Color(0xffe5e9f0)),
                          ),
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
                                    crossAxisAlignment:
                                        CrossAxisAlignment.start,
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
                                          if (profile?['login_required'] ==
                                              true) {
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
                                                    c.isProfileConnected(
                                                          p['id'] as String,
                                                        )
                                                        ? Icons
                                                              .cloud_done_outlined
                                                        : Icons
                                                              .cloud_off_outlined,
                                                    size: 18,
                                                  ),
                                                  const SizedBox(width: 8),
                                                  Expanded(
                                                    child: Text(
                                                      p['name'] as String,
                                                      maxLines: 1,
                                                      overflow:
                                                          TextOverflow.ellipsis,
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
                                                      padding: EdgeInsets.only(
                                                        left: 8,
                                                      ),
                                                      child: Icon(
                                                        Icons.check,
                                                        size: 18,
                                                      ),
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
                                            const Icon(
                                              Icons.expand_more,
                                              size: 19,
                                            ),
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
                                  popUpAnimationStyle:
                                      AnimationStyle.noAnimation,
                                  icon: const Icon(Icons.settings_outlined),
                                  onSelected: (value) async {
                                    switch (value) {
                                      case 'dashboard':
                                        await onManage(
                                          WorkspacePage.overview.index,
                                        );
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
                                          Icon(
                                            Icons.dashboard_outlined,
                                            size: 20,
                                          ),
                                          SizedBox(width: 12),
                                          Text('Dashboard'),
                                        ],
                                      ),
                                    ),
                                    const PopupMenuDivider(),
                                    PopupMenuItem(
                                      value: 'pause',
                                      enabled: c.connected && !c.busy,
                                      child: Text(
                                        c.paused ? tr('恢复同步') : tr('暂停同步'),
                                      ),
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
                            const SizedBox(height: 15),
                            Row(
                              children: [
                                Icon(
                                  issue != null
                                      ? Icons.error
                                      : fallback
                                      ? Icons.warning_amber_rounded
                                      : c.paused
                                      ? Icons.pause_circle_filled
                                      : c.syncing
                                      ? Icons.sync
                                      : c.connected
                                      ? Icons.check_circle
                                      : Icons.cloud_off,
                                  size: 19,
                                  color: color,
                                ),
                                const SizedBox(width: 8),
                                Expanded(
                                  child: Text(
                                    status,
                                    style: TextStyle(
                                      color: color,
                                      fontWeight: FontWeight.w600,
                                    ),
                                  ),
                                ),
                                if (onRefresh != null)
                                  IconButton(
                                    tooltip: tr('刷新状态'),
                                    onPressed:
                                        c.available && !c.busy && !c.quitting
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
                              shared: c.profile?['kind'] == 'peer',
                              value: c.quotas[c.selected],
                              connected: c.connected,
                            ),
                          ],
                        ),
                      ),
                      if (fallback ||
                          (c.connected &&
                              c.status['component_fallback'] == true))
                        Padding(
                          padding: EdgeInsets.all(12),
                          child: Text(
                            endpointFallback
                                ? tr('此设备连接已切换到系统网络。')
                                : fallback
                                ? tr('中继或设备发现的 TCP 连接正在使用系统网络。')
                                : tr('部分辅助网络请求尝试使用系统网络。'),
                            style: TextStyle(
                              color: fallback
                                  ? const Color(0xff985800)
                                  : Colors.blueGrey,
                              fontSize: 12,
                            ),
                          ),
                        ),
                      if (otherFallbacks.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.all(8),
                          child: Text(
                            tr('以下设备正在使用系统网络：{arg0}', [otherFallbacks]),
                          ),
                        ),
                      if (issue != null)
                        Padding(
                          padding: const EdgeInsets.all(12),
                          child: Text(
                            issue,
                            maxLines: 3,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(
                              color: Colors.deepOrange,
                              fontSize: 12,
                            ),
                          ),
                        ),
                      if (c.paused)
                        Padding(
                          padding: EdgeInsets.all(12),
                          child: Text(
                            tr('新同步请求已暂停；正在执行的请求可能完成。'),
                            style: TextStyle(
                              fontSize: 12,
                              color: Colors.blueGrey,
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
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
                        final state = item['state']?.toString();
                        final active = state == null || state == tr('传输中');
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
                              if (!active && item['error'] == null)
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
            padding: const EdgeInsets.symmetric(vertical: 12, horizontal: 14),
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
