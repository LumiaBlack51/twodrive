#include "status_tray.h"
#include "tray_geometry.h"
#include <unistd.h>
#include <string>
#include <algorithm>
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

// GNOME AppIndicator reserves SNI Activate for double-click. GTK XEmbed is
// retained only for that host, whose legacy tray forwards every primary click.
G_GNUC_BEGIN_IGNORE_DEPRECATIONS

// AppIndicator exposes a menu on primary click. StatusNotifierItem lets the
// desktop invoke Activate directly, opening our Flutter status panel instead.
namespace {
struct Tray {
  FlMethodChannel* channel = nullptr;
  GtkWindow* window = nullptr;
  GDBusConnection* bus = nullptr;
  GDBusNodeInfo* info = nullptr;
  guint object = 0;
  guint owner = 0;
  guint watcher = 0;
  std::string name;
  std::string icon;
  std::string title = "TwoDrive";
  twodrive::TrayAnchor anchor;
  bool host_anchor = false;
  GtkStatusIcon* legacy = nullptr;
  guint32 activation_time = GDK_CURRENT_TIME;
  bool panel = false;
  bool focus_armed = false;
  gint64 dismissed = 0;
  guint reposition = 0;
  guint placement_attempts = 0;
};
const char* kPath = "/StatusNotifierItem";
const char* kInterface = "org.kde.StatusNotifierItem";
const char* kAnchorPath = "/app/twodrive/TrayAnchor";
const char* kAnchorInterface = "app.twodrive.TrayAnchor1";
GVariant* host_call(Tray* tray, const char* method, const char* reply) {
  if (!tray->bus) tray->bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, nullptr);
  if (!tray->bus) return nullptr;
  return g_dbus_connection_call_sync(tray->bus, "org.gnome.Shell", kAnchorPath,
      kAnchorInterface, method, g_variant_new("(u)", static_cast<guint32>(getpid())),
      G_VARIANT_TYPE(reply), G_DBUS_CALL_FLAGS_NO_AUTO_START, 300, nullptr, nullptr);
}
void dismiss(Tray* tray) {
  if (!tray->panel || !gtk_widget_get_visible(GTK_WIDGET(tray->window))) return;
  tray->dismissed = g_get_monotonic_time();
  gtk_widget_hide(GTK_WIDGET(tray->window));
}
gboolean focus_out(GtkWidget*, GdkEventFocus*, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray focus-out: armed=%d", tray->focus_armed);
  if (tray->focus_armed) dismiss(tray);
  tray->focus_armed = false;
  return FALSE;
}
gboolean focus_in(GtkWidget*, GdkEventFocus*, gpointer data) {
  static_cast<Tray*>(data)->focus_armed = true;
  return FALSE;
}
gboolean key_press(GtkWidget*, GdkEventKey* event, gpointer data) {
  if (event->keyval != GDK_KEY_Escape) return FALSE;
  auto* tray = static_cast<Tray*>(data);
  if (!tray->panel) return FALSE;
  dismiss(tray);
  return TRUE;
}
gboolean position_mapped_panel(gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (tray->panel && gtk_widget_get_visible(GTK_WIDGET(tray->window))) {
    ++tray->placement_attempts;
    const auto bounds = tray->anchor.panel();
#ifdef GDK_WINDOWING_X11
    if (GDK_IS_X11_DISPLAY(gtk_widget_get_display(GTK_WIDGET(tray->window))))
      gdk_window_move_resize(gtk_widget_get_window(GTK_WIDGET(tray->window)),
          bounds.x, bounds.y, bounds.width, bounds.height);
#endif
    // Wayland does not accept application-selected absolute window positions.
    // GNOME places only this PID's panel using the actual compositor tray actor.
    if (tray->host_anchor) {
      g_autoptr(GVariant) result = host_call(tray, "PositionPanel", "(b)");
      gboolean placed = FALSE;
      if (result) g_variant_get(result, "(b)", &placed);
      // A Wayland surface can map before the compositor has registered its
      // MetaWindow. Retry briefly, including initial configure acknowledgements.
      if (result && tray->placement_attempts < 20 &&
          (!placed || tray->placement_attempts < 4)) return G_SOURCE_CONTINUE;
    }
    gtk_widget_set_opacity(GTK_WIDGET(tray->window), 1.0);
  }
  tray->reposition = 0;
  return G_SOURCE_REMOVE;
}
gboolean mapped(GtkWidget*, GdkEvent*, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (tray->panel && !tray->reposition) {
    tray->placement_attempts = 0;
    tray->reposition = g_timeout_add(25, position_mapped_panel, tray);
  }
  return FALSE;
}
void capture_anchor(Tray* tray, gint x, gint y) {
  g_autoptr(GVariant) host = host_call(tray, "GetAnchor", "(biiiiiiii)");
  gboolean found = FALSE;
  gint ix, iy, iw, ih, ax, ay, aw, ah;
  if (host) {
    g_variant_get(host, "(biiiiiiii)", &found, &ix, &iy, &iw, &ih, &ax, &ay, &aw, &ah);
    if (found && iw > 0 && ih > 0 && aw > 0 && ah > 0) {
      tray->anchor.capture_icon({ix, iy, iw, ih}, {ax, ay, aw, ah});
      tray->host_anchor = true;
      return;
    }
  }
  tray->host_anchor = false;
  auto* display = gtk_widget_get_display(GTK_WIDGET(tray->window));
  if (x == 0 && y == 0) {
    auto* pointer = gdk_seat_get_pointer(gdk_display_get_default_seat(display));
    gdk_device_get_position(pointer, nullptr, &x, &y);
  }
  auto* monitor = gdk_display_get_monitor_at_point(display, x, y);
  GdkRectangle area;
  gdk_monitor_get_workarea(monitor, &area);
  tray->anchor.capture(x, y, {area.x, area.y, area.width, area.height});
  if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray anchor: point=%d,%d work=%d,%d,%d,%d", x, y, area.x, area.y, area.width, area.height);
}
const char* kXml = R"XML(
<node><interface name="org.kde.StatusNotifierItem">
 <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
 <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
 <method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
 <method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
 <method name="ProvideXdgActivationToken"><arg type="s" direction="in"/></method>
 <property name="Category" type="s" access="read"/>
 <property name="Id" type="s" access="read"/>
 <property name="Title" type="s" access="read"/>
 <property name="Status" type="s" access="read"/>
 <property name="WindowId" type="u" access="read"/>
 <property name="IconName" type="s" access="read"/>
 <property name="IconPixmap" type="a(iiay)" access="read"/>
 <property name="OverlayIconName" type="s" access="read"/>
 <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
 <property name="AttentionIconName" type="s" access="read"/>
 <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
 <property name="AttentionMovieName" type="s" access="read"/>
 <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
 <property name="ItemIsMenu" type="b" access="read"/>
 <property name="Menu" type="o" access="read"/>
 <signal name="NewIcon"/><signal name="NewTitle"/>
 <signal name="NewToolTip"/><signal name="NewStatus"><arg type="s"/></signal>
