import 'dart:async';
import 'dart:io';

const _mountInfo = '/proc/self/mountinfo';
const _previewLimit = 2000;

// A mounted listing is display metadata. The provider still supplies the
// authoritative list, versions and permissions used by file operations.
Future<List<Map<String, dynamic>>?> readMountedDirectory({
  required String configuredRoot,
  required String mountedPath,
  required String directory,
}) async {
  if (!Platform.isLinux ||
      !configuredRoot.startsWith('/') ||
      !directory.startsWith('/')) {
    return null;
  }
  final parts = directory == '/'
      ? <String>[]
      : directory.substring(1).split('/');
  if (parts.any(
    (p) => p.isEmpty || p == '.' || p == '..' || p.contains('\u0000'),
  )) {
    return null;
  }
  try {
    final mount = await _mountedRoot(configuredRoot, mountedPath);
    if (mount == null) return null;
    final path = [mount.root, ...parts].join('/');
    final folder = Directory(path);
    if (parts.isNotEmpty &&
        await folder.resolveSymbolicLinks().timeout(
              const Duration(seconds: 2),
            ) !=
            path) {
      return null;
    }
    final entries = <Map<String, dynamic>>[];
    var scanned = 0;
    await for (final entity
        in folder
            .list(followLinks: false)
            .timeout(const Duration(seconds: 2))) {
      if (++scanned > _previewLimit) return null;
      if (entity is Link) continue;
      final name = entity.path.split('/').last;
      if (name == '.Trash' ||
          RegExp(r'^\.Trash-[0-9]+$').hasMatch(name) ||
          name.contains('\\')) {
        continue;
      }
      entries.add({
        'name': name,
        'path': directory == '/' ? '/$name' : '$directory/$name',
        'directory': entity is Directory,
        'etag': '',
      });
    }
    if (entries.isEmpty) return null;
    if (await _mountedRoot(configuredRoot, mountedPath) != mount) return null;
    entries.sort(
      (a, b) => (a['name'] as String).compareTo(b['name'] as String),
    );
    return entries;
  } on FileSystemException {
    return null;
  } on TimeoutException {
    return null;
  }
}

Future<({String id, String root})?> _mountedRoot(
  String configuredRoot,
  String mountedPath,
) async {
  final rows = await File(_mountInfo).readAsLines();
  if (mountedPath == configuredRoot) {
    final mount = _mountAt(rows, mountedPath);
    if (mount == null) return null;
    final owned = switch (mount.type) {
      'fuse' => mount.source == 'twodrive' || mount.source == 'twodrive-dev',
      'fuse.rclone' => mount.source.startsWith('twodrive-ro-'),
      _ => false,
    };
    if (!owned) return null;
    return (id: mount.id, root: mountedPath);
  }

  final uri = Uri.tryParse(mountedPath);
  final runtime = Platform.environment['XDG_RUNTIME_DIR'];
  if (runtime == null ||
      uri == null ||
      uri.scheme != 'dav' ||
      uri.host != '127.0.0.1' ||
      !uri.hasPort ||
      uri.path != '/' ||
      uri.hasQuery ||
      uri.hasFragment ||
      !RegExp(r'^iCloud-[a-zA-Z0-9_-]{8}$').hasMatch(uri.userInfo)) {
    return null;
  }
  final gvfs = '$runtime/gvfs';
  final mount = _mountAt(rows, gvfs);
  if (mount == null || mount.type != 'fuse.gvfsd-fuse') return null;
  final export = await Link(configuredRoot).target();
  if (!export.startsWith('$gvfs/dav:')) return null;
  final fields = export.substring('$gvfs/dav:'.length).split(',');
  if (!fields.contains('host=${uri.host}') ||
      !fields.contains('port=${uri.port}') ||
      !fields.contains('user=${uri.userInfo}') ||
      fields.any((field) => field.contains('/'))) {
    return null;
  }
  return (id: mount.id, root: export);
}

({String id, String type, String source})? _mountAt(
  List<String> rows,
  String path,
) {
  ({String id, String type, String source})? mount;
  for (final line in rows) {
    final halves = line.split(' - ');
    if (halves.length != 2) continue;
    final before = halves[0].split(' ');
    final after = halves[1].split(' ');
    if (before.length < 5 || after.length < 2) continue;
    final target = before[4].replaceAllMapped(
      RegExp(r'\\([0-7]{3})'),
      (m) => String.fromCharCode(int.parse(m[1]!, radix: 8)),
    );
    if (target != path) continue;
    if (mount != null) return null;
    mount = (id: before[0], type: after[0], source: after[1]);
  }
  return mount;
}
