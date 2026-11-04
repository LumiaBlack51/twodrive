#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>
#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <cwctype>

#include "flutter_window.h"
#include "utils.h"
#include "instance_identity.h"

namespace {
struct InstanceWindow {
  const wchar_t* property;
  HWND window = nullptr;
};
BOOL CALLBACK FindInstanceWindow(HWND window, LPARAM data) {
  auto* instance = reinterpret_cast<InstanceWindow*>(data);
  if (GetPropW(window, instance->property)) {
    instance->window = window;
    return FALSE;
  }
  return TRUE;
}
}  // namespace

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();

  bool background = std::find(command_line_arguments.begin(), command_line_arguments.end(), "--background") != command_line_arguments.end();
  const auto instance_name = InstanceName(command_line_arguments);
  if (instance_name.empty()) return EXIT_FAILURE;
  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  HANDLE instance_lock = CreateMutexW(nullptr, TRUE, instance_name.c_str());
  if (!instance_lock) return EXIT_FAILURE;
  if (GetLastError() == ERROR_ALREADY_EXISTS) {
    InstanceWindow target{instance_name.c_str()};
    EnumWindows(FindInstanceWindow, reinterpret_cast<LPARAM>(&target));
    HWND existing = target.window;
    // Older default-state builds do not publish the instance property.
    if (!existing && instance_name == L"Local\\TwoDriveDevelopmentDesktop") {
      existing = FindWindowW(nullptr, L"TwoDrive Development");
    }
    if (existing) { ShowWindow(existing, SW_SHOW); SetForegroundWindow(existing); }
    if (instance_lock) CloseHandle(instance_lock);
    CoUninitialize();
    return EXIT_SUCCESS;
  }
  FlutterWindow window(project, background);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1280, 720);
  if (!window.Create(L"TwoDrive Development", origin, size)) {
    CloseHandle(instance_lock);
    return EXIT_FAILURE;
  }
  if (!SetPropW(window.GetHandle(), instance_name.c_str(), reinterpret_cast<HANDLE>(1))) {
    CloseHandle(instance_lock);
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  if (instance_lock) CloseHandle(instance_lock);
  ::CoUninitialize();
  return EXIT_SUCCESS;
}
