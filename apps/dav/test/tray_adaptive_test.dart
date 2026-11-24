import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/tray_panel.dart';
import 'package:twodrive_dav/language.dart';

void main() {
  testWidgets(
    'tray footer stays anchored through account error and quota updates',
    (tester) async {
      final c = StorageClient()
        ..available = true
        ..selected = 'peer'
        ..profiles = [
          {'id': 'peer', 'kind': 'peer', 'name': 'Windows'},
        ]
        ..status = {'connected': true, 'profile': 'peer'};
      desktopLocale.value = 'en';
      addTearDown(() {
        desktopLocale.value = 'zh';
        c.dispose();
      });
      Future<void> draw() => tester.pumpWidget(
        MaterialApp(
          home: TrayPanel(
            client: c,
            onManage: ([int page = 0]) async {},
            onOpenFolder: () async {},
            onQuit: () async {},
          ),
        ),
      );
      await draw();
      final footer = find.text('TwoDrive').last;
      final bounds = tester.getRect(footer);
      c.status['overall'] = {
        'issues': [
          {'account': 'Google Drive', 'message': '备份需要处理，请查看该网盘的备份状态'},
        ],
      };
      c.quotas['peer'] = {
        'state': 'ready',
        'usage': {'scope': 'share', 'total': 100, 'used': 40, 'free': 60},
      };
      await draw();
      expect(
        find.text('Google Drive · Check the backup status for this drive.'),
        findsOneWidget,
      );
      expect(tester.getRect(footer), bounds);
      expect(
        tester.getRect(find.text('Open folder')).bottom,
        lessThan(bounds.top),
      );
    },
  );
  testWidgets('short tray keeps actions visible while its details scroll', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 320);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final client = StorageClient()
      ..available = true
      ..selected = 'fixture'
      ..profiles = [
        {'id': 'fixture', 'kind': 'baidu', 'name': '示例网盘'},
      ]
      ..status = {'connected': true, 'profile': 'fixture', 'paused': true}
      ..error = '测试提示：此条消息需要在较小的可用区域内显示，底部操作仍应可用。';
    client.quotas['fixture'] = {
      'state': 'ready',
      'usage': {
        'total': 1000000000000,
        'used': 300000000000,
        'free': 700000000000,
      },
    };
    var opened = false;
    await tester.pumpWidget(
      MaterialApp(
        builder: (context, child) => MediaQuery(
          data: MediaQuery.of(
            context,
          ).copyWith(textScaler: TextScaler.linear(1.6)),
          child: child!,
        ),
        home: TrayPanel(
          client: client,
          onRefresh: () {},
          onManage: ([int page = 0]) async {},
          onOpenFolder: () async {
            opened = true;
          },
          onQuit: () async {},
        ),
      ),
    );
    expect(tester.takeException(), isNull);
    expect(find.byTooltip('刷新状态'), findsOneWidget);
    expect(find.byTooltip('刷新空间信息'), findsNothing);
    for (final label in ['打开文件夹', '查看文件']) {
      final bounds = tester.getRect(find.text(label));
      expect(bounds.bottom, lessThanOrEqualTo(320));
      expect(bounds.left, greaterThanOrEqualTo(0));
      expect(bounds.right, lessThanOrEqualTo(320));
    }
    await tester.tap(find.text('打开文件夹'));
    await tester.pump();
    expect(opened, isTrue);
    await tester.drag(find.byType(Scrollable).first, const Offset(0, -400));
    await tester.pumpAndSettle();
    expect(find.text('查看文件').hitTestable(), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });
}
