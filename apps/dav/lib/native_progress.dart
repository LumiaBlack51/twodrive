String? nativeWorkLabel(Map progress) {
  if (progress['phase'] == 'stopping' || progress['stop_requested'] == true) {
    return '正在安全停止原生同步';
  }
  if (progress['phase'] == 'starting') return '正在恢复原生目录';
  if (progress['phase'] != 'working') return null;
  return switch (progress['stage']) {
    'remote' => '正在更新云端目录',
    'residency' => '正在处理本机保留与释放请求',
    _ => '正在核对本地目录',
  };
}
