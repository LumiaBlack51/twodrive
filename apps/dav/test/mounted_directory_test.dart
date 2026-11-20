import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/mounted_directory.dart';

final class _MountInfoOverride extends IOOverrides {
  _MountInfoOverride(this.mountInfo);

  final File mountInfo;

  @override
  File createFile(String path) =>
      path == '/proc/self/mountinfo' ? mountInfo : super.createFile(path);
}

void main() {
  test(
    'directory preview accepts only supported FUSE type and source pairs',
    () async {
      final root = await Directory.systemTemp.createTemp('td-mount-preview-');
      addTearDown(() => root.delete(recursive: true));
      final folder = await Directory('${root.path}/mount').create();
      await File('${folder.path}/preview.txt').create();
      final mountInfo = File('${root.path}/mountinfo');
      for (final (type, source, allowed) in [
        ('fuse', 'twodrive-dev', true),
        ('fuse', 'twodrive', true),
        ('fuse.rclone', 'twodrive-ro-fixture', true),
        ('fuse', 'unrelated-fuse', false),
        ('fuse.sshfs', 'twodrive', false),
        ('fuse.rclone', 'unrelated-fuse', false),
      ]) {
        await mountInfo.writeAsString(
          '42 34 0:77 / ${folder.path} rw - $type $source rw\n',
        );
        final entries = await IOOverrides.runWithIOOverrides(
          () => readMountedDirectory(
            configuredRoot: folder.path,
            mountedPath: folder.path,
            directory: '/',
          ),
          _MountInfoOverride(mountInfo),
        );
        expect(
          entries?.single['path'],
          allowed ? '/preview.txt' : null,
          reason: '$type $source',
        );
      }
    },
    skip: !Platform.isLinux,
  );
}
