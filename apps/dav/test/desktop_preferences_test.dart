import 'dart:convert';
import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/desktop_preferences.dart';
import 'auto_mount_connection_test.dart' show MountClient;

void main() {
  test('legacy preferences retain default startup restoration', () async {
    final root = await Directory.systemTemp.createTemp('twodrive-legacy-pref-');
    try {
      final file = File('${root.path}/desktop-settings.json');
      await file.writeAsString(jsonEncode({'auto_mount_on_connect': false}));
      final preferences = DesktopPreferences();
      await preferences.load(file);
      expect(preferences.autoMountOnConnect, isFalse);
      expect(preferences.restoreConnectionsOnStartup, isTrue);
      await preferences.setRestoreConnectionsOnStartup(false);
      final restored = DesktopPreferences();
      await restored.load(file);
      expect(restored.autoMountOnConnect, isFalse);
      expect(restored.restoreConnectionsOnStartup, isFalse);
    } finally {
      await root.delete(recursive: true);
    }
  });
  test('overlapping preference saves preserve both switches', () async {
    final root = await Directory.systemTemp.createTemp('twodrive-both-pref-');
    try {
      final file = File('${root.path}/desktop-settings.json');
      final preferences = DesktopPreferences();
      await preferences.load(file);
      await Future.wait([
        preferences.setAutoMountOnConnect(false),
        preferences.setRestoreConnectionsOnStartup(false),
      ]);
      final restored = DesktopPreferences();
      await restored.load(file);
      expect(restored.autoMountOnConnect, isFalse);
      expect(restored.restoreConnectionsOnStartup, isFalse);
      await restored.setAutoMountOnConnect(true);
      final again = DesktopPreferences();
      await again.load(file);
      expect(again.autoMountOnConnect, isTrue);
      expect(again.restoreConnectionsOnStartup, isFalse);
    } finally {
      await root.delete(recursive: true);
    }
  });
  test(
    'failed preference save preserves values and later saves can succeed',
    () async {
      final root = await Directory.systemTemp.createTemp(
        'twodrive-failed-pref-',
      );
      try {
        final file = File('${root.path}/desktop-settings.json');
        final preferences = DesktopPreferences();
        await preferences.load(file);
        final blocker = await Directory('${file.path}.tmp').create();
        await expectLater(
          preferences.setRestoreConnectionsOnStartup(false),
          throwsA(isA<FileSystemException>()),
        );
        expect(preferences.restoreConnectionsOnStartup, isTrue);
        await blocker.delete();
        await preferences.setAutoMountOnConnect(false);
        final restored = DesktopPreferences();
        await restored.load(file);
        expect(restored.autoMountOnConnect, isFalse);
        expect(restored.restoreConnectionsOnStartup, isTrue);
      } finally {
        await root.delete(recursive: true);
      }
    },
  );

  test('automatic mount preference survives restarting the client', () async {
    final root = await Directory.systemTemp.createTemp('twodrive-preferences-');
    try {
      final file = File('${root.path}/desktop-settings.json');
      final first = DesktopPreferences();
      await first.load(file);
      expect(first.autoMountOnConnect, isTrue);
      await first.setAutoMountOnConnect(false);
      final second = DesktopPreferences();
      await second.load(file);
      expect(second.autoMountOnConnect, isFalse);
      await second.setAutoMountOnConnect(true);
      final third = DesktopPreferences();
      await third.load(file);
      expect(third.autoMountOnConnect, isTrue);
    } finally {
      await root.delete(recursive: true);
    }
  });
  test('disabled preference keeps successful connection unmounted', () async {
    final client = MountClient();
    await client.setAutoMountOnConnect(false);
    await client.connect();
    expect(client.connected, isTrue);
    expect(client.operations, isNot(contains('mount')));
    client.dispose();
  });
}
