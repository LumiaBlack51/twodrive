import 'dart:convert';
import 'client.dart';

/// Lower-bound progress observed while the dashboard is visible, not NIC IO.
class DashboardMetrics {
  Map<String, ({int done, Duration at})> _previous = {};
  double uploadRate = 0, downloadRate = 0;
  double uploadPeak = 0, downloadPeak = 0;
  int uploaded = 0, downloaded = 0;

  void sample(Iterable<Json> rows, Duration now) {
    final next = <String, int>{};
    for (final row in rows) {
      final kind = row['kind'];
      if (kind != 'upload' && kind != 'download') continue;
      final done = row['bytes_done'];
      if (done is! num || !done.isFinite || done < 0) continue;
      final key = jsonEncode([
        row['profile'],
        row['revision'],
        kind,
        row['id'],
        row['path'] ?? row['name'],
        row['bytes_total'],
      ]);
      // Duplicate observations of the same task must not double-count bytes.
      next[key] = done.toInt() > (next[key] ?? -1) ? done.toInt() : next[key]!;
    }
    uploadRate = downloadRate = 0;
    final observations = <String, ({int done, Duration at})>{};
    for (final entry in next.entries) {
      final old = _previous[entry.key];
      observations[entry.key] = (done: entry.value, at: now);
      if (old == null || entry.value < old.done) continue;
      final delta = entry.value - old.done;
      if (delta == 0 || now <= old.at) {
        // Unchanged status polls do not shorten the observation interval.
        observations[entry.key] = old;
        continue;
      }
      final seconds = (now - old.at).inMicroseconds / 1e6;
      final kind = (jsonDecode(entry.key) as List)[2];
      if (kind == 'upload') {
        uploaded += delta;
        uploadRate += delta / seconds;
      } else {
        downloaded += delta;
        downloadRate += delta / seconds;
      }
    }
    if (uploadRate > uploadPeak) uploadPeak = uploadRate;
    if (downloadRate > downloadPeak) downloadPeak = downloadRate;
    _previous = observations;
  }

  void suspend() {
    _previous = {};
    uploadRate = downloadRate = 0;
  }
}
