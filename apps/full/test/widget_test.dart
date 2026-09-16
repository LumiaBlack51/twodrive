import 'dart:io';
import 'dart:ui' as ui;
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_full/main.dart';
import 'package:twodrive_full/engine_client.dart';

Map<String, dynamic> fixture() => {
  'status': 'transferring',
  'mode': 'isolated_mock',
  'paused': false,
  'capabilities': ['pause', 'download', 'release'],
  'queued': 2,
  'file_count': 3,
  'active': {
    'name': 'Project-assets.zip',
    'direction': 'download',
    'done': 174063616,
    'total': 268435456,
    'outcome': 'running',
  },
  'recent': [
    {
      'name': 'meeting-notes.pdf',
      'direction': 'download',
      'done': 2097152,
      'total': 2097152,
      'outcome': 'completed',
    },
    {
      'name': '实验结果.xlsx',
      'direction': 'upload',
      'done': 1048576,
      'total': 1048576,
      'outcome': 'completed',
    },
  ],
  'files': [
    {
      'id': 'one',
      'name': 'Project-assets.zip',
      'state': 'online_only',
      'size': 268435456,
    },
    {
      'id': 'two',
      'name': 'meeting-notes.pdf',
      'state': 'cached',
      'size': 2097152,
    },
    {'id': 'three', 'name': '实验结果.xlsx', 'state': 'cached', 'size': 1048576},
  ],
};
void main() {
  setUpAll(() async {
    final icons = FontLoader('packages/fluent_ui/FluentIcons')
      ..addFont(rootBundle.load('packages/fluent_ui/fonts/FluentIcons.ttf'));
    await icons.load();
    for (final path in [
      'C:/Windows/Fonts/segoeui.ttf',
      'C:/Windows/Fonts/msyh.ttc',
    ]) {
      if (File(path).existsSync()) {
        final loader =
            FontLoader(
              path.endsWith('.ttc') ? 'Microsoft YaHei' : 'Segoe UI',
            )..addFont(
              Future.value(ByteData.sublistView(File(path).readAsBytesSync())),
            );
        await loader.load();
      }
    }
  });
  for (final compact in [true, false]) {
    testWidgets(
      '${compact ? 'tray' : 'management'} layout, navigation and pause',
      (tester) async {
        tester.view.physicalSize = compact
            ? const Size(392, 620)
            : const Size(1020, 740);
        tester.view.devicePixelRatio = 1;
        addTearDown(tester.view.resetPhysicalSize);
        addTearDown(tester.view.resetDevicePixelRatio);
        final state = fixture();
        final client = EngineClient(
          '',
          '',
          transport: (request) async {
            final command = request['command'] as Map;
            if (command['type'] == 'set_paused') {
              state['paused'] = command['paused'];
              state['status'] = 'paused';
            }
            return {
              'version': 1,
              'id': request['id'],
              'ok': true,
              'snapshot': state,
            };
          },
        )..snapshot = state;
        final key = GlobalKey();
        await tester.pumpWidget(
          RepaintBoundary(
            key: key,
            child: TwoDrive(client: client, compact: compact),
          ),
        );
        await tester.pumpAndSettle();
        expect(tester.takeException(), isNull);
        expect(find.text('Project-assets.zip'), findsOneWidget);
        if (Platform.environment['TWODRIVE_CAPTURE_UI'] == '1') {
          await tester.runAsync(() async {
            final boundary =
                key.currentContext!.findRenderObject()!
                    as RenderRepaintBoundary;
            final shot = await boundary.toImage();
            final png = await shot.toByteData(format: ui.ImageByteFormat.png);
            final file = File(
              '../../design/windows/flutter-${compact ? 'tray' : 'management'}-redesign.png',
            );
            await file.writeAsBytes(png!.buffer.asUint8List());
            shot.dispose();
          });
        }
        await tester.tap(find.text('暂停'));
        await tester.pumpAndSettle();
        expect(find.text('同步已暂停'), findsOneWidget);
        expect(find.text('继续'), findsOneWidget);
        if (!compact) {
          tester.view.physicalSize = const Size(850, 650);
          for (final label in [
            '文件',
            '账户',
            '同步设置',
            '存储与缓存',
            '设备',
            '帮助与诊断',
            '概览',
          ]) {
            await tester.tap(find.text(label).first);
            await tester.pumpAndSettle();
            expect(tester.takeException(), isNull, reason: label);
          }
        }
        await tester.pumpWidget(const SizedBox.shrink());
        client.dispose();
      },
    );
  }
  testWidgets('disconnected tray never presents confirmed sync', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(392, 620);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final client = EngineClient('', '');
    await tester.pumpWidget(TwoDrive(client: client, compact: true));
    await tester.pumpAndSettle();
    expect(find.text('后台连接中断'), findsOneWidget);
    expect(
      tester.widget<Button>(find.widgetWithText(Button, '暂停')).onPressed,
      isNull,
    );
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
    client.dispose();
  });
}
