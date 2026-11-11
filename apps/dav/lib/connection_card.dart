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
    this.onSelect,
    this.onDisconnect,
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
  final VoidCallback? onSelect, onDisconnect, onEdit, onDelete, onRefresh;
  final VoidCallback? onMount, onOpenMount;
  @override
  Widget build(BuildContext context) {
    final provider =
        profile['provider_label'] as String? ?? profile['kind'] as String;
    final account = profile['account_name'] as String?;
    final description = [
      provider,
      if (online) '已连接',
      if (account != null &&
          account.isNotEmpty &&
          account != profile['name'] &&
          account != provider)
        '${profile['account_label'] ?? '账号'}：$account',
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
                label: const Text('打开挂载'),
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
              label: Text(mounted ? '卸载' : '挂载'),
            ),
          ),
        if (online)
          IconButton(
            tooltip: '断开此连接',
            onPressed: onDisconnect,
            icon: const Icon(Icons.link_off),
          ),
        IconButton(
          tooltip: '编辑',
          onPressed: onEdit,
          icon: const Icon(Icons.edit_outlined),
        ),
        IconButton(
          tooltip: '删除配置',
          onPressed: onDelete,
          icon: const Icon(Icons.delete_outline),
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
              const Chip(
                visualDensity: VisualDensity.compact,
                avatar: Icon(Icons.check_circle, size: 18, color: Colors.blue),
                label: Text('当前使用'),
              ),
          ],
        ),
        const SizedBox(height: 3),
        Text(
          description,
          style: const TextStyle(color: Colors.blueGrey),
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
        ),
        const SizedBox(height: 8),
        StorageUsageView(value: usage, connected: online, onRefresh: onRefresh),
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
