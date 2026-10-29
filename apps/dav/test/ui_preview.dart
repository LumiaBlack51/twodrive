import 'dart:io';
import 'dart:ui' as ui;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

/// Optional local visual QA with licensed system fonts, never bundled in a build.
Future<void> loadPreviewFonts() async {
  final fonts = [
    Platform.environment['TWODRIVE_UI_FONT'],
    Platform.environment['TWODRIVE_UI_FONT_BOLD'],
  ].whereType<String>();
  if (fonts.isNotEmpty) {
    final loader = FontLoader('Microsoft YaHei UI');
    for (final path in fonts) {
      loader.addFont(File(path).readAsBytes().then(ByteData.sublistView));
    }
    await loader.load();
  }
  final icons = Platform.environment['TWODRIVE_UI_ICONS'];
  if (icons != null) {
    await (FontLoader(
      'MaterialIcons',
    )..addFont(File(icons).readAsBytes().then(ByteData.sublistView))).load();
  }
}

Future<void> appearanceSnapshot(WidgetTester tester, String name) async {
  final directory = Platform.environment['TWODRIVE_UI_SNAPSHOTS'];
  if (directory == null) return;
  final boundary = tester.renderObject<RenderRepaintBoundary>(
    find.byKey(const ValueKey('appearance-preview')),
  );
  await tester.runAsync(() async {
    final image = await boundary.toImage(pixelRatio: 1);
    final data = await image.toByteData(format: ui.ImageByteFormat.png);
    await File('$directory/$name.png').writeAsBytes(data!.buffer.asUint8List());
    image.dispose();
  });
}
