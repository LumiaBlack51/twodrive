import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_full/engine_client.dart';
import 'package:twodrive_full/main.dart';
import 'widget_test.dart' show fixture;

void main() {
  testWidgets('persistent index download, cached open and refresh use IPC', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1100, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final state = fixture()
      ..['mode'] = 'authenticated_preview'
      ..['auth_status'] = 'signed_in'
      ..['capabilities'] = ['cloud_index', 'cloud_download']
      ..['cloud'] = {
        'status': 'ready',
        'error': null,
        'offset': 0,
        'count': 1,
        'items': [
          {
            'id': 'synthetic',
            'name': 'report.pdf',
            'kind': 'file',
            'size': 1024,
            'state': 'online_only',
          },
        ],
        'tasks': <Map<String, dynamic>>[],
      };
    final commands = <String>[];
    final client = EngineClient(
      '',
      '',
      transport: (r) async {
        final command = r['command'] as Map;
        commands.add(command['type'] as String);
        if (command['type'] == 'download_cloud') {
          state['cloud']['items'][0]['state'] = 'cached';
        }
        return {'version': 1, 'id': r['id'], 'ok': true, 'snapshot': state};
      },
    )..snapshot = state;
    await tester.pumpWidget(TwoDrive(client: client, compact: false));
    await tester.pumpAndSettle();
    await tester.tap(find.text('文件').first);
    await tester.pumpAndSettle();
    expect(find.textContaining('report.pdf'), findsOneWidget);
    expect(find.text('OneDrive · 持久索引与本地缓存'), findsOneWidget);
    expect(find.text('打开'), findsNothing);
    await tester.tap(find.text('下载 / 续传'));
    await tester.pumpAndSettle();
    expect(commands, ['download_cloud']);
    await tester.tap(find.text('显示位置'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('刷新索引'));
    await tester.pumpAndSettle();
    expect(commands, ['download_cloud', 'open_cached', 'refresh_index']);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
    client.dispose();
  });
}
