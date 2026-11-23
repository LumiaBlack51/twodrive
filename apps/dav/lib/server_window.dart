import 'dart:async';
import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';
import 'client.dart';
import 'language.dart';
import 'sharing_page.dart';

class ServerWindow extends StatefulWidget {
  const ServerWindow({super.key, required this.client, required this.desktop});
  final StorageClient client;
  final bool desktop;
  @override
  State<ServerWindow> createState() => _ServerWindowState();
}

class _ServerWindowState extends State<ServerWindow> with WindowListener {
  bool closing = false;
  @override
  void initState() {
    super.initState();
    if (widget.desktop) {
      windowManager.addListener(this);
      unawaited(windowManager.setPreventClose(true));
    }
  }

  Future<void> refresh() async {
    final c = widget.client;
    if (!c.available || c.busy || c.quitting) return;
    try {
      await c.refreshStatus();
    } catch (_) {
      // refreshStatus publishes the error for this window.
    }
  }

  @override
  void onWindowFocus() => unawaited(refresh());

  @override
  void onWindowClose() async {
    if (closing) return;
    closing = true;
    try {
      if (widget.client.hosting) {
        final stop = await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: Text(uiText('停止共享并退出？', 'Stop sharing and quit?')),
            content: Text(
              uiText('进行中的传输将中断。', 'Active transfers will be interrupted.'),
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context, false),
                child: Text(tr('取消')),
              ),
              FilledButton(
                onPressed: () => Navigator.pop(context, true),
                child: Text(tr('停止共享')),
              ),
            ],
          ),
        );
        if (stop != true) return;
      }
      if (await widget.client.shutdown()) {
        await windowManager.setPreventClose(false);
        await windowManager.destroy();
      }
    } finally {
      closing = false;
    }
  }

  @override
  void dispose() {
    if (widget.desktop) windowManager.removeListener(this);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.client,
    builder: (_, _) => Scaffold(
      appBar: AppBar(
        title: const Text('TDLink Server'),
        actions: [
          IconButton(
            tooltip: tr('刷新状态'),
            onPressed: widget.client.available && !widget.client.busy
                ? refresh
                : null,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: Column(
        children: [
          if (widget.client.error != null) Text(tr(widget.client.error!)),
          Expanded(child: SharingPage(client: widget.client)),
        ],
      ),
    ),
  );
}
