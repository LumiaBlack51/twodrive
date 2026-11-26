import 'language.dart';
import 'package:flutter/material.dart';
import 'client.dart';
import 'storage_usage.dart';

class ConnectionCard extends StatelessWidget {
  const ConnectionCard({
    super.key,
    required this.profile,
    required this.online,
    required this.selected,
    required this.connectLabel,
    this.usage,
    this.issue,
    this.onSelect,
    this.onDisconnect,
    this.onSignOut,
    this.onEdit,
    this.onDelete,
    this.onRefresh,
    this.mountSupported = false,
    this.mounted = false,
    this.onMount,
    this.onOpenMount,
  });
  final Json profile;
  final bool online, selected;
  final bool mountSupported, mounted;
  final String connectLabel;
  final Json? usage;
  final String? issue;
  final VoidCallback? onSelect,
      onDisconnect,
      onSignOut,
      onEdit,
      onDelete,
      onRefresh;
  final VoidCallback? onMount, onOpenMount;
  @override
  Widget build(BuildContext context) {
    final provider =
        profile['provider_label'] as String? ?? profile['kind'] as String;
    final account = profile['account_name'] as String?;
    final description = [
      tr(provider),
      if (online) tr('已连接'),
      if (account != null &&
          account.isNotEmpty &&
          account != profile['name'] &&
          account != provider)
        '${tr(profile['account_label'] as String? ?? '账号')}：$account',
      if (profile['summary'] != null &&
          profile['summary'] != provider &&
          profile['summary'] != profile['name'])
        profile['summary'] as String,
    ].join(' · ');
    final actions = Wrap(
      crossAxisAlignment: WrapCrossAlignment.center,
      spacing: 2,
      runSpacing: 4,
      children: [
        Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            OutlinedButton.icon(
              onPressed: onSelect,
              icon: Icon(
                online ? Icons.folder_open_outlined : Icons.login,
                size: 18,
              ),
              label: Text(connectLabel),
            ),
            if (mountSupported) ...[
              const SizedBox(height: 4),
              OutlinedButton.icon(
                onPressed: onOpenMount,
                icon: const Icon(Icons.folder_open, size: 18),
                label: Text(tr('打开挂载')),
              ),
            ],
          ],
        ),
        if (mountSupported)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: OutlinedButton.icon(
              onPressed: onMount,
              icon: Icon(
                mounted ? Icons.eject_outlined : Icons.add_to_drive_outlined,
                size: 18,
              ),
              label: Text(mounted ? tr('卸载') : tr('挂载')),
            ),
          ),
      ],
    );
    final managementActions = {
      if (online) '断开此连接': (Icons.link_off, onDisconnect),
      '编辑': (Icons.edit_outlined, onEdit),
      '删除配置': (Icons.delete_outline, onDelete),
      if (online) '退出登录…': (Icons.logout, onSignOut),
    };
    final menu = PopupMenuButton<String>(
      tooltip: tr('更多操作'),
      icon: const Icon(Icons.more_horiz),
      enabled: managementActions.values.any((action) => action.$2 != null),
      onSelected: (label) => managementActions[label]?.$2?.call(),
      itemBuilder: (context) => [
        for (final MapEntry(key: label, value: (icon, callback))
            in managementActions.entries)
          PopupMenuItem<String>(
            value: label,
            enabled: callback != null,
            child: Row(
              children: [
                Icon(icon, size: 18),
                const SizedBox(width: 10),
                Flexible(child: Text(tr(label))),
              ],
            ),
          ),
      ],
    );
    final details = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Wrap(
          spacing: 8,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            Text(
              profile['name'] as String,
              style: Theme.of(context).textTheme.titleMedium,
            ),
            if (selected)
              Chip(
                visualDensity: VisualDensity.compact,
                avatar: Icon(Icons.check_circle, size: 18, color: Colors.blue),
                label: Text(tr('当前使用')),
              ),
          ],
        ),
        if (issue != null)
          Text(
            tr(issue!),
            style: TextStyle(color: Theme.of(context).colorScheme.error),
          ),
        const SizedBox(height: 3),
        Text(
          description,
          style: const TextStyle(color: Colors.blueGrey),
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
        ),
        const SizedBox(height: 8),
        StorageUsageView(
          value: usage,
          shared: profile['kind'] == 'peer',
          connected: online,
          onRefresh: onRefresh,
        ),
      ],
    );
    return Card(
      color: selected ? const Color(0xfff6f8ff) : Colors.white,
      child: InkWell(
        onTap: onSelect,
        borderRadius: BorderRadius.circular(12),
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: LayoutBuilder(
            builder: (context, constraints) {
              final icon = Icon(
                profile['icon'] == 'devices'
                    ? Icons.devices
                    : Icons.cloud_outlined,
                size: 27,
                color: selected ? Theme.of(context).colorScheme.primary : null,
              );
              if (constraints.maxWidth >= 760) {
                return Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    icon,
                    const SizedBox(width: 16),
                    Expanded(child: details),
                    const SizedBox(width: 24),
                    actions,
                    menu,
                  ],
                );
              }
              return Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      icon,
                      const SizedBox(width: 12),
                      Expanded(child: details),
                      menu,
                    ],
                  ),
                  const SizedBox(height: 12),
                  actions,
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}
