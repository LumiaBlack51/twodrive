import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/overview_page.dart';
import 'package:twodrive_dav/workspace_navigation.dart';

class OverviewClient extends StorageClient {
  int refreshes = 0;
  final calls = <String>[];
  OverviewClient() {
    available = true;
    selected = 'a';
    profiles = [
      {'id': 'a', 'revision': 'ra', 'name': 'Baidu', 'kind': 'baidu'},
      {'id': 'b', 'revision': 'rb', 'name': 'OneDrive', 'kind': 'onedrive'},
    ];
    status = {
      'connected': true,
      'profile': 'a',
      'multi_connection': true,
      'connections': [
        {'id': 'a'},
        {'id': 'b'},
      ],
      'mounts': [
        {
          'profile': 'a',
          'revision': 'ra',
          'name': 'Baidu',
          'path': r'D:\Baidu',
        },
        {
          'profile': 'b',
          'revision': 'rb',
          'name': 'OneDrive',
          'path': r'D:\OneDrive',
        },
        {
          'profile': 'b',
          'revision': 'outdated',
          'name': 'Old',
          'path': r'D:\Old',
        },
      ],
      'cache': {'bytes': 1048576, 'pending': 0},
      'cfapi': {
        'configured': true,
        'running': true,
        'root': r'C:\Native',
        'progress': {
          'phase': 'sync',
          'summary': {
            'catalogue_files': 120,
            'catalogue_directories': 8,
            'durable_work_items': 0,
          },
        },
      },
    };
  }
  @override
  Future<void> refreshStatus() async {
    refreshes++;
    notifyListeners();
  }

  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add(op);
    return {};
  }
}

void main() {
  testWidgets(
    'working native heartbeat does not present old zero work as completion',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = OverviewClient();
      c.status['cfapi']['progress']['phase'] = 'working';
      c.status['cfapi']['progress']['stage'] = 'remote';
      await tester.pumpWidget(DavApp(client: c));
      expect(find.text('正在更新云端目录'), findsOneWidget);
      expect(find.textContaining('处理中'), findsOneWidget);
      expect(find.text('本轮尚未完成，统计来自最近一次检查。'), findsOneWidget);
      expect(find.textContaining('原生待处理'), findsNothing);
      c.status['cfapi']['progress']['phase'] = 'sync';
      c.notifyListeners();
      await tester.pump();
      expect(find.textContaining('原生待处理'), findsOneWidget);
      expect(find.text('正在更新云端目录'), findsNothing);
      expect(c.calls, isEmpty);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  void viewport(WidgetTester tester, Size size) {
    tester.view.physicalSize = size;
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
  }

  testWidgets(
    'startup opens the overview and status polling does no directory or quota IO',
    (tester) async {
      viewport(tester, const Size(1120, 760));
      final c = OverviewClient()..setView(visible: true);
      await tester.pumpWidget(DavApp(client: c));
      expect(find.byKey(const ValueKey('sync-overview')), findsOneWidget);
      expect(find.text('2 个网盘已连接 · 0 项活动传输'), findsOneWidget);
      expect(find.text(r'D:\Old'), findsNothing);
      await tester.pump(const Duration(seconds: 10));
      await tester.pump();
      expect(c.refreshes, 1);
      expect(c.calls, isEmpty);
      c.setView(visible: false);
      await tester.pump(const Duration(seconds: 20));
      expect(c.refreshes, 1);
      c.setView(visible: true);
      c.busy = true;
      await tester.pump(const Duration(seconds: 10));
      expect(c.refreshes, 1);
      c.busy = false;
      await tester.tap(find.text('文件').first);
      await tester.pumpAndSettle();
      await tester.pump(const Duration(seconds: 20));
      expect(c.refreshes, 1);
      expect(c.selected, 'a');
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'opening another mount preserves the browsing account and native errors remain visible',
    (tester) async {
      viewport(tester, const Size(1120, 900));
      final c = OverviewClient();
      c.status['cfapi']['progress']['remote'] = {
        'error': 'network_unavailable',
      };
      String? opened;
      int? destination;
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: OverviewPage(
              client: c,
              onNavigate: (v) => destination = v,
              onOpenMount: (id) async => opened = id,
            ),
          ),
        ),
      );
      expect(find.text('有状态需要处理'), findsOneWidget);
      expect(find.text('需要处理'), findsOneWidget);
      await tester.tap(
        find.descendant(
          of: find.byKey(const ValueKey('overview-account-b')),
          matching: find.text('打开目录'),
        ),
      );
      expect(opened, 'b');
      expect(c.selected, 'a');
      final native = find.text('管理原生同步');
      await tester.ensureVisible(native);
      await tester.tap(native);
      expect(destination, WorkspacePage.cfapi.index);
      expect(c.calls, isEmpty);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'minimum window has a scrollable overview with reachable policy and backup links',
    (tester) async {
      viewport(tester, const Size(840, 580));
      final c = OverviewClient();
      await tester.pumpWidget(DavApp(client: c));
      final button = find.text('缓存与排除规则');
      await tester.scrollUntilVisible(
        button,
        200,
        scrollable: find
            .descendant(
              of: find.byKey(const ValueKey('sync-overview')),
              matching: find.byType(Scrollable),
            )
            .first,
      );
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      await tester.tap(button);
      await tester.pumpAndSettle();
      expect(find.byKey(const ValueKey('sync-overview')), findsNothing);
      expect(c.selected, 'a');
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
}
