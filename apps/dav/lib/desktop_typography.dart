import 'dart:io';

/// Bundled families avoid enumerating the Linux host's entire font catalog
/// when a page first introduces a font style or a generic monospace alias.
/// Text scaling continues to follow the user's desktop accessibility setting.
class DesktopTypography {
  static String? get family => Platform.isLinux ? 'TwoDriveSans' : null;
  static List<String>? get fallback =>
      Platform.isLinux ? const ['TwoDriveCJK'] : null;
  static String get monospace =>
      Platform.isLinux ? 'TwoDriveMono' : 'monospace';
}
