import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/status_tray.dart';
import 'package:twodrive_dav/workspace.dart';
import 'package:twodrive_dav/tray_panel.dart';

void main() {
  testWidgets('tray to management never exposes native reconfiguration', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1120, 760);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    const window = MethodChannel('window_manager');
    const screens = MethodChannel('dev.leanflutter.plugins/screen_retriever');
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    final client = StorageClient();
    bool visible = true;
    double opacity = 1;
    bool record = false;
    final exposed = <String>[];
    final events = <String>[];
    bool configurationFails = false;
    final display = {
      'id': 'fixture',
      'size': {'width': 1920.0, 'height': 1080.0},
      'visiblePosition': {'dx': 0.0, 'dy': 0.0},
      'visibleSize': {'width': 1920.0, 'height': 1080.0},
    };
    messenger.setMockMethodCallHandler(screens, (call) async {
      return switch (call.method) {
        'getPrimaryDisplay' => display,
        'getAllDisplays' => {
          'displays': [display],
        },
        'getCursorScreenPoint' => {'dx': 1000.0, 'dy': 900.0},
        _ => null,
      };
    });
    messenger.setMockMethodCallHandler(StatusTray.channel, (call) async {
      if (call.method == 'panel') {
        final enabled = (call.arguments as Map)['enabled'] == true;
        if (record && visible && opacity > 0) exposed.add('panel');
        if (enabled) {
          visible = true;
        } else {
          opacity = 1; // Existing native handler resets panel opacity.
        }
      }
      return null;
    });
    messenger.setMockMethodCallHandler(window, (call) async {
      if (record) events.add(call.method);
      if (record &&
          [
            'setAlwaysOnTop',
            'setSkipTaskbar',
            'setTitleBarStyle',
            'setMinimumSize',
            'setBounds',
          ].contains(call.method) &&
          visible &&
          opacity > 0) {
        exposed.add(call.method);
      }
      switch (call.method) {
        case 'hide':
          visible = false;
        case 'show':
          visible = true;
        case 'setOpacity':
          opacity = ((call.arguments as Map)['opacity'] as num).toDouble();
        case 'setTitleBarStyle':
          if (configurationFails) throw PlatformException(code: 'fixture');
        case 'isVisible':
          return visible;
        case 'isMaximized':
          return false;
        case 'isMinimized':
          return false;
        case 'getBounds':
          return {'x': 0.0, 'y': 0.0, 'width': 1120.0, 'height': 760.0};
      }
      return null;
    });
    addTearDown(() {
      messenger.setMockMethodCallHandler(window, null);
      messenger.setMockMethodCallHandler(screens, null);
      messenger.setMockMethodCallHandler(StatusTray.channel, null);
    });
    await tester.pumpWidget(DavApp(client: client, desktop: true));
    await tester.pumpAndSettle();
    dynamic state = tester.state(find.byType(Workspace));
    final panel = state.showPanel(Offset.zero) as Future<void>;
    await tester.pumpAndSettle();
    await panel;
    record = true;
    final management = state.showManagement(5) as Future<void>;
    await tester.pumpAndSettle();
    await management;
    expect(
      exposed,
      isEmpty,
      reason: 'Decorations and geometry must change behind a hidden surface',
    );
    expect(events.first, 'hide');
    expect(visible, isTrue);
    expect(opacity, 1);
    expect(find.text('自动检查更新'), findsOneWidget);
    // A failed native call must not leave the transition guard stuck forever.
    configurationFails = true;
    await expectLater(
      state.showManagement(2) as Future<void>,
      throwsA(isA<PlatformException>()),
    );
    expect(state.presenting, isFalse);
    configurationFails = false;
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });
  testWidgets('tray settings dismisses the popup and selects settings', (
    tester,
  ) async {
    final client = StorageClient();
    dynamic destination;
    // Use the actual TrayPanel menu and callback route.
    await tester.pumpWidget(
      MaterialApp(
        home: TrayPanel(
          client: client,
          onManage: ([int page = 0]) async {
            destination = page;
          },
          onOpenFolder: () async {},
          onQuit: () async {},
        ),
      ),
    );
    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('设置'));
    await tester.pump();
    expect(destination, 5);
    await tester.pump();
    expect(find.text('管理备份文件夹'), findsNothing);
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });
}
