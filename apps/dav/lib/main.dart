import 'dart:async';
import 'dart:io';
import 'language.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';
import 'client.dart';
import 'workspace.dart';
import 'server_window.dart';
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

  final server = args.contains('--tdlink-server');
  await windowManager.setTitle(
    option('--window-title', server ? 'TDLink' : 'TwoDrive'),
  );

  final root = File(Platform.resolvedExecutable).parent.path;
  final client = StorageClient()
    ..setView(visible: !args.contains('--background'));
  final fixtureRoot = option('--native-local-fixture-root', '');
  runApp(DavApp(client: client, desktop: true, server: server));
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
    server: server,
    nativeFixtureRoot: fixtureRoot.isEmpty ? null : fixtureRoot,
  );
  if (server) return;
  if (fixtureRoot.isEmpty) {
    final restore = option('--restore-connections', '');
    unawaited(
      restore.isEmpty
          ? client.restoreOnStartup()
          : client.restoreConnectionsAtStartup(restore.split(',')),
    );
  }
  if (args.contains('--background') && trayReady) {
    client.setView(visible: false);
    await windowManager.hide();
  }
}

class DavApp extends StatelessWidget {
  const DavApp({
    super.key,
    required this.client,
    this.desktop = false,
    this.server = false,
  });
  final bool server;
  final StorageClient client;
  final bool desktop;
  @override
  Widget build(BuildContext context) => ValueListenableBuilder<String>(
    valueListenable: desktopLocale,
    builder: (context, locale, _) => MaterialApp(
      locale: Locale(locale),
      supportedLocales: const [Locale('zh'), Locale('en')],
      localizationsDelegates: GlobalMaterialLocalizations.delegates,
      title: server ? 'TDLink' : 'TwoDrive',
      debugShowCheckedModeBanner: false,
      theme: appTheme(),
      home: server
          ? ServerWindow(client: client, desktop: desktop)
          : Workspace(client: client, desktop: desktop),
    ),
  );
}
