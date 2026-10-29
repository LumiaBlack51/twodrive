#define _GNU_SOURCE
#include <gtk/gtk.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
static void check(GtkWindow *window,const char *operation) {
 const char *marker=getenv("TWODRIVE_TRANSITION_TEST_MARKER");
 if(marker && access(marker,F_OK)==0 && gtk_widget_get_visible(GTK_WIDGET(window)) && gtk_widget_get_opacity(GTK_WIDGET(window))>0.01)
  fprintf(stderr,"TRANSITION_EXPOSED %s\n",operation);
}
void gtk_window_resize(GtkWindow *w,gint width,gint height) {
 static void(*original)(GtkWindow*,gint,gint);if(!original)original=dlsym(RTLD_NEXT,"gtk_window_resize");
 check(w,"resize");original(w,width,height);
}
void gtk_window_set_decorated(GtkWindow *w,gboolean decorated) {
 static void(*original)(GtkWindow*,gboolean);if(!original)original=dlsym(RTLD_NEXT,"gtk_window_set_decorated");
 check(w,"decoration");original(w,decorated);
}
void gtk_window_set_geometry_hints(GtkWindow *w,GtkWidget *widget,GdkGeometry *geometry,GdkWindowHints flags) {
 static void(*original)(GtkWindow*,GtkWidget*,GdkGeometry*,GdkWindowHints);if(!original)original=dlsym(RTLD_NEXT,"gtk_window_set_geometry_hints");
 check(w,"geometry-hints");original(w,widget,geometry,flags);
}
