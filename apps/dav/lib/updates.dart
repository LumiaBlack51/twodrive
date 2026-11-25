import 'language.dart';
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/foundation.dart';

typedef ReleasePage = Future<List<dynamic>> Function(int page);

/// Development releases have their own version sequence; stable/peer tags
/// must never be compared with it. Release URLs are constructed locally.
class DevelopmentRelease {
  DevelopmentRelease(this.version);
  final String version;
  String get url =>
      'https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v$version';

  static List<int>? numbers(String value) {
    if (!RegExp(
      r'^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$',
    ).hasMatch(value)) {
      return null;
    }
    final parts = value.split('.').map(int.tryParse).toList();
    if (parts.any((p) => p == null || p > 65535)) return null;
    return parts.cast<int>();
  }

  static int compare(String a, String b) {
    final left = numbers(a)!;
    final right = numbers(b)!;
    for (var i = 0; i < 3; i++) {
      final result = left[i].compareTo(right[i]);
      if (result != 0) return result;
    }
    return 0;
  }

  static DevelopmentRelease? newest(List<dynamic> releases, String platform) {
    DevelopmentRelease? latest;
    for (final item in releases) {
      if (item is! Map || item['draft'] != false) continue;
      final tag = item['tag_name'];
      if (tag is! String || !tag.startsWith('dev-v')) continue;
      final version = tag.substring(5);
      if (numbers(version) == null) continue;
      final assets = item['assets'];
      final asset = 'twodrive-development-$version-$platform';
      if (assets is! List ||
          !assets.any(
            (a) =>
                a is Map &&
                a['name'] == asset &&
                a['state'] == 'uploaded' &&
                a['size'] is num &&
                a['size'] > 0,
          )) {
        continue;
      }
      if (latest == null || compare(version, latest.version) > 0) {
        latest = DevelopmentRelease(version);
      }
    }
    return latest;
  }
}

class UpdateChecker extends ChangeNotifier {
  UpdateChecker({
    ReleasePage? fetch,
    DateTime Function()? now,
    String? platform,
  }) : _fetch = fetch ?? fetchPage,
       _now = now ?? DateTime.now,
       _platform =
           platform ??
           (Platform.isWindows ? 'windows-x64.zip' : 'linux-x64.tar.xz');
  final ReleasePage _fetch;
  final DateTime Function() _now;
  final String _platform;
  File? _settings;
  Timer? _timer;
  Future<void>? _checking;
  bool _disposed = false;
  bool enabled = true;
  bool managed = false;
  String current = '';
  String? error;
  DateTime? lastAttempt;
  DateTime? lastSuccess;
  DevelopmentRelease? latest;
  bool get checking => _checking != null;
  bool get available =>
      latest != null &&
      DevelopmentRelease.numbers(current) != null &&
      DevelopmentRelease.compare(latest!.version, current) > 0;

  static Future<List<dynamic>> fetchPage(int page) async {
    final client = HttpClient()
      ..connectionTimeout = const Duration(seconds: 10);
    try {
      return await (() async {
        final request = await client.getUrl(
          Uri.https('api.github.com', '/repos/LumiaBlack51/twodrive/releases', {
            'per_page': '100',
            'page': '$page',
          }),
        );
        request.followRedirects = false;
        request.headers.set('Accept', 'application/vnd.github+json');
        request.headers.set('User-Agent', 'TwoDrive-Development-update-check');
        final response = await request.close();
        if (response.statusCode != 200) {
          throw HttpException(tr('更新服务暂不可用，请稍后重试'));
        }
        final bytes = <int>[];
        await for (final chunk in response) {
          if (bytes.length + chunk.length > 2 * 1024 * 1024) {
            throw FormatException(tr('更新响应超过大小限制'));
          }
          bytes.addAll(chunk);
        }
        final value = jsonDecode(utf8.decode(bytes));
        if (value is! List) throw FormatException(tr('更新响应格式无效'));
        return value;
      })().timeout(const Duration(seconds: 15));
    } finally {
      client.close(force: true);
    }
  }

  Future<void> start(
    String version,
    File settings, {
    bool managed = false,
  }) async {
    if (_disposed || current.isNotEmpty) return;
    current = version;
    this.managed = managed;
    _settings = settings;
    try {
      if (await settings.exists()) {
        final value = jsonDecode(await settings.readAsString());
        enabled = value['enabled'] != false;
        lastAttempt = DateTime.tryParse(value['last_attempt'] as String? ?? '');
      }
    } catch (_) {
      // A damaged preference file must not prevent file access or startup.
    }
    if (_disposed) return;
    _timer = Timer.periodic(
      const Duration(hours: 1),
      (_) => unawaited(check()),
    );
    _notify();
    await check();
  }

  Future<void> _save() async {
    final file = _settings;
    if (file == null || _disposed) return;
    try {
      await file.parent.create(recursive: true);
      final temporary = File('${file.path}.tmp');
      await temporary.writeAsString(
        jsonEncode({
          'enabled': enabled,
          'last_attempt': lastAttempt?.toUtc().toIso8601String(),
        }),
        flush: true,
      );
      await temporary.rename(file.path);
    } catch (_) {
      error = tr('无法保存更新检查偏好，请检查本地状态目录的权限');
    }
  }

  Future<void> setEnabled(bool value) async {
    enabled = value;
    await _save();
    _notify();
    if (value) await check();
  }

  Future<void> check({bool manual = false}) {
    if (_disposed || managed || DevelopmentRelease.numbers(current) == null) {
      return Future.value();
    }
    if (_checking != null) return _checking!;
    final elapsed = lastAttempt == null
        ? null
        : _now().difference(lastAttempt!);
    if (!manual &&
        (!enabled ||
            (elapsed != null &&
                !elapsed.isNegative &&
                elapsed < const Duration(hours: 24)))) {
      return Future.value();
    }
    final work = _perform();
    _checking = work;
    _notify();
    return work;
  }

  Future<void> _perform() async {
    lastAttempt = _now();
    error = null;
    try {
      await _save();
      if (_disposed) return;
      final all = <dynamic>[];
      for (var page = 1; page <= 3; page++) {
        final entries = await _fetch(page);
        if (_disposed) return;
        all.addAll(entries);
        if (entries.length < 100) break;
      }
      final release = DevelopmentRelease.newest(all, _platform);
      if (release == null) throw FormatException(tr('没有可用的 Development 发布包'));
      latest = release;
      lastSuccess = _now();
    } catch (_) {
      if (!_disposed) error = tr('暂时无法检查更新；文件连接与同步不受影响，可稍后重试');
    } finally {
      _checking = null;
      _notify();
    }
  }

  Future<void> openRelease() async {
    final url = latest?.url;
    if (url == null) return;
    try {
      final result = Platform.isWindows
          ? await Process.run('rundll32.exe', [
              'url.dll,FileProtocolHandler',
              url,
            ])
          : await Process.run('xdg-open', [url]);
      if (result.exitCode != 0) throw const FileSystemException();
    } catch (_) {
      error = tr('无法打开浏览器，请复制发布地址打开');
      _notify();
    }
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    super.dispose();
  }
}
