#include "tray_geometry.h"
#include <cassert>

int main() {
  twodrive::TrayAnchor anchor;
  anchor.capture_icon({850, 850, 20, 20}, {0, 0, 1440, 843});
  auto first = anchor.panel();
  assert(first.x + first.width / 2 == 860);
  assert(first.y + first.height == 835);
  assert(first.width == 390 && first.height == 570);
  // Exact icon geometry wins over a stale XWayland cursor in the workarea.
  anchor.capture(1200, 150, {0, 0, 1440, 843});
  anchor.capture_icon({850, 850, 20, 20}, {0, 0, 1440, 843});
  assert(anchor.panel().x == first.x && anchor.panel().y == first.y);
  // Host layout changes recapture the actual icon even inside 28px.
  anchor.capture_icon({860, 850, 20, 20}, {0, 0, 1440, 843});
  assert(anchor.panel().x == first.x + 10);
  anchor.capture_icon({710, 5, 20, 20}, {0, 30, 1440, 870});
  auto top = anchor.panel();
  assert(top.x == 525 && top.y == 38);
  anchor.capture_icon({-700, 1050, 20, 20}, {-1920, 0, 1920, 1040});
  auto second = anchor.panel();
  assert(second.x == -885 && second.y == 462);
  // Only screen edges may constrain horizontal centering.
  anchor.capture_icon({10, 850, 20, 20}, {0, 0, 1440, 843});
  assert(anchor.panel().x == 8);
  anchor.capture_icon({1410, 850, 20, 20}, {0, 0, 1440, 843});
  assert(anchor.panel().x == 1042);
  anchor.capture_icon({120, 485, 20, 20}, {0, 0, 320, 480});
  auto small = anchor.panel();
  assert(small.x == 8 && small.y == 8);
  assert(small.width == 304 && small.height == 464);
  // Point-only hosts still align with the taskbar edge and center horizontally.
  anchor.capture(860, 867, {0, 0, 1440, 843});
  assert(anchor.panel().x == first.x && anchor.panel().y == first.y);
  // An autohide dock is absent from the system workarea. Reserve its actual
  // 64px size plus two 4px margins before positioning the panel.
  auto safe = twodrive::reserve_dock({0, 0, 1920, 1080}, {0, 0, 1920, 1080},
      twodrive::DockEdge::bottom, 72);
  assert(safe.height == 1008);
  auto already = twodrive::reserve_dock(safe, {0, 0, 1920, 1080},
      twodrive::DockEdge::bottom, 72);
  assert(already.height == safe.height);
  anchor.capture(885, 1040, safe);
  assert(anchor.panel().y + anchor.panel().height == 1000);
  auto short_area = twodrive::reserve_dock({0, 0, 800, 480}, {0, 0, 800, 480},
      twodrive::DockEdge::bottom, 80);
  anchor.capture(700, 450, short_area);
  assert(anchor.panel().height == 384);
  assert(anchor.panel().y == 8);
  auto top_area = twodrive::reserve_dock({-1920, -1080, 1920, 1080},
      {-1920, -1080, 1920, 1080}, twodrive::DockEdge::top, 64);
  anchor.capture(-900, -1050, top_area);
  assert(anchor.panel().y == -1008);
  auto side_area = twodrive::reserve_dock({0, 0, 1920, 1080}, {0, 0, 1920, 1080},
      twodrive::DockEdge::left, 80);
  anchor.capture(40, 400, side_area);
  assert(anchor.panel().x == 88 && anchor.panel().y == 115);
  anchor.capture(1, 1, {0, 0, 1, 1});
  assert(anchor.panel().x == 0 && anchor.panel().y == 0);
  assert(anchor.panel().width == 1 && anchor.panel().height == 1);
}