</interface></node>)XML";

GVariant* property(GDBusConnection*, const gchar*, const gchar*, const gchar*,
                   const gchar* key, GError**, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (g_str_equal(key, "Category")) return g_variant_new_string("ApplicationStatus");
  if (g_str_equal(key, "Id")) return g_variant_new_string("twodrive-development");
  if (g_str_equal(key, "Title")) return g_variant_new_string(tray->title.c_str());
  if (g_str_equal(key, "Status")) return g_variant_new_string("Active");
  if (g_str_equal(key, "WindowId")) return g_variant_new_uint32(0);
  if (g_str_equal(key, "IconName")) return g_variant_new_string(tray->icon.c_str());
  if (g_str_equal(key, "ItemIsMenu")) return g_variant_new_boolean(false);
  if (g_str_equal(key, "Menu")) return g_variant_new_object_path("/");
  if (g_str_equal(key, "ToolTip")) return g_variant_new("(s@a(iiay)ss)",
      tray->icon.c_str(), g_variant_new_array(G_VARIANT_TYPE("(iiay)"), nullptr, 0),
      tray->title.c_str(), "打开同步状态");
  if (g_str_has_suffix(key, "Pixmap"))
    return g_variant_new_array(G_VARIANT_TYPE("(iiay)"), nullptr, 0);
  return g_variant_new_string("");
}
void activate(Tray* tray, gint x, gint y) {
    if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray activate: panel=%d visible=%d point=%d,%d", tray->panel, gtk_widget_get_visible(GTK_WIDGET(tray->window)), x, y);
    // Close synchronously on the native event. A tray click can first take
    // focus away; don't reopen the panel on that same click's Activate.
    if (tray->panel && (gtk_widget_get_visible(GTK_WIDGET(tray->window)) ||
        g_get_monotonic_time() - tray->dismissed < 250000)) {
      dismiss(tray);
      return;
    }
    capture_anchor(tray, x, y);
    g_autoptr(FlValue) args = fl_value_new_map();
    fl_value_set_string_take(args, "x", fl_value_new_int(x));
    fl_value_set_string_take(args, "y", fl_value_new_int(y));
    fl_method_channel_invoke_method(tray->channel, "activate", args, nullptr, nullptr, nullptr);
}
void legacy_activate(GtkStatusIcon* icon, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  (void)icon;
  gint x = 0, y = 0;
  g_autoptr(GdkEvent) event = gtk_get_current_event();
  if (event && (event->type == GDK_KEY_PRESS || event->type == GDK_KEY_RELEASE) &&
      tray->anchor.valid) {
    x = tray->anchor.x;
    y = tray->anchor.y;
  }
  // GNOME forwards synthetic icon-local coordinates as x_root/y_root. The
  // hidden XEmbed socket is also not the visible Shell clone's geometry.
  // Zero asks capture_anchor to sample the pointer now, before Dart awaits.
  tray->activation_time = gtk_get_current_event_time();
  activate(tray, x, y);
}
void legacy_menu(GtkStatusIcon* icon, guint, guint32 time, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  legacy_activate(icon, data);
  tray->activation_time = time;
}
void method(GDBusConnection*, const gchar*, const gchar*, const gchar*,
            const gchar* name, GVariant* parameters,
            GDBusMethodInvocation* invocation, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (g_str_equal(name, "ProvideXdgActivationToken")) {
    const gchar* token = nullptr;
    g_variant_get(parameters, "(&s)", &token);
    gtk_window_set_startup_id(tray->window, token);
    g_dbus_method_invocation_return_value(invocation, nullptr);
    return;
  }
  if (!g_str_equal(name, "Scroll")) {
    gint x = 0, y = 0;
    g_variant_get(parameters, "(ii)", &x, &y);
    activate(tray, x, y);
  }
  g_dbus_method_invocation_return_value(invocation, nullptr);
}
const GDBusInterfaceVTable kVtable = {method, property, nullptr, {nullptr}};
void watcher_appeared(GDBusConnection* bus, const gchar*, const gchar*, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  g_dbus_connection_call(bus, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
      "org.kde.StatusNotifierWatcher", "RegisterStatusNotifierItem",
      g_variant_new("(s)", tray->name.c_str()), nullptr, G_DBUS_CALL_FLAGS_NONE,
      3000, nullptr, nullptr, nullptr);
}
void stop(Tray* tray) {
  if (tray->reposition) { g_source_remove(tray->reposition); tray->reposition = 0; }
  if (tray->legacy) { gtk_status_icon_set_visible(tray->legacy, FALSE); g_clear_object(&tray->legacy); }
  if (tray->watcher) { g_bus_unwatch_name(tray->watcher); tray->watcher = 0; }
  if (tray->owner) { g_bus_unown_name(tray->owner); tray->owner = 0; }
  if (tray->object) { g_dbus_connection_unregister_object(tray->bus, tray->object); tray->object = 0; }
}
void call(FlMethodChannel*, FlMethodCall* request, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  const char* name = fl_method_call_get_name(request);
  if (g_str_equal(name, "panel")) {
    auto* args = fl_method_call_get_args(request);
    const bool panel = fl_value_get_bool(fl_value_lookup_string(args, "enabled"));
    tray->panel = panel;
    tray->focus_armed = false;
    if (!panel) {
      if (tray->reposition) g_source_remove(tray->reposition);
      tray->reposition = 0;
      gtk_widget_set_opacity(GTK_WIDGET(tray->window), 1.0);
    }
    if (panel && !tray->anchor.valid) capture_anchor(tray, 0, 0);
    const auto bounds = tray->anchor.panel();
    if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray bounds: %d,%d %dx%d", bounds.x, bounds.y, bounds.width, bounds.height);
    gtk_window_set_decorated(tray->window, !panel);
    gtk_window_set_resizable(tray->window, TRUE);
    GdkGeometry geometry = {};
    geometry.min_width = panel ? bounds.width : 840;
    geometry.min_height = panel ? bounds.height : 580;
    geometry.max_width = bounds.width;
    geometry.max_height = bounds.height;
    gtk_window_set_geometry_hints(tray->window, nullptr, &geometry,
        static_cast<GdkWindowHints>(panel ? GDK_HINT_MIN_SIZE | GDK_HINT_MAX_SIZE : GDK_HINT_MIN_SIZE));
    gtk_window_resize(tray->window, panel ? bounds.width : 1120, panel ? bounds.height : 760);
    if (panel) {
      if (tray->host_anchor) gtk_widget_set_opacity(GTK_WIDGET(tray->window), 0.0);
      gtk_window_move(tray->window, bounds.x, bounds.y);
      gtk_widget_show(GTK_WIDGET(tray->window));
      gtk_window_present_with_time(tray->window, tray->activation_time);
    }
  } else if (g_str_equal(name, "destroy")) {
    stop(tray);
  } else if (g_str_equal(name, "configure")) {
    FlValue* args = fl_method_call_get_args(request);
    FlValue* icon = fl_value_lookup_string(args, "icon");
    if (!icon || fl_value_get_type(icon) != FL_VALUE_TYPE_STRING) {
      fl_method_call_respond_error(request, "invalid-argument", "icon is required", nullptr, nullptr);
      return;
    }
    tray->icon = fl_value_get_string(icon);
    FlValue* title = fl_value_lookup_string(args, "title");
    tray->title = title && fl_value_get_type(title) == FL_VALUE_TYPE_STRING
        ? fl_value_get_string(title) : "TwoDrive";
    const char* desktop = g_getenv("XDG_CURRENT_DESKTOP");
    bool gnome = desktop && (g_strrstr(desktop, "GNOME") || g_strrstr(desktop, "Zorin"));
#ifdef GDK_WINDOWING_X11
    gnome = gnome && GDK_IS_X11_DISPLAY(gtk_widget_get_display(GTK_WIDGET(tray->window)));
#else
    gnome = false;
#endif
    if (g_strcmp0(g_getenv("TWODRIVE_TRAY_BACKEND"), "sni") == 0) gnome = false;
    if (gnome) {
      if (!tray->legacy) {
        tray->legacy = gtk_status_icon_new();
        gtk_status_icon_set_name(tray->legacy, "twodrive-development");
        g_signal_connect(tray->legacy, "activate", G_CALLBACK(legacy_activate), tray);
        g_signal_connect(tray->legacy, "popup-menu", G_CALLBACK(legacy_menu), tray);
      }
      gtk_status_icon_set_title(tray->legacy, tray->title.c_str());
      gtk_status_icon_set_tooltip_text(tray->legacy, tray->title.c_str());
      gtk_status_icon_set_from_file(tray->legacy, tray->icon.c_str());
      gtk_status_icon_set_visible(tray->legacy, TRUE);
      fl_method_call_respond_success(request, nullptr, nullptr);
      return;
    }
    if (!tray->bus) {
      g_autoptr(GError) error = nullptr;
      tray->bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &error);
      if (!tray->bus) {
        fl_method_call_respond_error(request, "dbus", "session bus unavailable", nullptr, nullptr);
        return;
      }
    }
    if (!tray->object) {
      tray->name = "org.kde.StatusNotifierItem-" + std::to_string(getpid()) + "-1";
      tray->info = g_dbus_node_info_new_for_xml(kXml, nullptr);
      tray->object = g_dbus_connection_register_object(tray->bus, kPath, tray->info->interfaces[0],
          &kVtable, tray, nullptr, nullptr);
      tray->owner = g_bus_own_name_on_connection(tray->bus, tray->name.c_str(),
          G_BUS_NAME_OWNER_FLAGS_NONE, nullptr, nullptr, nullptr, nullptr);
      tray->watcher = g_bus_watch_name_on_connection(tray->bus, "org.kde.StatusNotifierWatcher",
          G_BUS_NAME_WATCHER_FLAGS_NONE, watcher_appeared, nullptr, tray, nullptr);
    }
    g_dbus_connection_emit_signal(tray->bus, nullptr, kPath, kInterface, "NewIcon", nullptr, nullptr);
    g_dbus_connection_emit_signal(tray->bus, nullptr, kPath, kInterface, "NewTitle", nullptr, nullptr);
    g_dbus_connection_emit_signal(tray->bus, nullptr, kPath, kInterface, "NewToolTip", nullptr, nullptr);
  } else {
    fl_method_call_respond_not_implemented(request, nullptr);
    return;
  }
  fl_method_call_respond_success(request, nullptr, nullptr);
}
void dispose(gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  g_signal_handlers_disconnect_by_data(tray->window, tray);
  stop(tray);
  if (tray->info) g_dbus_node_info_unref(tray->info);
  g_clear_object(&tray->bus);
  delete tray;
}
}  // namespace
static void quit_action(GSimpleAction*, GVariant*, gpointer data) {
  auto* channel = FL_METHOD_CHANNEL(data);
  fl_method_channel_invoke_method(channel, "quit", nullptr, nullptr, nullptr, nullptr);
}
void register_status_tray(FlView* view) {
  auto* tray = new Tray();
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  tray->channel = fl_method_channel_new(fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "twodrive/status_tray", FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(tray->channel, call, tray, dispose);
  auto* window = GTK_WINDOW(gtk_widget_get_toplevel(GTK_WIDGET(view)));
  tray->window = window;
  g_signal_connect(window, "focus-out-event", G_CALLBACK(focus_out), tray);
  g_signal_connect(window, "focus-in-event", G_CALLBACK(focus_in), tray);
  g_signal_connect(window, "key-press-event", G_CALLBACK(key_press), tray);
  g_signal_connect(window, "map-event", G_CALLBACK(mapped), tray);
  auto* app = gtk_window_get_application(window);
  if (app) {
    g_autoptr(GSimpleAction) action = g_simple_action_new("quit", nullptr);
    g_signal_connect(action, "activate", G_CALLBACK(quit_action), tray->channel);
    g_action_map_add_action(G_ACTION_MAP(app), G_ACTION(action));
    const gchar* shortcuts[] = {"<Primary>q", nullptr};
    gtk_application_set_accels_for_action(app, "app.quit", shortcuts);
  }
  g_object_set_data_full(G_OBJECT(view), "twodrive-status-tray", tray->channel, g_object_unref);
}

G_GNUC_END_IGNORE_DEPRECATIONS
