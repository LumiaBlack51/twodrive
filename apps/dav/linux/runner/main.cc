#include "my_application.h"

int main(int argc, char** argv) {
  g_autoptr(MyApplication) app = my_application_new();
  for (int i = 1; i < argc; ++i) {
    if (g_str_equal(argv[i], "--tdlink-server")) {
      g_application_set_application_id(G_APPLICATION(app), "app.twodrive.tdlink");
      break;
    }
  }
  return g_application_run(G_APPLICATION(app), argc, argv);
}
