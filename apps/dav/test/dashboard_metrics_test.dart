import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/dashboard_metrics.dart';
import 'package:twodrive_dav/client.dart';

void main() {
  Json row(int done, {String revision = 'r1', String kind = 'upload'}) => {
    'profile': 'a',
    'revision': revision,
    'kind': kind,
    'path': '/file',
    'bytes_total': 10000,
    'bytes_done': done,
  };
  test(
    'unchanged ticks retain the interval of the last actual observation',
    () {
      final m = DashboardMetrics();
      m.sample([row(0)], Duration.zero);
      for (var i = 1; i < 10; i++) {
        m.sample([row(0)], Duration(seconds: i));
      }
      m.sample([row(1000)], const Duration(seconds: 10));
      expect(m.uploadRate, 100);
      expect(m.uploaded, 1000);
    },
  );
  test('observed deltas are bounded by identity, lifetime and visibility', () {
    final m = DashboardMetrics();
    m.sample([row(1000)], Duration.zero);
    expect(m.uploaded, 0); // First observation is only a baseline.
    m.sample([row(2024), row(2024)], const Duration(seconds: 2));
    expect(m.uploaded, 1024);
    expect(m.uploadRate, 512);
    m.sample([row(4000, revision: 'new')], const Duration(seconds: 3));
    expect(m.uploaded, 1024);
    m.sample([row(10, revision: 'new')], const Duration(seconds: 4));
    expect(m.uploadRate, 0); // Counter reset is not additional traffic.
    m.suspend();
    m.sample([row(9000, revision: 'new')], const Duration(seconds: 30));
    expect(m.uploaded, 1024);
    m.sample([], const Duration(seconds: 31));
    m.sample([row(9999, revision: 'new')], const Duration(seconds: 32));
    expect(m.uploaded, 1024); // Reused path after disappearance is new work.
    m.sample([
      row(10000, revision: 'new'),
      row(100, kind: 'download'),
    ], const Duration(seconds: 33));
    m.sample([row(1124, kind: 'download')], const Duration(seconds: 34));
    expect(m.downloaded, 1024);
    expect(m.downloadRate, 1024);
    expect(m.uploadPeak, 512);
  });
}
