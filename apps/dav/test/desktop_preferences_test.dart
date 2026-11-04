import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/desktop_preferences.dart';
import 'auto_mount_connection_test.dart' show MountClient;

void main() {
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
