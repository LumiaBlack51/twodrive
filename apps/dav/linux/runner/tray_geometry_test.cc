#include "tray_geometry.h"
#include <cassert>

int main() {
  // Host X11 pixels must be normalized before monitor selection and dock
  // reservation, then GDK scales the resulting panel exactly once on output.
  // Both nonzero and negative monitor origins use the screen-wide scale.
  for (int scale : {1, 2}) {
    for (int origin : {0, 1920, -1920}) {
      const auto monitor = twodrive::host_rectangle_to_gdk(
          {origin, 0, 1920, 1080}, twodrive::host_coordinate_scale(true, scale, 2, 2));
      const auto icon = twodrive::host_rectangle_to_gdk(
          {origin + 940, 1030, 40, 40}, twodrive::host_coordinate_scale(true, scale, 2, 2));
      const auto area = twodrive::host_rectangle_to_gdk(
          {origin, 0, 1920, 1080}, twodrive::host_coordinate_scale(true, scale, 2, 2));
      assert(monitor.x == origin / scale);
      assert(icon.x + icon.width / 2 == (origin + 960) / scale);
      assert(icon.x >= monitor.x && icon.x < monitor.x + monitor.width);
      const auto safe = twodrive::reserve_dock(area, monitor,
          twodrive::DockEdge::bottom, 72 / scale);
      const auto reserved = twodrive::reserve_dock(safe, monitor,
          twodrive::DockEdge::bottom, 72 / scale);
      assert(reserved.height == safe.height); // no double reservation
      twodrive::TrayAnchor scaled;
      scaled.capture_icon(icon, safe);
      const auto panel = scaled.panel();
      assert(panel.x * scale + panel.width * scale / 2 == origin + 960);
      assert((panel.y + panel.height) * scale == 1008 - 8 * scale);
      assert(panel.width == 390);
      assert(panel.height == (scale == 1 ? 570 : 488));
    }
  }
  // Logical-layout XWayland native scaling multiplies Shell coordinates by
  // the compositor's legacy scale before GDK divides by its own screen scale.
  const auto xwayland = twodrive::host_rectangle_to_gdk(
      {-960, 12, 960, 492}, twodrive::host_coordinate_scale(true, 2, 1, 2));
  assert(xwayland.x == -960 && xwayland.y == 12);
  assert(xwayland.width == 960 && xwayland.height == 492);
  const auto overridden = twodrive::host_rectangle_to_gdk(
      {960, 12, 960, 492}, twodrive::host_coordinate_scale(true, 1, 1, 2));
  assert(overridden.x == 1920 && overridden.y == 24);
  assert(overridden.width == 1920 && overridden.height == 984);
  // A missing layout property must not treat a physical X11 UI size as coordinates.
  const auto unknown_layout = twodrive::host_coordinate_scale(true, 2, 0, 2);
  assert(unknown_layout.numerator == 1 && unknown_layout.denominator == 2);
  const auto legacy_xwayland = twodrive::host_coordinate_scale(true, 2, 1, 1);
  assert(legacy_xwayland.numerator == 1 && legacy_xwayland.denominator == 2);
  // Scale 1 is also the native Wayland contract, even on a HiDPI monitor.
  const auto logical = twodrive::host_rectangle_to_gdk({-960, 12, 960, 492},
      twodrive::host_coordinate_scale(false, 2, 1, 2));
  assert(logical.x == -960 && logical.y == 12);
  assert(logical.width == 960 && logical.height == 492);
  // Match GDK truncation for unaligned/negative origins and keep tiny bounds.
  const auto odd = twodrive::host_rectangle_to_gdk({-1921, 3, 1919, 1079}, {1, 2});
  assert(odd.x == -960 && odd.y == 1 && odd.width == 959 && odd.height == 539);
  const auto tiny = twodrive::host_rectangle_to_gdk({0, 0, 1, 1}, {1, 2});
  assert(tiny.width == 1 && tiny.height == 1);

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
