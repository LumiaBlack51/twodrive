#include "status_tray.h"
#include "tray_geometry.h"
#include "tray_workarea.h"
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
  GtkWindow* popup = nullptr;
  GtkCssProvider* decoration = nullptr;
  FlView* primary = nullptr;
  GtkWindow* dashboard = nullptr;
  FlView* content = nullptr;
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
  bool frame_ready = false;
  bool placed = false;
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
// Flutter 3.41's GTK embedder derives app lifecycle only from its primary
// view. Keep secondary-view rendering active while the dashboard is minimized.
void sync_hidden_dashboard_lifecycle(Tray* tray) {
  auto* primary = gtk_widget_get_toplevel(GTK_WIDGET(tray->primary));
  auto* surface = gtk_widget_get_window(primary);
  if (!surface || (gtk_widget_get_mapped(primary) &&
      !(gdk_window_get_state(surface) & (GDK_WINDOW_STATE_WITHDRAWN | GDK_WINDOW_STATE_ICONIFIED)))) return;
  const char* state = gtk_widget_get_mapped(GTK_WIDGET(tray->popup))
      ? "AppLifecycleState.resumed" : "AppLifecycleState.hidden";
  g_autoptr(GBytes) message = g_bytes_new(state, strlen(state));
  fl_binary_messenger_send_on_channel(fl_engine_get_binary_messenger(fl_view_get_engine(tray->primary)),
      "flutter/lifecycle", message, nullptr, nullptr, nullptr);
}
void dismiss(Tray* tray) {
  if (!tray->panel || !gtk_widget_get_visible(GTK_WIDGET(tray->popup))) return;
  tray->dismissed = g_get_monotonic_time();
  gtk_widget_hide(GTK_WIDGET(tray->popup));
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
void configure_popup_decoration(Tray* tray) {
  auto* popup = tray->popup;
  auto* widget = GTK_WIDGET(popup);
  auto* screen = gtk_widget_get_screen(widget);
#ifdef GDK_WINDOWING_X11
  if (GDK_IS_X11_SCREEN(screen) &&
      (!gdk_screen_is_composited(screen) || !gdk_screen_get_rgba_visual(screen) ||
       !gdk_x11_screen_supports_net_wm_hint(screen, gdk_atom_intern_static_string("_GTK_FRAME_EXTENTS")))) {
    gtk_window_set_decorated(popup, FALSE);
    return;
  }
#endif
  // A hidden custom titlebar opts the popup into GTK client-side decoration.
  // Its shadow is painted with the content, including the first visible frame.
  gtk_window_set_titlebar(popup, gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 0));
  gtk_widget_set_name(widget, "twodrive-tray-popup");
  tray->decoration = gtk_css_provider_new();
  gtk_css_provider_load_from_data(tray->decoration,
      "#twodrive-tray-popup.csd decoration, #twodrive-tray-popup.csd decoration:backdrop {"
      " margin: 0; padding: 0; border: 0; border-radius: 0; transition: none;"
      " box-shadow: 0 3px 9px rgba(0,0,0,0.22); }", -1, nullptr);
  gtk_style_context_add_provider_for_screen(screen,
      GTK_STYLE_PROVIDER(tray->decoration), GTK_STYLE_PROVIDER_PRIORITY_APPLICATION);
}
void reveal_when_ready(Tray* tray) {
  if (tray->placed && tray->frame_ready && gtk_widget_get_mapped(GTK_WIDGET(tray->popup)))
    gtk_widget_set_opacity(GTK_WIDGET(tray->popup), 1.0);
}
gboolean position_mapped_panel(gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (tray->panel && gtk_widget_get_mapped(GTK_WIDGET(tray->popup))) {
#ifdef GDK_WINDOWING_X11
    if (GDK_IS_X11_DISPLAY(gtk_widget_get_display(GTK_WIDGET(tray->popup)))) {
      // GTK has already requested these bounds before map. Correct the WM only
      // if it placed us elsewhere. Shell and XWayland use different units.
      const auto bounds = tray->anchor.panel();
      gint x, y, width, height;
      // GtkWindow coordinates describe the panel itself, excluding its shadow.
      gtk_window_get_position(tray->popup, &x, &y);
      gtk_window_get_size(tray->popup, &width, &height);
      if (x != bounds.x || y != bounds.y)
        gtk_window_move(tray->popup, bounds.x, bounds.y);
      if (width != bounds.width || height != bounds.height)
        gtk_window_resize(tray->popup, bounds.width, bounds.height);
    } else
#endif
    if (tray->host_anchor) {
      g_autoptr(GVariant) result = host_call(tray, "PositionTray", "(b)");
      gboolean placed = FALSE;
      if (result) g_variant_get(result, "(b)", &placed);
      // A Wayland surface may map before GNOME registers its MetaWindow.
      if (result && !placed && ++tray->placement_attempts < 20) return G_SOURCE_CONTINUE;
    }
    tray->placed = true;
    reveal_when_ready(tray);
  }
  tray->reposition = 0;
  return G_SOURCE_REMOVE;
}
gboolean mapped(GtkWidget*, GdkEvent*, gpointer data) {
  auto* tray = static_cast<Tray*>(data);
  if (tray->panel) {
    // Keep the mapped utility surface out of the taskbar and window switcher.
    gtk_window_set_skip_taskbar_hint(tray->popup, TRUE);
    gtk_window_set_skip_pager_hint(tray->popup, TRUE);
  }
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
      auto* display = gtk_widget_get_display(GTK_WIDGET(tray->popup));
      g_autoptr(GVariant) state = twodrive::current_display_state(tray->bus);
      const int ui_scale = twodrive::legacy_ui_scale(tray->bus, state);
      const auto scale = twodrive::host_coordinate_scale(display, state, ui_scale);
      const auto icon = twodrive::host_rectangle_to_gdk({ix, iy, iw, ih}, scale);
      const auto area = twodrive::host_rectangle_to_gdk({ax, ay, aw, ah}, scale);
      auto* monitor = gdk_display_get_monitor_at_point(display,
          icon.x + icon.width / 2, icon.y + icon.height / 2);
      const auto usable = twodrive::configured_dock_workarea(
          tray->bus, monitor, area, state, ui_scale);
      tray->anchor.capture_icon(icon, usable);
      tray->host_anchor = true;
      return;
    }
  }
  tray->host_anchor = false;
  auto* display = gtk_widget_get_display(GTK_WIDGET(tray->popup));
  if (x == 0 && y == 0) {
    auto* pointer = gdk_seat_get_pointer(gdk_display_get_default_seat(display));
    gdk_device_get_position(pointer, nullptr, &x, &y);
  }
  auto* monitor = gdk_display_get_monitor_at_point(display, x, y);
  GdkRectangle area;
  gdk_monitor_get_workarea(monitor, &area);
  const auto usable = twodrive::configured_dock_workarea(tray->bus, monitor,
      {area.x, area.y, area.width, area.height});
  tray->anchor.capture(x, y, usable);
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
      tray->title.c_str(), "");
  if (g_str_has_suffix(key, "Pixmap"))
    return g_variant_new_array(G_VARIANT_TYPE("(iiay)"), nullptr, 0);
  return g_variant_new_string("");
}
void activate(Tray* tray, gint x, gint y) {
    if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray activate: panel=%d visible=%d point=%d,%d", tray->panel, gtk_widget_get_visible(GTK_WIDGET(tray->popup)), x, y);
    // Close synchronously on the native event. A tray click can first take
    // focus away; don't reopen the panel on that same click's Activate.
    if (tray->panel && (gtk_widget_get_visible(GTK_WIDGET(tray->popup)) ||
        g_get_monotonic_time() - tray->dismissed < 250000)) {
      dismiss(tray);
      return;
    }
    capture_anchor(tray, x, y);
    if (!tray->content) {
      tray->content = fl_view_new_for_engine(fl_view_get_engine(tray->primary));
      GdkRGBA background;
      gdk_rgba_parse(&background, "#fafbfe");
      fl_view_set_background_color(tray->content, &background);
      g_signal_connect(tray->content, "first-frame", G_CALLBACK(+[](FlView*, Tray* t) {
        t->frame_ready = true;
        reveal_when_ready(t);
      }), tray);
      gtk_container_add(GTK_CONTAINER(tray->popup), GTK_WIDGET(tray->content));
      gtk_widget_show(GTK_WIDGET(tray->content));
    }
    const auto bounds = tray->anchor.panel();
    gtk_window_set_default_size(tray->popup, bounds.width, bounds.height);
    gtk_window_resize(tray->popup, bounds.width, bounds.height);
    gtk_window_move(tray->popup, bounds.x, bounds.y);
    tray->panel = true;
    tray->placed = false;
    gtk_widget_set_opacity(GTK_WIDGET(tray->popup), 0);
    gtk_widget_show(GTK_WIDGET(tray->popup));
    gtk_window_present_with_time(tray->popup, tray->activation_time);
    gtk_widget_grab_focus(GTK_WIDGET(tray->content));
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
    gtk_window_set_startup_id(tray->popup, token);
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
  if (tray->dashboard) {
    g_signal_handlers_disconnect_by_data(tray->dashboard, tray);
    g_object_remove_weak_pointer(G_OBJECT(tray->dashboard), reinterpret_cast<gpointer*>(&tray->dashboard));
    tray->dashboard = nullptr;
  }
  if (tray->popup) {
    g_signal_handlers_disconnect_by_data(tray->popup, tray);
    // On X11 the Flutter 3.41 embedder can release a secondary framebuffer
    // on the GTK thread while its EGL rendering context is busy elsewhere.
    // Keep a GDK context current until the popup has released its GL objects.
    // Unparenting can invalidate the drawing surface/current context. Hold
    // the view through that step, then dispose it while the popup still exists.
    if (tray->content) {
      g_signal_handlers_disconnect_by_data(tray->content, tray);
      g_object_ref(tray->content);
      gtk_container_remove(GTK_CONTAINER(tray->popup), GTK_WIDGET(tray->content));
    }
    g_autoptr(GdkGLContext) cleanup = nullptr;
    if (tray->content && g_strcmp0(g_getenv("FLUTTER_LINUX_RENDERER"), "software") != 0) {
      auto* surface = gtk_widget_get_window(GTK_WIDGET(tray->popup));
      if (surface) {
        g_autoptr(GError) error = nullptr;
        cleanup = gdk_window_create_gl_context(surface, &error);
        if (cleanup && gdk_gl_context_realize(cleanup, &error)) {
          // GDK caches its last context. Flutter can change the underlying
          // EGL binding directly, so force a real rebind on the GTK thread.
          gdk_gl_context_clear_current();
          gdk_gl_context_make_current(cleanup);
          if (gdk_gl_context_get_current() != cleanup)
            g_warning("Tray shutdown: could not activate the GDK cleanup context");
        } else {
          g_warning("Tray shutdown: GDK cleanup context unavailable: %s",
                    error ? error->message : "unknown error");
        }
      }
    }
    if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray disposing detached view");
    g_clear_object(&tray->content);
    if (g_getenv("TWODRIVE_TRAY_TRACE")) g_message("tray detached view released");
    if (tray->decoration) {
      gtk_style_context_remove_provider_for_screen(gtk_widget_get_screen(GTK_WIDGET(tray->popup)),
          GTK_STYLE_PROVIDER(tray->decoration));
      g_clear_object(&tray->decoration);
    }
    gtk_widget_destroy(GTK_WIDGET(tray->popup));
    if (cleanup) gdk_gl_context_clear_current();
    tray->popup = nullptr;
  }
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
    dismiss(tray);
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
    gnome = gnome && GDK_IS_X11_DISPLAY(gtk_widget_get_display(GTK_WIDGET(tray->popup)));
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
  tray->dashboard = window;
  g_object_add_weak_pointer(G_OBJECT(window), reinterpret_cast<gpointer*>(&tray->dashboard));
  tray->primary = view;
  tray->popup = GTK_WINDOW(gtk_window_new(GTK_WINDOW_TOPLEVEL));
  gtk_window_set_title(tray->popup, "TwoDrive Tray");
  gtk_window_set_role(tray->popup, "twodrive-tray");
  gtk_window_set_type_hint(tray->popup, GDK_WINDOW_TYPE_HINT_UTILITY);
  configure_popup_decoration(tray);
  gtk_window_set_keep_above(tray->popup, TRUE);
  gtk_window_set_skip_taskbar_hint(tray->popup, TRUE);
  gtk_window_set_skip_pager_hint(tray->popup, TRUE);
  g_signal_connect(tray->popup, "delete-event", G_CALLBACK(gtk_widget_hide_on_delete), nullptr);
  g_signal_connect(tray->popup, "focus-out-event", G_CALLBACK(focus_out), tray);
  g_signal_connect(tray->popup, "focus-in-event", G_CALLBACK(focus_in), tray);
  g_signal_connect(tray->popup, "key-press-event", G_CALLBACK(key_press), tray);
  g_signal_connect(tray->popup, "map-event", G_CALLBACK(mapped), tray);
  g_signal_connect(tray->popup, "map", G_CALLBACK(+[](GtkWidget*, Tray* t) {
    sync_hidden_dashboard_lifecycle(t);
    fl_method_channel_invoke_method(t->channel, "trayVisible", nullptr, nullptr, nullptr, nullptr);
  }), tray);
  g_signal_connect(tray->popup, "unmap", G_CALLBACK(+[](GtkWidget*, Tray* t) {
    sync_hidden_dashboard_lifecycle(t);
    fl_method_channel_invoke_method(t->channel, "trayHidden", nullptr, nullptr, nullptr, nullptr);
  }), tray);
  g_signal_connect_after(window, "window-state-event", G_CALLBACK(+[](GtkWidget*, GdkEventWindowState*, Tray* t) -> gboolean {
    if (gtk_widget_get_mapped(GTK_WIDGET(t->popup))) sync_hidden_dashboard_lifecycle(t);
    return FALSE;
  }), tray);
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
