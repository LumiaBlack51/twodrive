import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/status_tray.dart';
import 'package:twodrive_dav/tray_panel.dart';

class SelectingClient extends StorageClient {
  String? requested;
  @override
  Future<void> connect([String? profileId]) async => requested = profileId;
}

void main() {
  testWidgets(
    'saved connection has an explicit login action and active marker',
    (tester) async {
      tester.view.physicalSize = const Size(840, 580);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = SelectingClient()..available = true;
      c.selected = 'p';
      c.profiles = [
        {
          'id': 'p',
          'name': 'My cloud',
          'kind': 'example',
          'provider_label': 'Example',
        },
        {
          'id': 'q',
          'name': 'Other cloud',
          'kind': 'example',
          'provider_label': 'Example',
        },
      ];
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      expect(find.text('登录并使用'), findsNWidgets(2));
      await tester.tap(find.text('登录并使用').first);
      expect(c.requested, 'p');
      c.status = {
        'connected': true,
        'profile': 'p',
        'account_name': 'Real signed in person',
      };
      c.notifyListeners();
      await tester.pumpAndSettle();
      expect(find.text('当前使用'), findsOneWidget);
      expect(find.text('切换使用'), findsOneWidget);
      expect(find.textContaining('账号：Real signed in person'), findsOneWidget);
      await tester.ensureVisible(find.text('切换使用'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('切换使用'));
      expect(c.requested, 'q');
      // A click cannot relabel old files as the new account before the engine
      // confirms the guarded switch.
      expect(c.selected, 'p');
      expect(tester.takeException(), isNull);
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
      expect(find.text('账号 · Real signed in person'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets('native tray receives the active account on each update', (
    tester,
  ) async {
    final titles = <String>[];
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(StatusTray.channel, (call) async {
      titles.add((call.arguments as Map)['title'] as String);
      return null;
    });
    addTearDown(
      () => messenger.setMockMethodCallHandler(StatusTray.channel, null),
    );
    final tray = StatusTray();
    await tray.icon('connected', title: 'TwoDrive · Real account');
    await tray.icon('offline', title: 'TwoDrive Development · 未连接');
    expect(titles, ['TwoDrive · Real account', 'TwoDrive Development · 未连接']);
  });
}
