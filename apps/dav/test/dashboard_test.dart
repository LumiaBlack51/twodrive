import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/main.dart';
import 'overview_test.dart' show OverviewClient;
import 'ui_preview.dart';

class DashboardClient extends OverviewClient {
  @override
  Future<void> pauseToggle() async {
    calls.add('pause');
    status['paused'] = !paused;
    notifyListeners();
  }
}

void main() {
  setUpAll(loadPreviewFonts);
  testWidgets(
    'dashboard uses account quotas, live activity and the existing pause control',
    (tester) async {
      tester.view.physicalSize = const Size(1440, 1080);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = DashboardClient();
      c.quotas['a'] = {
        'usage': {
          'used': 3 * 1024 * 1024 * 1024,
          'total': 12 * 1024 * 1024 * 1024,
        },
      };
      c.status['overall'] = {
        'active': [
          {
            'profile': 'a',
            'revision': 'ra',
            'name': 'test-document.txt',
            'kind': 'upload',
            'bytes_done': 100,
            'bytes_total': 1000,
            'account': 'Baidu',
          },
        ],
      };
      await tester.pumpWidget(DavApp(client: c));
      expect(find.text('仪表盘'), findsNWidgets(2));
      expect(find.text('3.0 GiB'), findsOneWidget);
      expect(find.text('总量 12.0 GiB'), findsOneWidget);
      expect(find.text('容量尚未获取'), findsOneWidget);
      expect(find.text('2 个网盘已连接 · 1 项活动传输'), findsOneWidget);
      expect(find.text('test-document.txt'), findsOneWidget);
      expect(find.text('正在同步'), findsOneWidget);
      await tester.tap(find.byType(Switch));
      await tester.pump();
      expect(c.calls, ['pause']);
      expect(c.selected, 'a');
      expect(find.text('同步已暂停'), findsOneWidget);
      expect(c.status['cfapi']['running'], true);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
    variant: TargetPlatformVariant({TargetPlatform.windows}),
  );

  for (final size in [const Size(1440, 1000), const Size(840, 580)]) {
    testWidgets(
      'dashboard visual layout ${size.width.toInt()}',
      (tester) async {
        tester.view.physicalSize = size;
        tester.view.devicePixelRatio = 1;
        addTearDown(tester.view.resetPhysicalSize);
        addTearDown(tester.view.resetDevicePixelRatio);
        final c = DashboardClient();
        c.profiles = [
          {'id': 'a', 'revision': 'ra', 'name': 'OneDrive', 'kind': 'onedrive'},
          {
            'id': 'b',
            'revision': 'rb',
            'name': 'iCloud Drive',
            'kind': 'icloud',
          },
          {'id': 'c', 'revision': 'rc', 'name': '百度网盘', 'kind': 'baidu'},
          {
            'id': 'd',
            'revision': 'rd',
            'name': 'Google Drive',
            'kind': 'gdrive',
          },
        ];
        c.status['connections'] = [
          for (final p in c.profiles) {'id': p['id']},
        ];
        c.status['mounts'] = [
          for (final p in c.profiles)
            {
              'profile': p['id'],
              'revision': p['revision'],
              'path': 'X:/Preview/${p['name']}',
            },
        ];
        for (var i = 0; i < c.profiles.length; i++) {
          c.quotas[c.profiles[i]['id']] = {
            'usage': {
              'used': [25.8, 708.9, 3174.4, 151.3][i] * 1073741824,
              'total': [120, 2048, 14336, 5120][i] * 1073741824,
            },
          };
        }
        c.status['build'] = {'core_version': '0.2.24'};
        c.status['accounts'] = [
          {
            'name': 'OneDrive',
            'backup': {'folders': 1, 'state': 'idle', 'completed_scan': true},
          },
        ];
        await tester.pumpWidget(
          RepaintBoundary(
            key: const ValueKey('appearance-preview'),
            child: DavApp(client: c),
          ),
        );
        await tester.pump();
        expect(tester.takeException(), isNull);
        await appearanceSnapshot(tester, 'dashboard-${size.width.toInt()}');
        await tester.pumpWidget(const SizedBox());
        c.dispose();
      },
      variant: TargetPlatformVariant({TargetPlatform.windows}),
    );
  }
}
