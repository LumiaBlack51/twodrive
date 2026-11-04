#ifndef TWODRIVE_TRAY_GEOMETRY_H_
#define TWODRIVE_TRAY_GEOMETRY_H_

#include <algorithm>
#include <cstdlib>

namespace twodrive {
struct Rectangle {
  int x, y, width, height;
};
enum class DockEdge { top, bottom, left, right };
// Intersect the reported workarea with a monitor's occupied dock band.
// Taking min/max against the monitor edge avoids subtracting a reserved dock
// twice, and also handles intellihide docks that reserve no workarea at all.
inline Rectangle reserve_dock(Rectangle area, Rectangle monitor,
                              DockEdge edge, int thickness) {
  if (thickness <= 0) return area;
  const int right = area.x + area.width, bottom = area.y + area.height;
  if (edge == DockEdge::top) area.y = std::min(bottom - 1, std::max(area.y, monitor.y + thickness));
  if (edge == DockEdge::left) area.x = std::min(right - 1, std::max(area.x, monitor.x + thickness));
  area.width = std::max(1, (edge == DockEdge::right
      ? std::min(right, monitor.x + monitor.width - thickness) : right) - area.x);
  area.height = std::max(1, (edge == DockEdge::bottom
      ? std::min(bottom, monitor.y + monitor.height - thickness) : bottom) - area.y);
  return area;
}
struct TrayAnchor {
  int x = 0, y = 0;
  Rectangle area = {};
  bool valid = false;
  Rectangle icon = {};

  void capture_icon(Rectangle next_icon, Rectangle next_area) {
    icon = next_icon;
    x = icon.x + icon.width / 2;
    y = icon.y + icon.height / 2;
    area = next_area;
    valid = true;
  }

  void capture(int next_x, int next_y, Rectangle next_area) {
    // A host supplies a click point, not the icon's bounds. Preserve the first
    // point for subsequent clicks inside that icon so the panel does not jitter.
    if (valid && area.x == next_area.x && area.y == next_area.y &&
        area.width == next_area.width && area.height == next_area.height &&
        std::abs(x - next_x) <= 28 && std::abs(y - next_y) <= 28) return;
    x = next_x;
    y = next_y;
    area = next_area;
    icon = {x, y, 0, 0};
    valid = true;
  }

  Rectangle panel() const {
    const int mx = std::min(8, std::max(0, (area.width - 1) / 2));
    const int my = std::min(8, std::max(0, (area.height - 1) / 2));
    const int width = std::min(390, std::max(1, area.width - 2 * mx));
    const int height = std::min(570, std::max(1, area.height - 2 * my));
    const int left = std::max(area.x + mx,
        std::min(x - width / 2, area.x + area.width - width - mx));
    // Anchor to the reserved taskbar edge, never to the cursor's vertical
    // position. A bottom icon opens above it; a top icon opens below it.
    const bool side = x < area.x || x >= area.x + area.width;
    const int top = side
        ? std::max(area.y + my, std::min(y - height / 2,
            area.y + area.height - height - my))
        : y < area.y + area.height / 2 ? area.y + my
        : area.y + area.height - height - my;
    return {left, top, width, height};
  }
};
}  // namespace twodrive
#endif
