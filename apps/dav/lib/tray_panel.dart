import 'package:flutter/material.dart';
import 'client.dart';
import 'storage_usage.dart';

class TrayPanel extends StatelessWidget {
  const TrayPanel({
    super.key,
    required this.client,
    required this.onManage,
    required this.onOpenFolder,
    required this.onQuit,
    this.onSignOut,
  });
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
    final fallback = c.proxyFallback || overall?['route_fallback'] == true;
    final issue =
        c.error ??
        (issues.isNotEmpty
            ? issues.map((v) => '${v['account']} · ${v['message']}').join('\n')
            : null) ??
        (c.status['backup_failed'] == true
            ? '备份进程已退出，请重新连接'
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
        ? '未连接'
        : c.paused
        ? '同步已暂停'
        : issue != null
        ? '需要处理'
        : fallback
        ? '已连接 · 使用系统／代理兜底'
        : c.syncing
        ? '正在同步'
        : (backup['folders'] as num? ?? 0) > 0 &&
              backup['completed_scan'] == true
        ? '已备份和同步'
        : '文件已就绪';
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
          Container(
            padding: const EdgeInsets.fromLTRB(20, 18, 10, 12),
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
                            tooltip: '切换网盘',
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
                                            ? '当前浏览'
                                            : c.isProfileConnected(
                                                p['id'] as String,
                                              )
                                            ? '已连接'
                                            : '未连接',
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
                      tooltip: '设置',
                      popUpAnimationStyle: AnimationStyle.noAnimation,
                      icon: const Icon(Icons.settings_outlined),
                      onSelected: (value) async {
                        switch (value) {
                          case 'settings':
                            await onManage(5);
                          case 'backups':
                            await onManage(3);
                          case 'pause':
                            await c.pauseToggle();
                          case 'connect':
                            await onManage(2);
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
                          value: 'settings',
                          child: Text('设置'),
                        ),
                        const PopupMenuItem(
                          value: 'backups',
                          child: Text('管理备份文件夹'),
                        ),
                        PopupMenuItem(
                          value: 'pause',
                          enabled: c.connected && !c.busy,
                          child: Text(c.paused ? '恢复同步' : '暂停同步'),
                        ),
                        PopupMenuItem(
                          value: c.connected ? 'signout' : 'connect',
                          child: Text(c.connected ? '退出登录…' : '连接账号'),
                        ),
                        const PopupMenuDivider(),
                        const PopupMenuItem(
                          value: 'quit',
                          child: Text('退出 TwoDrive'),
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
                  ],
                ),
                const SizedBox(height: 8),
                StorageUsageView(
                  ring: true,
                  value: c.quotas[c.selected],
                  connected: c.connected,
                  onRefresh: c.connected
                      ? () {
                          c.refreshQuota(c.selected!, force: true);
                        }
                      : null,
                ),
              ],
            ),
          ),
          if (fallback)
            const Padding(
              padding: EdgeInsets.all(12),
              child: Text(
                '直连未能完成部分请求，已使用系统／代理兜底。',
                style: TextStyle(color: Color(0xff985800), fontSize: 12),
              ),
            ),
          if (issue != null)
            Padding(
              padding: const EdgeInsets.all(12),
              child: Text(
                issue,
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(color: Colors.deepOrange, fontSize: 12),
              ),
            ),
          if (c.paused)
            const Padding(
              padding: EdgeInsets.all(12),
              child: Text(
                '新同步请求已暂停；正在执行的请求可能完成。',
                style: TextStyle(fontSize: 12, color: Colors.blueGrey),
              ),
            ),
          Expanded(
            child: all.isEmpty
                ? Center(
                    child: Text(
                      c.connected ? '暂无传输记录' : '连接账号后查看文件与备份状态',
                      style: const TextStyle(
                        color: Colors.blueGrey,
                        fontSize: 13,
                      ),
                    ),
                  )
                : ListView.separated(
                    padding: const EdgeInsets.symmetric(vertical: 10),
                    itemCount: all.length,
                    separatorBuilder: (_, index) =>
                        const Divider(height: 1, indent: 58),
                    itemBuilder: (_, index) {
                      final item = all[index];
                      final total = (item['bytes_total'] as num?) ?? 0;
                      final done = (item['bytes_done'] as num?) ?? 0;
                      final state = item['state']?.toString();
                      final active = state == null || state == '传输中';
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
                                    item['name']?.toString() ?? '文件',
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
                                        ? '未完成'
                                        : state ??
                                              (item['kind'].toString().contains(
                                                    'download',
                                                  )
                                                  ? '正在下载'
                                                  : '正在上传'),
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
                                          : '准备中',
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
          const Divider(height: 1),
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 12, horizontal: 14),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.spaceAround,
              children: [
                TextButton.icon(
                  onPressed: c.connected && !c.busy ? onOpenFolder : null,
                  icon: const Icon(Icons.folder_open, size: 20),
                  label: const Text('打开文件夹'),
                ),
                TextButton.icon(
                  onPressed: () => onManage(0),
                  icon: const Icon(Icons.grid_view, size: 18),
                  label: const Text('查看文件'),
                ),
              ],
            ),
          ),
          const Padding(
            padding: EdgeInsets.only(bottom: 8),
            child: Text(
              'TwoDrive',
              style: TextStyle(fontSize: 10, color: Colors.blueGrey),
            ),
          ),
        ],
      ),
    );
  }
}
