#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include "reservation_protocol.h"
#include <cstdio>
#include <cwchar>
#include <string>

using InitializeDiagnostics = HRESULT(WINAPI*)(LPCWSTR, DWORD, LPCWSTR, LPCWSTR, CLSID, LPCWSTR);

int wmain(int argc, wchar_t** argv) {
    DWORD process = 0;
    std::wstring dll;
    for (int i = 1; i + 1 < argc; i += 2) {
        if (std::wcscmp(argv[i], L"--pid") == 0) process = std::wcstoul(argv[i + 1], nullptr, 10);
        else if (std::wcscmp(argv[i], L"--dll") == 0) dll = argv[i + 1];
        else { std::fwprintf(stderr, L"Unknown argument: %s\n", argv[i]); return 2; }
    }
    if (!process || dll.empty() || dll.size() < 3 || dll[1] != L':' || GetFileAttributesW(dll.c_str()) == INVALID_FILE_ATTRIBUTES) {
        std::fwprintf(stderr, L"Usage: injector --pid <Explorer PID> --dll <absolute TAP DLL>\n");
        return 2;
    }
    // Use the system WUX implementation; never search the current directory.
    HMODULE xaml = LoadLibraryExW(L"Windows.UI.Xaml.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!xaml) { std::fwprintf(stderr, L"Windows.UI.Xaml.dll: %lu\n", GetLastError()); return 3; }
    auto initialize = reinterpret_cast<InitializeDiagnostics>(GetProcAddress(xaml, "InitializeXamlDiagnosticsEx"));
    if (!initialize) { std::fwprintf(stderr, L"InitializeXamlDiagnosticsEx is unavailable\n"); FreeLibrary(xaml); return 3; }
    HRESULT result = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    for (unsigned i = 1; i <= 10000; ++i) {
        const auto connection = L"VisualDiagConnection" + std::to_wstring(i);
        result = initialize(connection.c_str(), process, L"", dll.c_str(), capper::kTapClsid, nullptr);
        if (result != HRESULT_FROM_WIN32(ERROR_NOT_FOUND)) break;
    }
    std::wprintf(L"XAML taskbar bridge: HRESULT=0x%08lX\n", static_cast<unsigned long>(result));
    FreeLibrary(xaml);
    return SUCCEEDED(result) ? 0 : 4;
}
