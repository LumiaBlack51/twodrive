import 'language.dart';
import 'package:flutter/material.dart';

/// Order, labels and icons for the dashboard sidebar.
enum WorkspacePage {
  files('文件', 'Files', Icons.folder_outlined),
  transfers('传输', 'Transfers', Icons.swap_vert),
  connections('连接', 'Connections', Icons.dns_outlined),
  backup('备份', 'Backup', Icons.backup_outlined),
  settings('设置', 'Settings', Icons.tune),
  cfapi('原生同步', 'On-demand sync', Icons.cloud_sync_outlined),
  overview('仪表盘', 'Dashboard', Icons.dashboard_outlined);

  const WorkspacePage(this.chineseLabel, this.englishLabel, this.icon);
  final String chineseLabel;
  final String englishLabel;
  String get label => uiText(chineseLabel, englishLabel);
  final IconData icon;

  static const sidebar = [
    overview,
    files,
    transfers,
    connections,
    backup,
    settings,
  ];
}
