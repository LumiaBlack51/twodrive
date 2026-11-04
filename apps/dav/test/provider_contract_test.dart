import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/profile_dialog.dart';
import 'package:twodrive_dav/tray_panel.dart';
import 'package:twodrive_dav/connection_card.dart';

void main() {
  testWidgets(
    'connected account can open add dialog and edit inactive accounts',
    (tester) async {
      tester.view.physicalSize = const Size(1100, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = StorageClient()
        ..available = true
        ..selected = 'active';
      c.status = {'connected': true, 'profile': 'active'};
      c.profiles = [
        {'id': 'active', 'name': 'Active account', 'kind': 'example'},
        {'id': 'other', 'name': 'Other account', 'kind': 'example'},
      ];
      c.providers = [
        {'id': 'example', 'label': 'Example storage', 'fields': []},
      ];
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      final add = tester.widget<FilledButton>(
        find.widgetWithText(FilledButton, '添加连接'),
      );
      expect(add.onPressed, isNotNull);
      await tester.tap(find.text('添加连接'));
      await tester.pumpAndSettle();
      expect(find.byType(ProfileDialog), findsOneWidget);
      expect(c.connected, isTrue);
      expect(c.selected, 'active');
      await tester.tap(find.text('取消'));
      await tester.pumpAndSettle();
      final inactive = find.ancestor(
        of: find.text('Other account'),
        matching: find.byType(ConnectionCard),
      );
      final edit = find.descendant(
        of: inactive,
        matching: find.byIcon(Icons.edit_outlined),
      );
      await tester.tap(edit);
      await tester.pumpAndSettle();
      expect(find.text('编辑连接'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets('new provider schema works without DAV or peer fields', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1000, 1100);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    Json? saved;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async {
              saved = await showDialog<Json>(
                context: context,
                builder: (_) => const ProfileDialog(
                  providers: [
                    {
                      'id': 'bucket',
                      'label': 'Example Storage',
                      'fields': [
                        {'key': 'bucket', 'label': '存储空间', 'required': true},
                        {
                          'key': 'access_token',
                          'label': '访问凭据',
                          'secret': true,
                        },
                      ],
                    },
                  ],
                  original: {
                    'id': 'p',
                    'kind': 'bucket',
                    'name': 'Account',
                    'bucket': 'sample',
                    'mount_path': '/tmp/example',
                    'network_route': 'auto',
                  },
                ),
              );
            },
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    expect(find.text('Example Storage'), findsOneWidget);
    expect(find.text('存储空间'), findsOneWidget);
    expect(find.textContaining('WebDAV'), findsNothing);
    expect(find.textContaining('配对状态'), findsNothing);
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(saved?['kind'], 'bucket');
    expect(saved?['bucket'], 'sample');
    expect(saved!.containsKey('url'), isFalse);
    expect(saved!.containsKey('peer_state'), isFalse);
  });
  testWidgets(
    'capabilities independently control file operations and provider labels',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = StorageClient()
        ..available = true
        ..selected = 'p';
      c.profiles = [
        {
          'id': 'p',
          'kind': 'bucket',
          'name': 'Example',
          'provider_label': 'Example Storage',
          'account_name': 'bucket account',
          'writable': true,
        },
      ];
      c.status = {
        'connected': true,
        'capabilities': {
          'upload': true,
          'conditional_upload': false,
          'rename': true,
          'delete': false,
          'mkdir': false,
          'backup': false,
          'mount': false,
        },
      };
      c.entries = [
        {
          'name': 'sample',
          'path': '/sample',
          'directory': false,
          'size': 3,
          'etag': 'version',
        },
      ];
      await tester.pumpWidget(DavApp(client: c));
      expect(find.text('上传'), findsNothing);
      expect(find.byTooltip('新建文件夹'), findsNothing);
      await tester.tap(find.byType(PopupMenuButton<String>));
      await tester.pumpAndSettle();
      expect(find.text('重命名'), findsOneWidget);
      expect(find.text('删除'), findsNothing);
      expect(find.text('替换内容'), findsNothing);
      await tester.pumpWidget(
        MaterialApp(
          home: TrayPanel(
            client: c,
            onManage: ([int p = 0]) async {},
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      );
      expect(find.text('Example Storage'), findsOneWidget);
      expect(find.text('账号 · bucket account'), findsOneWidget);
      expect(find.text('WebDAV'), findsNothing);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'successful proxy fallback uses warning triangle and native warning asset',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final icons = <String>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      const tray = MethodChannel('twodrive/status_tray');
      const window = MethodChannel('window_manager');
      messenger.setMockMethodCallHandler(tray, (call) async {
        if (call.method == 'configure') {
          icons.add((call.arguments as Map)['icon'] as String);
        }
        return null;
      });
      messenger.setMockMethodCallHandler(
        window,
        (call) async => call.method.startsWith('is') ? false : null,
      );
      addTearDown(() {
        messenger.setMockMethodCallHandler(tray, null);
        messenger.setMockMethodCallHandler(window, null);
      });
      final c = StorageClient()..available = true;
      c.status = {
        'connected': true,
        'route_fallback': true,
        'network': ['network: route=system/proxy fallback'],
      };
      await tester.pumpWidget(DavApp(client: c, desktop: true));
      await tester.pumpAndSettle();
      expect(
        icons.last,
        endsWith('cloud-warning.${Platform.isWindows ? 'ico' : 'svg'}'),
      );
      expect(find.byIcon(Icons.warning_amber_rounded), findsOneWidget);
      expect(find.byIcon(Icons.error_outline), findsNothing);
      await tester.pumpWidget(
        MaterialApp(
          home: TrayPanel(
            client: c,
            onManage: ([int p = 0]) async {},
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      );
      expect(find.byIcon(Icons.warning_amber_rounded), findsOneWidget);
      expect(find.byIcon(Icons.error), findsNothing);
      c.error = '同步失败';
      await tester.pumpWidget(DavApp(client: c, desktop: true));
      await tester.pumpAndSettle();
      expect(
        icons.last,
        endsWith('cloud-error.${Platform.isWindows ? 'ico' : 'svg'}'),
      );
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  test('warning is connected fallback only; actual errors retain priority', () {
    final c = StorageClient();
    c.status = {'connected': true, 'route_fallback': true, 'paused': true};
    expect(c.trayState, 'warning');
    c.status['backup_failed'] = true;
    expect(c.trayState, 'error');
    c.status = {'connected': false, 'route_fallback': true};
    expect(c.trayState, 'offline');
    c.status = {
      'connected': true,
      'network': ['text route=system/proxy fallback'],
    };
    expect(c.trayState, 'connected');
    c.status = {'connected': true};
    expect(c.trayState, 'connected');
    c.dispose();
  });
}
