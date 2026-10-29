import 'dart:async';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';
import 'client.dart';
import 'workspace.dart';
import 'platform_paths.dart';
import 'app_theme.dart';

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  await windowManager.ensureInitialized();
  // Windows creates its taskbar COM interface in this step. Tray transitions
  // must not call setSkipTaskbar before that interface exists.
  await windowManager.waitUntilReadyToShow();
  await windowManager.setMinimumSize(const Size(840, 580));
  final home = userHome;
  final data = userData;
  String option(String name, String fallback) {
    final index = args.indexOf(name);
    return index >= 0 && index + 1 < args.length ? args[index + 1] : fallback;
  }

  await windowManager.setTitle(
    option('--window-title', 'TwoDrive Development'),
  );

  final root = File(Platform.resolvedExecutable).parent.path;
  final client = StorageClient();
  final fixtureRoot = option('--native-local-fixture-root', '');
  runApp(DavApp(client: client, desktop: true));
  await client.start(
    option('--engine', '$root/${executableName('twodrive-dav-engine')}'),
    option('--state', '$data/twodrive-dav/state'),
    option('--peer-bin', '$root/${executableName('twodrive-dev')}'),
    option(
      '--launcher',
      Platform.isWindows
          ? Platform.resolvedExecutable
          : '$home/.local/bin/twodrive-dav',
    ),
    nativeFixtureRoot: fixtureRoot.isEmpty ? null : fixtureRoot,
  );
  if (fixtureRoot.isEmpty) {
    unawaited(client.restoreOnStartup());
  }
  if (args.contains('--background') && trayReady) {
    await windowManager.hide();
  }
}

class DavApp extends StatelessWidget {
  const DavApp({super.key, required this.client, this.desktop = false});
  final StorageClient client;
  final bool desktop;
  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'TwoDrive Development',
    debugShowCheckedModeBanner: false,
    theme: appTheme(),
    home: Workspace(client: client, desktop: desktop),
  );
}
