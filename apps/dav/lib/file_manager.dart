import 'dart:convert';
import 'dart:io';

Future<void> _launchFileManager(String executable, List<String> args) async {
  await Process.start(executable, args, mode: ProcessStartMode.detached);
}

Future<void> openLocalDirectory(
  String path, {
  String? platform,
  Future<void> Function(String, List<String>)? launch,
}) async {
  final target = platform ?? Platform.operatingSystem;
  final executable = switch (target) {
    'windows' => 'explorer.exe',
    'linux' => 'xdg-open',
    'macos' => 'open',
    _ => throw UnsupportedError('Unsupported file manager platform'),
  };
  await (launch ?? _launchFileManager)(executable, [path]);
}

Future<void> openLocalFile(String path) async {
  if (Platform.isWindows) {
    await _launchFileManager('explorer.exe', [path]);
    return;
  }
  final (executable, args) = switch (Platform.operatingSystem) {
    'linux' => ('gio', ['open', path]),
    'macos' => ('open', [path]),
    _ => throw UnsupportedError('Unsupported desktop file opener'),
  };
  final result = await Process.run(executable, args);
  if (result.exitCode != 0) throw StateError('无法打开本机查看副本');
}

/// Returns false if the desktop can only open the containing folder.
Future<bool> revealLocalFile(
  String path, {
  String? platform,
  Future<void> Function(String, List<String>)? launch,
  Future<void> Function(String)? openDirectory,
}) async {
  final target = platform ?? Platform.operatingSystem;
  switch (target) {
    case 'windows':
      await (launch ?? _launchFileManager)('explorer.exe', ['/select,', path]);
    case 'macos':
      await (launch ?? _launchFileManager)('open', ['-R', path]);
    case 'linux':
      return revealGnomeFile(
        Uri.file(path, windows: false),
        launch: launch,
        openDirectory: openDirectory,
      );
    default:
      throw UnsupportedError('Unsupported file manager platform');
  }
  return true;
}

Future<bool> revealGnomeFile(
  Uri uri, {
  Future<void> Function(String, List<String>)? launch,
  Future<void> Function(String)? openDirectory,
}) async {
  try {
    await (launch ?? _callShowItems)('gdbus', [
      'call',
      '--session',
      '--timeout',
      '5',
      '--dest',
      'org.freedesktop.FileManager1',
      '--object-path',
      '/org/freedesktop/FileManager1',
      '--method',
      'org.freedesktop.FileManager1.ShowItems',
      jsonEncode([uri.toString()]),
      "''",
    ]);
    return true;
  } on ProcessException {
    final parent = uri.resolve('.');
    await (openDirectory ?? openLocalDirectory)(
      uri.scheme == 'file' ? File.fromUri(uri).parent.path : parent.toString(),
    );
    return false;
  }
}

Future<void> _callShowItems(String executable, List<String> args) async {
  final result = await Process.run(executable, args);
  if (result.exitCode != 0) {
    throw ProcessException(executable, args, '', result.exitCode);
  }
}
