import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

/// Use one family for Latin and CJK text, including Material component labels.
/// These are system fonts; no account data or downloaded font is required.
ThemeData appTheme({TargetPlatform? platform}) {
  final target = platform ?? defaultTargetPlatform;
  final family = switch (target) {
    TargetPlatform.windows => 'Microsoft YaHei UI',
    TargetPlatform.macOS || TargetPlatform.iOS => 'PingFang SC',
    TargetPlatform.linux => 'TwoDriveSans',
    _ => 'Noto Sans CJK SC',
  };
  return ThemeData(
    platform: target,
    useMaterial3: true,
    fontFamily: family,
    fontFamilyFallback: target == TargetPlatform.linux
        ? const ['TwoDriveCJK']
        : const [
            'Microsoft YaHei',
            'PingFang SC',
            'Noto Sans CJK SC',
            'Noto Sans',
            'sans-serif',
          ],
    colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xff326bd6)),
    scaffoldBackgroundColor: const Color(0xfff5f7fb),
    inputDecorationTheme: const InputDecorationTheme(
      border: OutlineInputBorder(),
      isDense: true,
    ),
  );
}
