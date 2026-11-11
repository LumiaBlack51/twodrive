import 'package:flutter/material.dart';

/// Order, labels and icons for the dashboard sidebar.
enum WorkspacePage {
  files('文件', Icons.folder_outlined),
  transfers('传输', Icons.swap_vert),
  connections('连接', Icons.dns_outlined),
  backup('备份', Icons.backup_outlined),
  sharing('共享', Icons.share_outlined),
  settings('设置', Icons.tune),
  cfapi('原生同步', Icons.cloud_sync_outlined),
  overview('仪表盘', Icons.dashboard_outlined);

  const WorkspacePage(this.label, this.icon);
  final String label;
  final IconData icon;

  static const sidebar = [
    overview,
    files,
    transfers,
    connections,
    backup,
    sharing,
    cfapi,
    settings,
  ];
}
