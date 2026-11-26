import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/tray_panel.dart';
import 'package:twodrive_dav/workspace_navigation.dart';

class _TrayAccounts extends StorageClient {
  @override
  Future<void> connect([String? id]) async {
    selected = id;
    status['profile'] = id;
    status.remove('backup');
    status.remove('cache');
    notifyListeners();
  }
}

void main() {
  testWidgets('tray shows transfers and sync state for the selected account', (
    tester,
  ) async {
    final client = _TrayAccounts()
      ..available = true
      ..selected = 'work'
      ..profiles = [
        {'id': 'work', 'revision': 'new', 'kind': 'onedrive', 'name': 'Work'},
        {'id': 'home', 'revision': 'one', 'kind': 'onedrive', 'name': 'Home'},
        {'id': 'empty', 'revision': 'one', 'kind': 'google', 'name': 'Empty'},
      ];
    Map<String, Object> row(String name, {String? state}) => {
      'profile': 'work',
      'revision': 'new',
      'name': name,
      'path': '/$name',
      'kind': 'upload',
      'state': ?state,
    };
    final workBackup = {'state': 'scanning', 'folders': 1};
    final workCache = {'pending': 0};
    final active = <Map<String, Object>>[];
    final waiting = <Map<String, Object>>[];
    client.status = {
      'connected': true,
      'profile': 'work',
      'multi_connection': true,
      'connections': client.profiles,
      'accounts': [
        {
          'profile': 'work',
          'revision': 'new',
          'backup': workBackup,
          'cache': workCache,
        },
      ],
      'backup': workBackup,
      'overall': {
        'syncing': true,
        'active': active,
        'waiting': waiting,
        'recent': [
          row('work-backup.zip', state: '已备份'),
          {...row('obsolete-backup.zip', state: '已备份'), 'revision': 'old'},
        ],
      },
    };
    client.transfers.addAll([
      Transfer(1, 'download', {
        'profile': 'home',
        'revision': 'one',
        'path': '/home-download.zip',
      }),
      Transfer(2, 'upload', {
        'profile': 'work',
        'revision': 'old',
        'path': '/obsolete-upload.zip',
      })..phase = TransferPhase.completed,
    ]);
    addTearDown(client.dispose);
    await tester.pumpWidget(
      MaterialApp(
        home: ListenableBuilder(
          listenable: client,
          builder: (_, _) => TrayPanel(
            client: client,
            onManage: ([int page = 0]) async {},
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      ),
    );
    expect(find.text('work-backup.zip'), findsOneWidget);
    for (final other in [
      'home-download.zip',
      'obsolete-upload.zip',
      'obsolete-backup.zip',
    ]) {
      expect(find.text(other), findsNothing);
    }
    expect(find.text('正在扫描备份'), findsOneWidget);

    waiting.add(row('waiting.zip', state: '等待同步'));
    workCache['pending'] = 1;
    client.notifyListeners();
    await tester.pump();
    expect(find.text('waiting.zip'), findsOneWidget);
    expect(find.text('正在同步'), findsNothing);
    expect(find.text('等待同步'), findsNWidgets(2));
    active.add(row('waiting.zip'));
    client.notifyListeners();
    await tester.pump();
    expect(find.text('waiting.zip'), findsOneWidget);
    expect(find.text('正在同步'), findsOneWidget);

    active.clear();
    waiting.clear();
    workCache['pending'] = 0;
    workBackup['state'] = 'idle';
    workBackup['completed_scan'] = true;
    client.notifyListeners();
    await tester.pump();
    expect(find.text('已备份和同步'), findsOneWidget);

    Future<void> switchTo(String name) async {
      await tester.tap(find.byKey(const ValueKey('cloud-switcher')));
      // The current account has an indeterminate transfer bar when selected.
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await tester.tap(find.text(name));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
    }

    await switchTo('Home');
    expect(find.text('work-backup.zip'), findsNothing);
    expect(find.text('home-download.zip'), findsOneWidget);
    expect(find.text('正在同步'), findsOneWidget);
    await switchTo('Empty');
    expect(find.text('home-download.zip'), findsNothing);
    expect(find.text('文件已就绪'), findsOneWidget);
    expect(find.text('暂无传输记录'), findsOneWidget);
    client.busy = true;
    client.notifyListeners();
    await tester.pump();
    expect(find.text('文件已就绪'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('scrolling tray history keeps account and actions fixed', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(390, 570);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final client = StorageClient()
      ..selected = 'work'
      ..profiles = [
        {'id': 'work', 'revision': 'one', 'kind': 'onedrive', 'name': 'Work'},
      ]
      ..status = {'connected': true, 'profile': 'work'};
    addTearDown(client.dispose);
    client.quotas['work'] = {
      'usage': {'used': 50, 'total': 100, 'free': 50},
    };
    for (var index = 0; index < 30; index++) {
      client.transfers.add(
        Transfer(index, 'download', {
          'profile': 'work',
          'revision': 'one',
          'path': '/file-$index.zip',
        })..phase = TransferPhase.completed,
      );
    }
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
    final fixed = [
      find.byKey(const ValueKey('cloud-switcher')),
      find.text('文件已就绪'),
      find.text('账号空间'),
      find.byKey(const ValueKey('tray-actions')),
    ];
    final before = fixed.map(tester.getRect).toList();
    final scroll = find.byType(Scrollable);
    await tester.drag(scroll, const Offset(0, -430));
    await tester.pumpAndSettle();
    expect(
      tester.state<ScrollableState>(scroll).position.pixels,
      greaterThan(0),
    );
    for (final (index, widget) in fixed.indexed) {
      expect(widget.hitTestable(), findsOneWidget);
      expect(tester.getRect(widget), before[index]);
    }
    expect(find.text('file-0.zip').hitTestable(), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });

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
      expect(labels, ['仪表盘', '文件', '传输', '连接', '备份', '设置']);
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
