import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/tray_panel.dart';
import 'package:twodrive_dav/workspace_navigation.dart';

void main() {
  testWidgets(
    'tray Dashboard opens the workspace and retains its sidebar pages',
    (tester) async {
      tester.view.physicalSize = const Size(1120, 760);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final client = StorageClient();
      await tester.pumpWidget(DavApp(client: client));
      final navigation = tester
          .widgetList<ListTile>(
            find.descendant(
              of: find.byKey(const ValueKey('navigation')),
              matching: find.byType(ListTile),
            ),
          )
          .toList();
      final labels = navigation
          .map((item) => (item.title! as Text).data!)
          .toList();
      expect(labels, ['仪表盘', '文件', '传输', '连接', '备份', '共享', '原生同步', '设置']);
      int? destination;
      await tester.pumpWidget(
        MaterialApp(
          home: TrayPanel(
            client: client,
            onManage: ([int page = 0]) async {
              destination = page;
            },
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      );
      await tester.tap(find.byTooltip('设置'));
      await tester.pumpAndSettle();
      final items = tester
          .widgetList<PopupMenuItem<String>>(find.byType(PopupMenuItem<String>))
          .toList();
      expect(items.map((item) => item.value), ['dashboard', 'pause', 'quit']);
      expect(items.first.enabled, isTrue);
      expect(find.text('Dashboard'), findsOneWidget);
      expect(find.byIcon(Icons.dashboard_outlined), findsOneWidget);
      for (final label in labels) {
        expect(find.text(label), findsNothing);
      }
      await tester.tap(find.text('Dashboard'));
      await tester.pumpAndSettle();
      expect(destination, WorkspacePage.overview.index);
      expect(find.byType(PopupMenuItem<String>), findsNothing);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );

  testWidgets(
    'connected tray keeps Dashboard before sync and account actions',
    (tester) async {
      tester.view.physicalSize = const Size(390, 570);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final client = StorageClient()
        ..available = true
        ..busy = true
        ..status = {'connected': true, 'profile': 'fixture'};
      await tester.pumpWidget(
        MaterialApp(
          home: TrayPanel(
            client: client,
            onManage: ([int page = 0]) async {},
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      );
      await tester.tap(find.byTooltip('设置'));
      await tester.pumpAndSettle();
      final items = tester
          .widgetList<PopupMenuItem<String>>(find.byType(PopupMenuItem<String>))
          .toList();
      expect(items.map((item) => item.value), [
        'dashboard',
        'pause',
        'signout',
        'quit',
      ]);
      expect(items.first.enabled, isTrue);
      expect(items[1].enabled, isFalse);
      expect(find.text('暂停同步'), findsOneWidget);
      expect(find.text('退出登录…'), findsOneWidget);
      expect(find.text('退出 TwoDrive'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );
}
