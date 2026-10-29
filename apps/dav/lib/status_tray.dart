import 'dart:io';
import 'package:flutter/services.dart';

class StatusTray {
  static const channel = MethodChannel('twodrive/status_tray');
  void Function(Offset)? onActivate;
  void Function()? onQuit;
  StatusTray() {
    channel.setMethodCallHandler((call) async {
      if (call.method == 'quit') {
        onQuit?.call();
      }
      if (call.method == 'activate') {
        final args = Map<String, dynamic>.from(call.arguments as Map);
        onActivate?.call(
          Offset((args['x'] as num).toDouble(), (args['y'] as num).toDouble()),
        );
      }
    });
  }
  Future<void> icon(
    String state, {
    String title = 'TwoDrive',
  }) => channel.invokeMethod('configure', {
    'title': title,
    'icon':
        '${File(Platform.resolvedExecutable).parent.path}/data/flutter_assets/assets/cloud-$state.${Platform.isWindows ? "ico" : "svg"}',
  });
  Future<void> panel(bool enabled) =>
      channel.invokeMethod('panel', {'enabled': enabled});
  Future<void> destroy() => channel.invokeMethod('destroy');
}
