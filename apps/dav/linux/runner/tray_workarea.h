#ifndef TWODRIVE_TRAY_WORKAREA_H_
#define TWODRIVE_TRAY_WORKAREA_H_

#include <flutter_linux/flutter_linux.h>
#include <gtk/gtk.h>
#include <cmath>
#include <cstring>
#include <string>
#include <vector>
#include "tray_geometry.h"
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

namespace twodrive {
inline GVariant* current_display_state(GDBusConnection* bus) {
  if (!bus) return nullptr;
  return g_dbus_connection_call_sync(bus,
      "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig",
      "org.gnome.Mutter.DisplayConfig", "GetCurrentState", nullptr, nullptr,
      G_DBUS_CALL_FLAGS_NO_AUTO_START, 150, nullptr, nullptr);
}

// Mutter <=46 exposes a signed integer in DisplayConfig. Mutter >=47 moved
// it to the X11 interface; do not confuse its type with unsigned layout-mode.
inline int legacy_ui_scale(GDBusConnection* bus, GVariant* state) {
  gint scale = 1;
  if (state && g_variant_n_children(state) == 4) {
    g_autoptr(GVariant) props = g_variant_get_child_value(state, 3);
    if (g_variant_lookup(props, "legacy-ui-scaling-factor", "i", &scale))
      return scale;
  }
  if (bus) {
    g_autoptr(GVariant) result = g_dbus_connection_call_sync(bus,
        "org.gnome.Mutter.X11", "/org/gnome/Mutter/X11",
        "org.freedesktop.DBus.Properties", "Get",
        g_variant_new("(ss)", "org.gnome.Mutter.X11", "UiScalingFactor"),
        G_VARIANT_TYPE("(v)"), G_DBUS_CALL_FLAGS_NO_AUTO_START, 150, nullptr, nullptr);
    if (result) {
      g_autoptr(GVariant) value = nullptr;
      g_variant_get(result, "(v)", &value);
      if (g_variant_is_of_type(value, G_VARIANT_TYPE_INT32))
        scale = g_variant_get_int32(value);
    }
  }
  return scale;
}

inline HostCoordinateScale host_coordinate_scale(GdkDisplay* display,
                                                 GVariant* state, int legacy_scale) {
#ifdef GDK_WINDOWING_X11
  int gdk_scale = 1;
  // Only a confirmed logical layout may turn the UI factor into a coordinate factor.
  guint32 layout = 0;
  if (GDK_IS_X11_DISPLAY(display)) {
    // All GTK3 X11 monitors share the screen scale. Lookup itself needs GDK
    // units, so do not select a monitor with an unconverted Shell point.
    auto* monitor = gdk_display_get_monitor(display, 0);
    if (monitor) gdk_scale = std::max(1, gdk_monitor_get_scale_factor(monitor));
    if (state && g_variant_n_children(state) == 4) {
      g_autoptr(GVariant) props = g_variant_get_child_value(state, 3);
      g_variant_lookup(props, "layout-mode", "u", &layout);
      // Mutter logical layout: legacy scale is XWayland's coordinate scale
      // (1 without native scaling). In physical layout it is only UI sizing.
    }
    return host_coordinate_scale(true, gdk_scale, layout, legacy_scale);
  }
#else
  (void)display;
  (void)state;
  (void)legacy_scale;
#endif
  return {};
}

inline FlValue* dock_setting(FlValue* values,
                            const std::vector<std::string>& keys) {
  if (!values || fl_value_get_type(values) != FL_VALUE_TYPE_MAP) return nullptr;
  for (const auto& key : keys)
    if (auto* value = fl_value_lookup_string(values, key.c_str())) return value;
  return nullptr;
}

// GTK/XWayland workarea includes an intellihide dock. Read its current size
// without changing the desktop, starting services, or using private user data.
inline Rectangle configured_dock_workarea(GDBusConnection* bus,
                                           GdkMonitor* monitor, Rectangle area,
                                           GVariant* display_state = nullptr,
                                           int ui_scale = -1) {
  auto* source = g_settings_schema_source_get_default();
  if (!source) return area;
  g_autoptr(GSettingsSchema) schema = g_settings_schema_source_lookup(source,
      "org.gnome.shell.extensions.zorin-taskbar", TRUE);
  if (!schema) return area;
  g_autofree gchar* desktop = g_ascii_strdown(g_getenv("XDG_CURRENT_DESKTOP")
      ? g_getenv("XDG_CURRENT_DESKTOP") : "", -1);
  bool enabled = g_strrstr(desktop, "zorin") != nullptr;
  g_autoptr(GSettingsSchema) shell_schema = g_settings_schema_source_lookup(
      source, "org.gnome.shell", TRUE);
  if (shell_schema) {
    g_autoptr(GSettings) shell = g_settings_new_full(shell_schema, nullptr, nullptr);
    g_auto(GStrv) extensions = g_settings_get_strv(shell, "enabled-extensions");
    enabled |= g_strv_contains(const_cast<const gchar* const*>(extensions),
        "zorin-taskbar@zorinos.com");
  }
  if (!enabled) return area;
  auto* display = gdk_monitor_get_display(monitor);
  int index = 0;
  while (index < gdk_display_get_n_monitors(display) &&
         gdk_display_get_monitor(display, index) != monitor) ++index;
  GdkRectangle bounds;
  gdk_monitor_get_geometry(monitor, &bounds);
  std::vector<std::string> keys;
  int shell_scale = ui_scale >= 0 ? ui_scale : 1;
  if (bus) {
    g_autoptr(GVariant) state = display_state ? g_variant_ref(display_state)
        : current_display_state(bus);
    if (ui_scale < 0) shell_scale = legacy_ui_scale(bus, state);
    if (state && g_variant_n_children(state) == 4) {
      g_autoptr(GVariant) logical = g_variant_get_child_value(state, 2);
      for (gsize i = 0; i < g_variant_n_children(logical); ++i) {
        g_autoptr(GVariant) entry = g_variant_get_child_value(logical, i);
        gint x, y;
        g_variant_get_child(entry, 0, "i", &x);
        g_variant_get_child(entry, 1, "i", &y);
        const auto origin = host_rectangle_to_gdk({x, y, 1, 1},
            host_coordinate_scale(display, state, shell_scale));
        if (origin.x != bounds.x || origin.y != bounds.y) continue;
        g_autoptr(GVariant) specs = g_variant_get_child_value(entry, 5);
        if (!g_variant_n_children(specs)) continue;
        const gchar *connector, *vendor, *product, *serial;
        g_variant_get_child(specs, 0, "(&s&s&s&s)",
            &connector, &vendor, &product, &serial);
        (void)product;
        if (*vendor && *serial) keys.emplace_back(std::string(vendor) + "-" + serial);
        keys.emplace_back(connector);
        break;
      }
    }
  }
  keys.push_back(std::to_string(index));
  g_autoptr(GSettings) settings = g_settings_new_full(schema, nullptr, nullptr);
  g_autofree gchar* raw_sizes = g_settings_get_string(settings, "panel-sizes");
  g_autofree gchar* raw_edges = g_settings_get_string(settings, "panel-positions");
  if (std::strlen(raw_sizes) > 16384 || std::strlen(raw_edges) > 16384) return area;
  g_autoptr(FlJsonMessageCodec) codec = fl_json_message_codec_new();
  g_autoptr(FlValue) sizes = fl_json_message_codec_decode(codec, raw_sizes, nullptr);
  g_autoptr(FlValue) edges = fl_json_message_codec_decode(codec, raw_edges, nullptr);
  auto* size_value = dock_setting(sizes, keys);
  int size = size_value && fl_value_get_type(size_value) == FL_VALUE_TYPE_INT
      ? fl_value_get_int(size_value) : g_settings_get_int(settings, "panel-size");
  g_autofree gchar* fallback_edge = g_settings_get_string(settings, "panel-position");
  auto* edge_value = dock_setting(edges, keys);
  const char* edge = edge_value && fl_value_get_type(edge_value) == FL_VALUE_TYPE_STRING
      ? fl_value_get_string(edge_value) : fallback_edge;
  const int margin = g_settings_get_int(settings, "panel-margin");
  if (size < 16 || size > 256 || margin < 0 || margin > 128 ||
      shell_scale < 1 || shell_scale > 4) return area;
  DockEdge position;
  if (g_str_equal(edge, "TOP")) position = DockEdge::top;
  else if (g_str_equal(edge, "BOTTOM")) position = DockEdge::bottom;
  else if (g_str_equal(edge, "LEFT")) position = DockEdge::left;
  else if (g_str_equal(edge, "RIGHT")) position = DockEdge::right;
  else return area;
  const int thickness = std::lround((size + 2.0 * margin) * shell_scale /
      std::max(1, gdk_monitor_get_scale_factor(monitor)));
  return reserve_dock(area, {bounds.x, bounds.y, bounds.width, bounds.height},
      position, thickness);
}
}  // namespace twodrive
#endif
