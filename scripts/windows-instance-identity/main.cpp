#include <windows.h>
#include <algorithm>
#include <cstdint>
#include <cwctype>
#include <filesystem>
#include <iostream>
#include <string>
#include <vector>
#ifdef USE_LEGACY_IDENTITY
#include "legacy_identity.h"
#else
#include "../../apps/dav/windows/runner/instance_identity.h"
#endif

bool SameMutex(const std::wstring& first, const std::wstring& second) {
  // Isolated namespace; never acquire or activate a real client's mutex.
  const auto prefix = L"Local\\TDIdentityTest-" + std::to_wstring(GetCurrentProcessId()) + L"-";
  const auto label = [&](const std::wstring& s) { return prefix + s.substr(s.find(L'\\') + 1); };
  HANDLE a = CreateMutexW(nullptr, FALSE, label(first).c_str());
  if (!a) return false;
  HANDLE b = CreateMutexW(nullptr, FALSE, label(second).c_str());
  const bool same = b && GetLastError() == ERROR_ALREADY_EXISTS;
  if (b) CloseHandle(b);
  CloseHandle(a);
  return same;
}
int main(int argc, char** argv) {
  if (argc != 2) return 2;
  const auto root = std::filesystem::u8path(argv[1]);
  const auto data = root / L"local-data";
  const auto state = data / L"twodrive-dav" / L"state";
  std::filesystem::create_directories(state);
  SetEnvironmentVariableW(L"LOCALAPPDATA", data.c_str());
  const auto implicit = InstanceName({});
  const auto explicit_default = InstanceName({"--state", state.u8string()});
  if (!SameMutex(implicit, explicit_default) || !SameMutex(explicit_default, implicit)) {
    std::cerr << "implicit/explicit default state must share a mutex in both launch orders\n";
    return 1;
  }
  if (implicit != L"Local\\TwoDriveDevelopmentDesktop") return 3;
  const auto a = InstanceName({"--state", (root / L"a").u8string()});
  const auto alias = InstanceName({"--state", (root / L"a" / L".").u8string()});
  const auto b = InstanceName({"--state", (root / L"b").u8string()});
  if (!SameMutex(a, alias) || SameMutex(a, b) || SameMutex(a, implicit)) return 4;
  SetEnvironmentVariableW(L"LOCALAPPDATA", nullptr);
  const auto home = root / L"home";
  SetEnvironmentVariableW(L"USERPROFILE", home.c_str());
  const auto fallback = home / L"AppData" / L"Local" / L"twodrive-dav" / L"state";
  const auto fallback_explicit = InstanceName({"--state", fallback.u8string()});
  if (!SameMutex(InstanceName({}), fallback_explicit) || !SameMutex(fallback_explicit, InstanceName({}))) return 5;
  std::cout << "{\"passed\":true,\"checks\":{\"implicit_then_explicit_default\":true,\"explicit_then_implicit_default\":true,\"legacy_default_mutex_preserved\":true,\"canonical_custom_alias\":true,\"separate_custom_states\":true,\"userprofile_fallback_both_orders\":true}}\n";
  return 0;
}
