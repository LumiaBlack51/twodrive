import 'package:flutter/material.dart';
import 'client.dart';

String capacityBytes(num? value) {
  if (value == null || !value.isFinite || value < 0) return '未提供';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  var amount = value.toDouble();
  var unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit++;
  }
  return '${unit == 0 ? amount.toInt() : amount.toStringAsFixed(1)} ${units[unit]}';
}

class StorageUsageView extends StatelessWidget {
  const StorageUsageView({
    super.key,
    required this.value,
    required this.connected,
    this.onRefresh,
    this.ring = false,
  });
  final Json? value;
  final bool connected;
  final VoidCallback? onRefresh;
  final bool ring;
  @override
  Widget build(BuildContext context) {
    final usage = value?['usage'] as Map?;
    final total = usage?['total'] as num?;
    final used = usage?['used'] as num?;
    final free = usage?['free'] as num?;
    final ratio = total != null && total > 0 && used != null
        ? (used / total).clamp(0.0, 1.0)
        : null;
    final text = usage != null
        ? '已用 ${capacityBytes(used)} / 总量 ${capacityBytes(total)} · 剩余 ${capacityBytes(free)}'
        : !connected
        ? '未连接 · 连接后获取空间信息'
        : value?['state'] == 'unsupported'
        ? '此网盘暂不提供容量信息'
        : value?['state'] == 'unavailable'
        ? '空间信息暂时无法获取'
        : '正在获取空间信息…';
    final details = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Row(
          children: [
            const Tooltip(
              message: '账号总空间可能与照片、邮件等服务共用；容量由网盘返回。',
              child: Text(
                '账号空间',
                style: TextStyle(fontSize: 12, color: Colors.blueGrey),
              ),
            ),
            const Spacer(),
            if (connected && onRefresh != null)
              SizedBox(
                width: 28,
                height: 28,
                child: IconButton(
                  tooltip: '刷新空间信息',
                  padding: EdgeInsets.zero,
                  iconSize: 17,
                  onPressed: value?['loading'] == true ? null : onRefresh,
                  icon: const Icon(Icons.refresh),
                ),
              ),
          ],
        ),
        if (ring && usage != null) ...[
          Text(
            '已用 ${capacityBytes(used)} / 总量 ${capacityBytes(total)}',
            style: const TextStyle(fontSize: 12),
          ),
          Text(
            '剩余 ${capacityBytes(free)}',
            style: const TextStyle(fontSize: 12, color: Colors.blueGrey),
          ),
        ] else
          Text(text, style: const TextStyle(fontSize: 12)),
        if (!ring && ratio != null)
          Padding(
            padding: const EdgeInsets.only(top: 7),
            child: ClipRRect(
              borderRadius: BorderRadius.circular(4),
              child: LinearProgressIndicator(
                value: ratio.toDouble(),
                minHeight: 5,
                color: ratio >= 0.95 ? Colors.deepOrange : null,
              ),
            ),
          ),
        if (value?['stale'] == true)
          const Padding(
            padding: EdgeInsets.only(top: 4),
            child: Text(
              '更新未完成，显示上次结果',
              style: TextStyle(fontSize: 11, color: Colors.blueGrey),
            ),
          ),
      ],
    );
    if (ring) {
      return Semantics(
        label: '云盘空间：$text',
        child: Row(
          children: [
            if (ratio != null) ...[
              SizedBox(
                width: 54,
                height: 54,
                child: Stack(
                  alignment: Alignment.center,
                  children: [
                    Positioned.fill(
                      child: CircularProgressIndicator(
                        value: ratio.toDouble(),
                        strokeWidth: 5,
                        backgroundColor: Colors.grey.shade200,
                        color: ratio >= 0.95 ? Colors.deepOrange : null,
                      ),
                    ),
                    Padding(
                      padding: const EdgeInsets.all(8),
                      child: FittedBox(
                        fit: BoxFit.scaleDown,
                        child: Text(
                          used! / total! > 9.99
                              ? '>999%'
                              : '${((used / total) * 100).toStringAsFixed(0)}%',
                          style: const TextStyle(
                            fontSize: 12,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 14),
            ] else ...[
              const Icon(
                Icons.pie_chart_outline,
                size: 34,
                color: Colors.blueGrey,
              ),
              const SizedBox(width: 14),
            ],
            Expanded(child: details),
          ],
        ),
      );
    }
    return Semantics(label: '云盘空间：$text', child: details);
  }
}
