import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/profile_dialog.dart';
import 'package:twodrive_dav/connection_card.dart';

void main() {
  testWidgets('new account saves editable without a mode or write password', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1100, 1000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    Json? saved;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) {
            return TextButton(
              onPressed: () async {
                saved = await showDialog<Json>(
                  context: context,
                  builder: (_) => const ProfileDialog(
                    providers: [
                      {'id': 'fixture', 'label': '测试网盘', 'fields': []},
                    ],
                  ),
                );
              },
              child: const Text('添加'),
            );
          },
        ),
      ),
    );
    await tester.tap(find.text('添加'));
    await tester.pumpAndSettle();
    expect(find.text('允许修改远端文件'), findsNothing);
    expect(find.textContaining('写入密码'), findsNothing);
    expect(find.text('只读'), findsNothing);
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(saved?['writable'], true);
    expect(saved?['kind'], 'fixture');
  });

  testWidgets('mounted account keeps independent actions without access pill', (
    tester,
  ) async {
    var opened = false;
    var unmounted = false;
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ConnectionCard(
            profile: const {'id': 'a', 'kind': 'fixture', 'name': '测试网盘'},
            online: true,
            selected: true,
            connectLabel: '查看文件',
            mounted: true,
            mountSupported: true,
            onSelect: () {},
            onOpenMount: () => opened = true,
            onMount: () => unmounted = true,
          ),
        ),
      ),
    );
    expect(find.text('只读'), findsNothing);
    expect(find.text('可编辑'), findsNothing);
    await tester.tap(find.text('打开挂载'));
    await tester.tap(find.text('卸载'));
    expect(opened, true);
    expect(unmounted, true);
  });
}
