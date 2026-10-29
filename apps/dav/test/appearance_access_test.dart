import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/app_theme.dart';
import 'package:twodrive_dav/profile_dialog.dart';
import 'package:twodrive_dav/tray_panel.dart';
import 'ui_preview.dart';

StorageClient connection({
  bool adapterWrites = false,
  bool configuredWrites = false,
  Json capabilities = const {},
}) => StorageClient()
  ..available = true
  ..selected = 'example'
  ..providers = [
    {
      'id': 'example',
      'label': 'Example 网盘',
      'fields': <Json>[],
      'capabilities': {
        'upload': adapterWrites,
        'conditional_upload': adapterWrites,
        'mkdir': adapterWrites,
        'rename': adapterWrites,
        'delete': adapterWrites,
      },
    },
  ]
  ..profiles = [
    {
      'id': 'example',
      'kind': 'example',
      'name': 'Example 网盘',
      'provider_label': 'Example 网盘',
      'writable': configuredWrites,
    },
  ]
  ..status = {
    'connected': true,
    'profile': 'example',
    'capabilities': capabilities,
  };

void main() {
  setUpAll(loadPreviewFonts);
  testWidgets(
    'Windows text roles share a Chinese and Latin font family',
    (tester) async {
      final c = connection();
      await tester.pumpWidget(DavApp(client: c));
      final theme = Theme.of(tester.element(find.text('TwoDrive')));
      for (final style in [
        theme.textTheme.headlineLarge,
        theme.textTheme.titleMedium,
        theme.textTheme.bodyMedium,
        theme.textTheme.labelLarge,
      ]) {
        expect(style!.fontFamily, 'Microsoft YaHei UI');
        expect(style.fontFamilyFallback, contains('Microsoft YaHei'));
      }
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
    variant: TargetPlatformVariant({TargetPlatform.windows}),
  );

  testWidgets('large text header has no local access controls', (tester) async {
    tester.platformDispatcher.textScaleFactorTestValue = 1.4;
    addTearDown(tester.platformDispatcher.clearTextScaleFactorTestValue);
    tester.view.physicalSize = const Size(840, 580);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final c = connection(
      adapterWrites: true,
      capabilities: {
        'upload': true,
        'conditional_upload': true,
        'rename': true,
      },
    );
    await tester.pumpWidget(DavApp(client: c));
    expect(find.byKey(const ValueKey('connection-access')), findsNothing);
    expect(find.text('只读'), findsNothing);
    expect(find.text('断开'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });

  testWidgets(
    'shared typography covers connections, form and tray',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = connection();
      c.profiles[0]['name'] = 'iCloud Drive';
      c.profiles[0]['provider_label'] = 'iCloud Drive';
      c.profiles.add({
        'id': 'other',
        'kind': 'example',
        'name': 'OneDrive',
        'provider_label': 'OneDrive',
        'account_name': '示例账号 Example',
      });
      c.status.addAll({
        'multi_connection': true,
        'connections': [
          {'id': 'example'},
          {'id': 'other'},
        ],
      });
      for (final p in c.profiles) {
        c.quotas[p['id']] = {
          'state': 'ready',
          'usage': {
            'total': 10737418240,
            'used': 4294967296,
            'free': 6442450944,
          },
        };
      }
      await tester.pumpWidget(
        RepaintBoundary(
          key: const ValueKey('appearance-preview'),
          child: DavApp(client: c),
        ),
      );
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      await appearanceSnapshot(tester, 'ui-font-connections');
      await tester.tap(find.text('添加连接'));
      await tester.pumpAndSettle();
      expect(find.byType(ProfileDialog), findsOneWidget);
      final formTheme = Theme.of(tester.element(find.byType(ProfileDialog)));
      expect(formTheme.textTheme.bodyLarge!.fontFamily, 'Microsoft YaHei UI');
      await appearanceSnapshot(tester, 'ui-font-form');
      await tester.pumpWidget(const SizedBox());
      tester.view.physicalSize = const Size(390, 600);
      await tester.pumpWidget(
        RepaintBoundary(
          key: const ValueKey('appearance-preview'),
          child: MaterialApp(
            debugShowCheckedModeBanner: false,
            theme: appTheme(),
            home: TrayPanel(
              client: c,
              onManage: ([int page = 0]) async {},
              onOpenFolder: () async {},
              onQuit: () async {},
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      await appearanceSnapshot(tester, 'ui-font-tray');
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
    variant: TargetPlatformVariant({TargetPlatform.windows}),
  );
}
