import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'client.dart';
import 'file_manager.dart';

class CfapiPage extends StatefulWidget {
  const CfapiPage({super.key, required this.client});
  final StorageClient client;
  @override
  State<CfapiPage> createState() => _CfapiPageState();
}

class _CfapiPageState extends State<CfapiPage> {
  String? profileId;
  final root = TextEditingController();
  bool working = false;
  String? error;
  @override
  void dispose() {
    root.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.client;
    final state = c.status['cfapi'] as Map? ?? {};
    final profiles = c.profiles.where((p) => p['kind'] == 'onedrive').toList();
    final selected =
        state['profile'] as String? ??
        profileId ??
        profiles.firstOrNull?['id'] as String?;
    final configured = state['configured'] == true;
    final running = state['running'] == true;
    final progress = state['progress'] as Map? ?? {};
    final summary = progress['summary'] as Map? ?? {};
    final syncIssue =
        progress['local']?['error'] ??
        progress['remote']?['error'] ??
        progress['residency']?['error'];
    return ListView(
      children: [
        Container(
          padding: const EdgeInsets.all(24),
          decoration: BoxDecoration(
            color: const Color(0xffeaf2ff),
            borderRadius: BorderRadius.circular(16),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Icon(
                Icons.cloud_sync_outlined,
                size: 38,
                color: Color(0xff326bd6),
              ),
              const SizedBox(height: 12),
              const Text(
                'Windows 原生按需同步',
                style: TextStyle(fontSize: 24, fontWeight: FontWeight.w700),
              ),
              const SizedBox(height: 8),
              const Text(
                'CFAPI 将 OneDrive 文件显示为 Explorer 原生占位文件。打开文件时下载，并支持“始终保留在此设备上”和“释放空间”。',
              ),
              const SizedBox(height: 12),
              Text(
                state['supported'] == true
                    ? (running
                          ? '● 原生同步运行中'
                          : configured
                          ? '● 原生目录已就绪'
                          : '● CFAPI 组件已安装')
                    : '当前引擎未包含 CFAPI 运行组件',
                style: const TextStyle(fontWeight: FontWeight.w600),
              ),
            ],
          ),
        ),
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                DropdownButtonFormField<String>(
                  initialValue: selected,
                  decoration: const InputDecoration(labelText: 'OneDrive 账户'),
                  items: profiles
                      .map(
                        (p) => DropdownMenuItem(
                          value: p['id'] as String,
                          child: Text(p['name'] as String? ?? 'OneDrive'),
                        ),
                      )
                      .toList(),
                  onChanged: working || configured
                      ? null
                      : (v) => setState(() => profileId = v),
                ),
                const SizedBox(height: 16),
                if (!configured) ...[
                  TextField(
                    controller: root,
                    decoration: const InputDecoration(
                      labelText: '新的原生同步目录',
                      hintText: r'D:\TwoDrive\OneDrive-Native',
                    ),
                  ),
                  TextButton.icon(
                    onPressed: working
                        ? null
                        : () async {
                            final parent = await getDirectoryPath(
                              confirmButtonText: '选择父目录',
                            );
                            if (parent != null && mounted) {
                              setState(
                                () => root.text = '$parent/TwoDrive-Native',
                              );
                            }
                          },
                    icon: const Icon(Icons.folder_open),
                    label: const Text('选择父目录'),
                  ),
                  const Text(
                    '使用新的本机目录。准备只获取云端目录信息，文件内容按需下载。现有挂载和 Microsoft OneDrive 目录保留。',
                  ),
                  const SizedBox(height: 12),
                  FilledButton(
                    onPressed:
                        working ||
                            selected == null ||
                            state['supported'] != true
                        ? null
                        : () => run('native_prepare', selected),
                    child: const Text('准备 CFAPI 原生目录'),
                  ),
                ] else ...[
                  SelectableText(state['root'] as String? ?? ''),
                  const SizedBox(height: 12),
                  Wrap(
                    spacing: 12,
                    runSpacing: 8,
                    children: [
                      FilledButton.icon(
                        onPressed: working || selected == null
                            ? null
                            : () => run(
                                running ? 'native_stop' : 'native_start',
                                selected,
                              ),
                        icon: Icon(
                          running
                              ? Icons.stop_circle_outlined
                              : Icons.play_arrow,
                        ),
                        label: Text(running ? '安全停止原生同步' : '启动原生同步'),
                      ),
                      OutlinedButton.icon(
                        onPressed: () =>
                            openLocalDirectory(state['root'] as String),
                        icon: const Icon(Icons.folder_open),
                        label: const Text('在 Explorer 中打开'),
                      ),
                    ],
                  ),
                  const SizedBox(height: 16),
                  if (syncIssue is String)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Text(
                        '本轮同步待重试：$syncIssue',
                        style: const TextStyle(color: Colors.deepOrange),
                      ),
                    ),
                  Text(
                    progress['phase'] == 'paused'
                        ? '已暂停自动同步'
                        : running
                        ? '正在监测本地与云端变化'
                        : '原生目录保留，等待启动',
                  ),
                  const SizedBox(height: 8),
                  Text(
                    '文件 ${summary['catalogue_files'] ?? '—'} · 目录 ${summary['catalogue_directories'] ?? '—'} · 待处理 ${summary['durable_work_items'] ?? '—'}',
                  ),
                  const SizedBox(height: 8),
                  const Text(
                    '云端变化每 30 秒检查；固定与释放空间每 10 秒检查。文件打开时的写入会等待可一致捕获的时机。冲突版本保留在恢复状态中。',
                  ),
                ],
                if (working)
                  const Padding(
                    padding: EdgeInsets.only(top: 16),
                    child: LinearProgressIndicator(),
                  ),
                if (working)
                  const Padding(
                    padding: EdgeInsets.only(top: 8),
                    child: Text('正在校验账户和原生目录，大型账户可能需要几分钟。'),
                  ),
                if (error != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 12),
                    child: Text(
                      error!,
                      style: const TextStyle(color: Colors.deepOrange),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }

  Future<void> run(String op, String id) async {
    final c = widget.client;
    final profile = c.profiles.where((p) => p['id'] == id).firstOrNull;
    if (profile == null) return;
    setState(() {
      working = true;
      error = null;
    });
    try {
      await c.request(op, {
        'profile': id,
        'revision': profile['revision'],
        if (op == 'native_prepare') 'root': root.text.trim(),
      });
      await c.refreshStatus();
    } catch (e) {
      if (mounted) {
        setState(() => error = '$e'.replaceFirst('Bad state: ', ''));
      }
    } finally {
      if (mounted) setState(() => working = false);
    }
  }
}
