// Native release benchmark on a private display. No storage engine or account.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:window_manager/window_manager.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';

class SettingsFixtureClient extends StorageClient {
  SettingsFixtureClient() {
    available = true;
    selected = 'fixture';
    profiles = [
      {
        'id': 'fixture',
        'revision': 'fixture-revision',
        'name': 'iCloud Drive',
        'kind': 'icloud',
        'provider_label': 'iCloud Drive',
        'network_route': 'system',
        'mount_readonly': true,
        'mount_path': '/tmp/fixture-icloud',
      },
    ];
    updates.current = '0.1.7';
    status = {
      'connected': true,
      'profile': 'fixture',
      'autostart': false,
      'capabilities': {'mount': true},
      'mount_support': {
        'supported': true,
        'platform': 'linux',
        'readonly': true,
        'driver_installed': true,
      },
      'mounts': [
        {
          'profile': 'fixture',
          'revision': 'fixture-revision',
          'path': '/tmp/fixture-icloud',
          'readonly': true,
          'failed': false,
        },
      ],
      'mounted': true,
      'local_files': {'schema_version': 1, 'metadata_preflight': true},
      'network': ['network: iCloud route=system'],
    };
  }
  @override
  Future<void> pollNow({bool forceDirectory = false}) async {}
}

class SettingsFixtureApp extends StatelessWidget {
  const SettingsFixtureApp({
    super.key,
    required this.client,
    required this.scale,
  });
  final StorageClient client;
  final double scale;
  @override
  Widget build(BuildContext context) {
    final actual =
        DavApp(client: client, desktop: true).build(context) as MaterialApp;
    return MaterialApp(
      title: actual.title,
      debugShowCheckedModeBanner: false,
      theme: actual.theme,
      home: actual.home,
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(
          context,
        ).copyWith(textScaler: TextScaler.linear(scale)),
        child: child!,
      ),
    );
  }
}

Future<void> main(List<String> args) async {
  final scale = args.contains('--scale')
      ? double.parse(args[args.indexOf('--scale') + 1])
      : 1.5625;
  WidgetsFlutterBinding.ensureInitialized();
  await windowManager.ensureInitialized();
  await windowManager.waitUntilReadyToShow();
  await windowManager.setMinimumSize(const Size(1120, 760));
  runApp(SettingsFixtureApp(client: SettingsFixtureClient(), scale: scale));
  await Future<void>.delayed(const Duration(seconds: 2));
  void visit(void Function(Element) action) {
    void walk(Element element) {
      action(element);
      element.visitChildren(walk);
    }

    walk(WidgetsBinding.instance.rootElement!);
  }

  void selectPage(String title) {
    VoidCallback? handler;
    visit((element) {
      final widget = element.widget;
      if (widget is ListTile &&
          widget.title is Text &&
          (widget.title as Text).data == title) {
        handler = widget.onTap;
      }
    });
    if (handler == null) throw StateError('Navigation handler missing');
    handler!();
  }

  final frames = <FrameTiming>[];
  SchedulerBinding.instance.addTimingsCallback(frames.addAll);
  for (var i = 0; i < 3; i++) {
    frames.clear();
    stdout.writeln('NAV_BEGIN $i');
    final clock = Stopwatch()..start();
    selectPage('设置');
    final handlerUs = clock.elapsedMicroseconds;
    await WidgetsBinding.instance.endOfFrame;
    final frameUs = clock.elapsedMicroseconds;
    await Future<void>.delayed(const Duration(milliseconds: 1200));
    stdout.writeln(
      'SETTINGS_FIRST_OPEN ${jsonEncode({'iteration': i, 'handler_us': handlerUs, 'framework_frame_us': frameUs, 'scale': scale, 'build_us': frames.map((f) => f.buildDuration.inMicroseconds).toList(), 'raster_us': frames.map((f) => f.rasterDuration.inMicroseconds).toList()})}',
    );
    selectPage('文件');
    await WidgetsBinding.instance.endOfFrame;
    await Future<void>.delayed(const Duration(milliseconds: 200));
  }
  stdout.writeln('SETTINGS_FIRST_OPEN_DONE');
  exit(0);
}
