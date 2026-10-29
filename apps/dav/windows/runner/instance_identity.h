#ifndef RUNNER_INSTANCE_IDENTITY_H_
#define RUNNER_INSTANCE_IDENTITY_H_
#include <algorithm>
#include <cstdint>
#include <windows.h>
#include <cwctype>
#include <filesystem>
#include <string>
#include <optional>
#include <vector>

inline std::optional<std::wstring> DesktopEnvironment(const wchar_t* name) {
  SetLastError(ERROR_SUCCESS);
  DWORD length = GetEnvironmentVariableW(name, nullptr, 0);
  if (!length) return GetLastError() == ERROR_ENVVAR_NOT_FOUND
      ? std::nullopt : std::optional<std::wstring>(L"");
  std::wstring value(length, L'\0');
  const DWORD copied = GetEnvironmentVariableW(name, value.data(), length);
  if (copied >= length) return std::nullopt;
  value.resize(copied);
  return value;
}
inline std::filesystem::path DefaultDesktopState() {
  if (const auto data = DesktopEnvironment(L"LOCALAPPDATA"))
    return std::filesystem::path(*data) / L"twodrive-dav" / L"state";
  const auto home = DesktopEnvironment(L"USERPROFILE");
  return (home ? std::filesystem::path(*home) : std::filesystem::current_path()) /
         L"AppData" / L"Local" / L"twodrive-dav" / L"state";
}

inline std::wstring CanonicalDesktopState(const std::filesystem::path& input) {
  std::error_code error;
  auto path = std::filesystem::weakly_canonical(input, error);
  if (error) return L"";
  path = path.lexically_normal().make_preferred();
  while (!path.has_filename() && path != path.root_path()) path = path.parent_path();
  auto normalized = path.native();
  std::transform(normalized.begin(), normalized.end(), normalized.begin(),
                 [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });
  return normalized;
}

inline std::wstring InstanceName(const std::vector<std::string>& arguments) {
  const std::wstring base = L"Local\\TwoDriveDevelopmentDesktop";
  const auto default_path = CanonicalDesktopState(DefaultDesktopState());
  if (default_path.empty()) return L"";
  auto state = std::find(arguments.begin(), arguments.end(), "--state");
  const auto normalized = state == arguments.end() || ++state == arguments.end()
      ? default_path : CanonicalDesktopState(std::filesystem::u8path(*state));
  if (normalized.empty()) return L"";
  // Retain the older default-state mutex, for both implicit and explicit use.
  if (normalized == default_path) return base;
  uint64_t hash = 14695981039346656037ULL;
  for (wchar_t c : normalized) {
    hash ^= static_cast<uint16_t>(c);
    hash *= 1099511628211ULL;
  }
  return base + L"-" + std::to_wstring(hash);
}
#endif  // RUNNER_INSTANCE_IDENTITY_H_
