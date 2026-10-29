import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/mount_card.dart';
import 'package:twodrive_dav/tray_panel.dart';
import 'ui_preview.dart';

class MountClient extends StorageClient {
  MountClient({bool installed = true}) {
    available = true;
    selected = 'a';
    profiles = [
      {
        'id': 'a',
        'revision': 'rev-a',
        'kind': 'example',
        'name': 'Example 网盘',
        'mount_path': r'C:\TwoDrive\Example',
      },
    ];
    status = {
      'profile': 'a',
      'connected': true,
      'mounted': false,
      'capabilities': {'mount': installed},
      'mount_support': {
        'supported': true,
        'platform': 'windows',
        'readonly': true,
        'driver_installed': installed,
      },
    };
  }
  final calls = <Json>[];
  final opened = <String>[];
  @override
  Future<void> openMountFolder(
    String id, {
    Future<void> Function(String)? openDirectory,
  }) => super.openMountFolder(
    id,
    openDirectory:
        openDirectory ??
        (path) async {
          opened.add(path);
        },
  );
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add({'op': op, ...args});
    if (op == 'mount' || op == 'unmount') {
      final owner = profiles.singleWhere((p) => p['id'] == args['profile']);
      final rows = mounts.where((m) => m['profile'] != owner['id']).toList();
      if (op == 'mount') {
        rows.add({
          'profile': owner['id'],
          'revision': owner['revision'],
          'name': owner['name'],
          'path': owner['mount_path'],
          'failed': false,
        });
      }
      status = {
        ...status,
        'mounts': rows,
        'mounted': rows.any((m) => m['profile'] == status['profile']),
      };
    }
    return op == 'status' ? status : {};
  }
}

