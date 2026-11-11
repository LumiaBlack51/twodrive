import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/tray_panel.dart';

void main() {
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
          onManage: ([int page = 0]) async {},
          onOpenFolder: () async {
            opened = true;
          },
          onQuit: () async {},
        ),
      ),
    );
    expect(tester.takeException(), isNull);
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
