// Real native transition fixture: private displays only; no engine or account.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/workspace.dart';

class WindowFixtureClient extends StorageClient {
  WindowFixtureClient() {
    selected = 'fixture';
    profiles = [
      {
        'id': 'fixture',
        'name': 'Fixture',
        'kind': 'onedrive',
        'provider_label': 'OneDrive',
        'mount_path': '/tmp/fixture-mount',
        'network_route': 'system',
      },
    ];
    status = {
      'connected': true,
      'profile': 'fixture',
      'account_name': 'fixture@example.invalid',
      'capabilities': {'mount': true},
      'cache': {'pending': 0},
      'sharing': {
        'supported': true,
        'settings': {'name': 'Fixture'},
      },
    };
    available = true;
  }
  @override
  Future<void> pollNow({bool forceDirectory = false}) async {}
}

Future<void> main(List<String> args) async {
  final root = Directory(args[args.indexOf('--fixture-root') + 1]);
  final changing = File('${root.path}/changing');
  WidgetsFlutterBinding.ensureInitialized();
  await windowManager.ensureInitialized();
  await windowManager.waitUntilReadyToShow();
  await windowManager.setMinimumSize(const Size(840, 580));
  runApp(DavApp(client: WindowFixtureClient(), desktop: true));
  await Future<void>.delayed(const Duration(seconds: 2));
  void visit(void Function(Element) action) {
    void walk(Element element) {
      action(element);
      element.visitChildren(walk);
    }

    walk(WidgetsBinding.instance.rootElement!);
  }

  dynamic workspace;
  visit((e) {
    if (e is StatefulElement && e.widget is Workspace) workspace = e.state;
  });
  final pages = <int>[];
  for (var i = 0; i < 3; i++) {
    await workspace.showPanel(const Offset(1000, 1000));
    await Future<void>.delayed(const Duration(milliseconds: 300));
    PopupMenuButtonState<String>? menu;
    visit((e) {
      if (e is StatefulElement && e.state is PopupMenuButtonState<String>) {
        menu = e.state as PopupMenuButtonState<String>;
      }
    });
    if (menu == null) throw StateError('Fixture has no tray menu');
    menu!.showButtonMenu();
    await Future<void>.delayed(const Duration(milliseconds: 300));
    changing.writeAsStringSync('management-transition');
    Navigator.of(menu!.context).pop('settings');
    final deadline = DateTime.now().add(const Duration(seconds: 30));
    while (workspace.panel == true || workspace.presenting == true) {
      if (DateTime.now().isAfter(deadline)) {
        throw StateError('Native transition did not complete');
      }
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    changing.deleteSync();
    pages.add(workspace.page as int);
    await Future<void>.delayed(const Duration(milliseconds: 300));
  }
  stdout.writeln(
    'TRANSITION_RESULT ${jsonEncode({'cycles': pages.length, 'pages': pages, 'final_opacity': await windowManager.getOpacity(), 'final_visible': await windowManager.isVisible()})}',
  );
  exit(0);
}