void main() {
  setUpAll(loadPreviewFonts);
  testWidgets(
    'missing driver gives installation help without requesting mount',
    (tester) async {
      final c = MountClient(installed: false);
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.widgetWithText(OutlinedButton, '挂载'));
      await tester.tap(find.widgetWithText(OutlinedButton, '挂载'));
      await tester.pumpAndSettle();
      expect(find.text('安装挂载组件'), findsOneWidget);
      expect(find.text('https://winfsp.dev/rel/'), findsOneWidget);
      expect(c.calls, isEmpty);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'mount and unmount carry profile revision from the connection card',
    (tester) async {
      tester.view.physicalSize = const Size(840, 580);
      tester.view.devicePixelRatio = 1;
      tester.platformDispatcher.textScaleFactorTestValue = 1.4;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      addTearDown(tester.platformDispatcher.clearTextScaleFactorTestValue);
      final c = MountClient();
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      expect(find.widgetWithText(TextButton, '挂载'), findsNothing);
      await tester.ensureVisible(find.widgetWithText(OutlinedButton, '挂载'));
      await tester.tap(find.widgetWithText(OutlinedButton, '挂载'));
      await tester.pumpAndSettle();
      expect(c.calls.where((v) => v['op'] == 'mount').single, {
        'op': 'mount',
        'profile': 'a',
        'revision': 'rev-a',
      });
      expect(find.widgetWithText(OutlinedButton, '卸载'), findsOneWidget);
      await tester.ensureVisible(find.widgetWithText(OutlinedButton, '卸载'));
      await tester.pumpAndSettle();
      await tester.tap(find.widgetWithText(OutlinedButton, '卸载'));
      await tester.pumpAndSettle();
      expect(c.calls.where((v) => v['op'] == 'unmount').single, {
        'op': 'unmount',
        'profile': 'a',
        'revision': 'rev-a',
      });
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets('mount card explains editing and disables absent driver', (
    tester,
  ) async {
    final c = MountClient(installed: false);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: MountCard(client: c, onOpen: () {}),
          ),
        ),
      ),
    );
    expect(find.text('文件管理器挂载'), findsOneWidget);
    final button = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, '挂载'),
    );
    expect(button.onPressed, isNull);
    expect(find.textContaining('网盘回收站'), findsOneWidget);
    expect(find.textContaining('待上传完成'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });
  testWidgets(
    'a retained mount can still be unmounted when its connection fails',
    (tester) async {
      final c = MountClient();
      c.status = {...c.status, 'connected': false, 'mounted': true};
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SingleChildScrollView(
              child: MountCard(client: c, onOpen: () {}),
            ),
          ),
        ),
      );
      final button = tester.widget<FilledButton>(
        find.widgetWithText(FilledButton, '卸载'),
      );
      expect(button.onPressed, isNotNull);
      await tester.tap(find.widgetWithText(FilledButton, '卸载'));
      await tester.pumpAndSettle();
      expect(c.calls.where((v) => v['op'] == 'unmount').single, {
        'op': 'unmount',
        'profile': 'a',
        'revision': 'rev-a',
      });
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'each account mounts and opens independently while all folders remain visible',
    (tester) async {
      tester.view.physicalSize = const Size(1000, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = MountClient();
      c.profiles.add({
        'id': 'b',
        'revision': 'rev-b',
        'kind': 'example',
        'name': 'Second 网盘',
        'mount_path': r'D:\TwoDrive\Second',
      });
      c.status = {
        ...c.status,
        'multi_connection': true,
        'connections': [
          {'id': 'a'},
          {'id': 'b'},
        ],
        'mounts': <Json>[],
      };
      await tester.pumpWidget(
        RepaintBoundary(
          key: const ValueKey('appearance-preview'),
          child: DavApp(client: c),
        ),
      );
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      final b = find.byKey(const ValueKey('connection-b'));
      final a = find.byKey(const ValueKey('connection-a'));
      final bMount = find.descendant(
        of: b,
        matching: find.widgetWithText(OutlinedButton, '挂载'),
      );
      await tester.ensureVisible(bMount);
      await tester.tap(bMount);
      await tester.pumpAndSettle();
      expect(c.calls.where((v) => v['op'] == 'mount').single, {
        'op': 'mount',
        'profile': 'b',
        'revision': 'rev-b',
      });
      expect(c.selected, 'a');
      final aMount = find.descendant(
        of: a,
        matching: find.widgetWithText(OutlinedButton, '挂载'),
      );
      await tester.ensureVisible(aMount);
      await tester.tap(aMount);
      await tester.pumpAndSettle();
      expect(c.mounts.length, 2);
      tester.view.physicalSize = const Size(1400, 1000);
      await tester.pumpAndSettle();
      await appearanceSnapshot(tester, 'multi-mount-connections');
      final footer = find.byKey(const ValueKey('mounted-folders'));
      expect(
        find.descendant(
          of: footer,
          matching: find.textContaining(r'C:\TwoDrive\Example'),
        ),
        findsOneWidget,
      );
      expect(
        find.descendant(
          of: footer,
          matching: find.textContaining(r'D:\TwoDrive\Second'),
        ),
        findsOneWidget,
      );
      final openB = find.descendant(
        of: b,
        matching: find.widgetWithText(OutlinedButton, '打开挂载'),
      );
      final viewB = find.descendant(
        of: b,
        matching: find.widgetWithText(OutlinedButton, '查看文件'),
      );
      expect(
        tester.getTopLeft(openB).dy,
        greaterThan(tester.getBottomLeft(viewB).dy),
      );
      await tester.ensureVisible(openB);
      await tester.tap(openB);
      await tester.pumpAndSettle();
      expect(c.opened, [r'D:\TwoDrive\Second']);
      expect(c.selected, 'a');
      expect(c.calls.where((v) => v['op'] == 'connect'), isEmpty);
      await tester.tap(find.text('设置').first);
      await tester.pumpAndSettle();
      expect(find.byKey(const ValueKey('mount-a')), findsOneWidget);
      expect(find.byKey(const ValueKey('mount-b')), findsOneWidget);
      final unmountB = find.descendant(
        of: find.byKey(const ValueKey('mount-b')),
        matching: find.widgetWithText(FilledButton, '卸载'),
      );
      await tester.ensureVisible(unmountB);
      await tester.tap(unmountB);
      await tester.pumpAndSettle();
      expect(c.isProfileMounted('a'), true);
      expect(c.isProfileMounted('b'), false);
      expect(c.selected, 'a');
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
    variant: TargetPlatformVariant({TargetPlatform.windows}),
  );
  test('stale mount revisions cannot open a different folder', () async {
    final c = MountClient();
    c.status = {
      ...c.status,
      'mounts': [
        {'profile': 'a', 'revision': 'stale', 'path': r'D:\wrong'},
      ],
    };
    await c.openMountFolder('a');
    expect(c.opened, isEmpty);
    expect(c.error, isNotNull);
    c.dispose();
  });
  testWidgets(
    'tray reports the failing background account while another account stays selected',
    (tester) async {
      final c = MountClient();
      c.profiles.add({
        'id': 'b',
        'revision': 'rev-b',
        'kind': 'example',
        'name': 'Second 网盘',
      });
      c.selected = 'b';
      c.status = {
        ...c.status,
        'profile': 'b',
        'multi_connection': true,
        'connections': [
          {'id': 'a'},
          {'id': 'b'},
        ],
        'overall': {
          'error': true,
          'syncing': true,
          'issues': [
            {'profile': 'a', 'account': 'Example 网盘', 'message': '备份需要处理'},
          ],
          'active': [
            {
              'profile': 'a',
              'revision': 'rev-a',
              'account': 'Example 网盘',
              'path': '/synthetic.txt',
              'kind': 'upload',
              'bytes_done': 1,
              'bytes_total': 3,
            },
          ],
        },
      };
      expect(c.syncError, true);
      expect(c.trayState, 'error');
      expect(c.syncActivity.single['profile'], 'a');
      await tester.pumpWidget(
        MaterialApp(
          home: TrayPanel(
            client: c,
            onManage: ([int page = 0]) async {},
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      );
      expect(find.textContaining('Example 网盘 · 备份需要处理'), findsOneWidget);
      expect(find.text('需要处理'), findsOneWidget);
      expect(c.selected, 'b');
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
}
