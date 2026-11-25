import 'language.dart';

String? nativeWorkLabel(Map progress) {
  if (progress['phase'] == 'stopping' || progress['stop_requested'] == true) {
    return tr('正在安全停止原生同步');
  }
  if (progress['phase'] == 'starting') return tr('正在恢复原生目录');
  if (progress['phase'] != 'working') return null;
  return switch (progress['stage']) {
    'remote' => tr('正在更新云端目录'),
    'residency' => tr('正在处理本机保留与释放请求'),
    _ => tr('正在核对本地目录'),
  };
}
