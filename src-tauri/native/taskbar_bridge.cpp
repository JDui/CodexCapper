#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include "reservation_protocol.h"
#include <xamlom.h>
#include <windows.ui.xaml.hosting.desktopwindowxamlsource.h>
#ifdef GetCurrentTime
#undef GetCurrentTime
#endif
#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.UI.Xaml.h>
#include <winrt/Windows.UI.Xaml.Controls.Primitives.h>
#include <winrt/Windows.UI.Xaml.Hosting.h>
#include <winrt/Windows.UI.Xaml.Media.h>
#include <algorithm>
#include <atomic>
#include <cmath>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include <cstdarg>
#include <string_view>

namespace wux = winrt::Windows::UI::Xaml;
namespace wuxh = winrt::Windows::UI::Xaml::Hosting;
namespace wf = winrt::Windows::Foundation;

namespace {
HINSTANCE g_module;
HANDLE g_leader = nullptr;
HANDLE g_mapping = nullptr;
capper::Reservation* g_shared = nullptr;
std::mutex g_mappingMutex;
std::atomic<uint32_t> g_matchingGeneration{0};
constexpr wchar_t kTimerClass[] = L"CodexCapper.XamlReservation.Timer.v2";
std::mutex g_logMutex;
std::atomic<unsigned> g_treeLogCount{0};

void Debug(const wchar_t* text) noexcept {
    OutputDebugStringW(L"CodexCapper taskbar bridge: ");
    OutputDebugStringW(text);
    OutputDebugStringW(L"\n");
    try {
        std::lock_guard lock(g_logMutex);
        wchar_t data[MAX_PATH]{};
        const auto length = GetEnvironmentVariableW(L"APPDATA", data, MAX_PATH);
        if (!length || length >= MAX_PATH) return;
        auto folder = std::wstring(data) + L"\\CodexCapper\\logs";
        CreateDirectoryW((std::wstring(data) + L"\\CodexCapper").c_str(), nullptr);
        CreateDirectoryW(folder.c_str(), nullptr);
        auto path = folder + L"\\taskbar-bridge-v2.log";
        HANDLE file = CreateFileW(path.c_str(), FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (file == INVALID_HANDLE_VALUE) return;
        auto entry = std::to_wstring(GetTickCount64()) + L" pid=" + std::to_wstring(GetCurrentProcessId())
            + L" tid=" + std::to_wstring(GetCurrentThreadId()) + L" " + text + L"\r\n";
        const auto bytes = WideCharToMultiByte(CP_UTF8, 0, entry.c_str(), static_cast<int>(entry.size()), nullptr, 0, nullptr, nullptr);
        std::string utf8(bytes, '\0');
        WideCharToMultiByte(CP_UTF8, 0, entry.c_str(), static_cast<int>(entry.size()), utf8.data(), bytes, nullptr, nullptr);
        DWORD written = 0;
        WriteFile(file, utf8.data(), static_cast<DWORD>(utf8.size()), &written, nullptr);
        CloseHandle(file);
    } catch (...) {}
}

void Logf(const wchar_t* format, ...) noexcept {
    wchar_t buffer[1600]{};
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(buffer, _countof(buffer), _TRUNCATE, format, args);
    va_end(args);
    Debug(buffer);
}

bool OpenMapping() noexcept {
    std::lock_guard lock(g_mappingMutex);
    if (g_shared) return true;
    g_mapping = OpenFileMappingW(FILE_MAP_ALL_ACCESS, FALSE, capper::kMappingName);
    if (!g_mapping) return false;
    g_shared = static_cast<capper::Reservation*>(MapViewOfFile(g_mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(capper::Reservation)));
    if (!g_shared) { CloseHandle(g_mapping); g_mapping = nullptr; }
    return g_shared != nullptr;
}

bool ReadRequest(capper::Reservation& request) noexcept {
    if (!OpenMapping()) return false;
    for (int i = 0; i < 3; ++i) {
        const auto enabled = static_cast<uint32_t>(InterlockedCompareExchange(
            reinterpret_cast<volatile LONG*>(&g_shared->enabled), 0, 0));
        const auto generation = static_cast<uint32_t>(InterlockedCompareExchange(
            reinterpret_cast<volatile LONG*>(&g_shared->generation), 0, 0));
        MemoryBarrier();
        request = *g_shared;
        MemoryBarrier();
        const auto finalEnabled = static_cast<uint32_t>(InterlockedCompareExchange(
            reinterpret_cast<volatile LONG*>(&g_shared->enabled), 0, 0));
        if (request.generation == generation && g_shared->generation == generation
            && request.enabled == enabled && finalEnabled == enabled)
            return request.magic == capper::kMagic && request.version == 1;
    }
    return false;
}

void Publish(const capper::Reservation& request, int32_t status, double before = 0, double after = 0, const RECT& slot = {}) noexcept {
    if (!g_shared || g_shared->generation != request.generation || g_shared->owner_pid != request.owner_pid) return;
    g_shared->before_width = static_cast<float>(before);
    g_shared->after_width = static_cast<float>(after);
    g_shared->slot_left = slot.left;
    g_shared->slot_top = slot.top;
    g_shared->slot_right = slot.right;
    g_shared->slot_bottom = slot.bottom;
    InterlockedExchange(reinterpret_cast<volatile LONG*>(&g_shared->status), status);
    MemoryBarrier();
    InterlockedExchange(reinterpret_cast<volatile LONG*>(&g_shared->applied_generation), request.generation);
}

bool OwnerAlive(const capper::Reservation& request) noexcept {
    const auto now = GetTickCount64();
    if (!request.enabled || !request.owner_pid || request.width_px == 0 || request.width_px > 1600
        || now < request.heartbeat_ms || now - request.heartbeat_ms > 8000) return false;
    HANDLE owner = OpenProcess(SYNCHRONIZE, FALSE, request.owner_pid);
    if (!owner) return false;
    const bool alive = WaitForSingleObject(owner, 0) == WAIT_TIMEOUT;
    CloseHandle(owner);
    return alive;
}

bool SameMargin(const wux::Thickness& a, const wux::Thickness& b) noexcept {
    return std::abs(a.Left-b.Left)<0.1 && std::abs(a.Top-b.Top)<0.1
        && std::abs(a.Right-b.Right)<0.1 && std::abs(a.Bottom-b.Bottom)<0.1;
}

HWND TaskbarAncestor(HWND window) noexcept {
    for (unsigned i = 0; window && i < 16; ++i, window = GetParent(window)) {
        wchar_t name[128]{};
        GetClassNameW(window, name, 128);
        if (std::wcscmp(name, L"Shell_TrayWnd") == 0 || std::wcscmp(name, L"Shell_SecondaryTrayWnd") == 0) return window;
    }
    return nullptr;
}

struct Source {
    InstanceHandle handle;
    winrt::weak_ref<wuxh::DesktopWindowXamlSource> source;
};
struct Candidate {
    InstanceHandle handle;
    winrt::weak_ref<wux::FrameworkElement> element;
    winrt::weak_ref<wux::FrameworkElement> root;
    HWND taskbar = nullptr;
    wux::Thickness original{};
    wux::Thickness applied{};
    bool ownsMargin = false;
    uint32_t owner = 0;
    uint32_t generation = 0;
    double beforeWidth = 0;
    unsigned settleTicks = 0;

    void Restore() noexcept {
        try {
            if (ownsMargin) {
                if (auto target = element.get()) {
                    // A different taskbar customizer may have replaced the
                    // property. Never overwrite its new value on shutdown.
                    if (SameMargin(target.Margin(), applied)) {
                        target.Margin(original);
                        if (auto frame = root.get()) frame.UpdateLayout();
                    }
                }
            }
        } catch (...) { Debug(L"could not restore a removed taskbar element"); }
        ownsMargin = false;
        owner = generation = 0;
        settleTicks = 0;
    }
};

struct UiContext {
    winrt::com_ptr<IXamlDiagnostics> diagnostics;
    HWND timer = nullptr;
    std::vector<Source> sources;
    std::vector<Candidate> candidates;
    explicit UiContext(winrt::com_ptr<IXamlDiagnostics> value) : diagnostics(std::move(value)) {}
    template<typename T> T FromHandle(InstanceHandle handle) {
        winrt::com_ptr<::IInspectable> value;
        winrt::check_hresult(diagnostics->GetIInspectableFromHandle(handle, value.put()));
        return value.try_as<T>();
    }
    void Resolve(Candidate& candidate) {
        if (candidate.taskbar && IsWindow(candidate.taskbar)) return;
        const auto element = candidate.element.get();
        if (!element) return;
        auto root = element;
        for (unsigned i = 0; i < 100; ++i) {
            auto parent = wux::Media::VisualTreeHelper::GetParent(root).try_as<wux::FrameworkElement>();
            if (!parent) break;
            root = parent;
        }
        for (auto& item : sources) {
            auto source = item.source.get();
            if (!source) continue;
            if (source.Content() == root) {
                HWND bridge = nullptr;
                auto native = source.as<IDesktopWindowXamlSourceNative>();
                if (SUCCEEDED(native->get_WindowHandle(&bridge))) {
                    candidate.taskbar = TaskbarAncestor(bridge);
                    candidate.root = winrt::make_weak(root);
                    Logf(L"Resolve source matched handle=%llu bridge=%p taskbar=%p sources=%zu", candidate.handle, bridge, candidate.taskbar, sources.size());
                    return;
                }
            }
        }
        // Some taskbar diagnostic trees omit DesktopWindowXamlSource. Permit
        // only one same-thread taskbar whose client size exactly matches this
        // XAML root, so two similarly sized screens cannot be confused.
        struct Match { HWND found = nullptr; unsigned count = 0; double width = 0; double height = 0; DWORD thread = 0; } match;
        const auto scale = root.XamlRoot() ? root.XamlRoot().RasterizationScale() : 1.0;
        match.width = root.ActualWidth() * scale;
        match.height = root.ActualHeight() * scale;
        match.thread = GetCurrentThreadId();
        EnumThreadWindows(match.thread, [](HWND window, LPARAM value) -> BOOL {
            auto& m = *reinterpret_cast<Match*>(value);
            if (TaskbarAncestor(window) != window) return TRUE;
            RECT client{};
            if (GetClientRect(window, &client) && std::abs((client.right-client.left)-m.width)<=2.0
                && std::abs((client.bottom-client.top)-m.height)<=2.0) { m.found = window; ++m.count; }
            return TRUE;
        }, reinterpret_cast<LPARAM>(&match));
        if (match.count == 1) {
            candidate.taskbar = match.found;
            candidate.root = winrt::make_weak(root);
            Logf(L"Resolve unique same-thread size match handle=%llu taskbar=%p root=%.1fx%.1f", candidate.handle, candidate.taskbar, match.width, match.height);
        } else {
            static thread_local unsigned unresolvedLogs = 0;
            if (++unresolvedLogs <= 8) Logf(L"Resolve unresolved handle=%llu sources=%zu root=%.1fx%.1f same-thread matches=%u", candidate.handle, sources.size(), match.width, match.height, match.count);
        }
    }
    void Tick() noexcept {
        capper::Reservation request{};
        const bool readable = ReadRequest(request);
        const bool active = readable && OwnerAlive(request);
        bool matched = false;
        for (auto& candidate : candidates) {
            try {
                auto target = candidate.element.get();
                if (!target) { candidate.Restore(); continue; }
                Resolve(candidate);
                if (!active || reinterpret_cast<uint64_t>(candidate.taskbar) != request.taskbar_hwnd) {
                    const bool restoredRequested = readable && !active && candidate.ownsMargin
                        && reinterpret_cast<uint64_t>(candidate.taskbar) == request.taskbar_hwnd;
                    candidate.Restore();
                    if (restoredRequested) Publish(request, -3);
                    continue;
                }
                matched = true;
                g_matchingGeneration.store(request.generation);
                auto frame = candidate.root.get();
                if (!frame || !candidate.taskbar || !IsWindow(candidate.taskbar)) {
                    candidate.Restore(); Publish(request, -1); continue;
                }
                DWORD process = 0;
                GetWindowThreadProcessId(candidate.taskbar, &process);
                if (process != GetCurrentProcessId()) { candidate.Restore(); Publish(request, -2); continue; }
                const double scale = frame.XamlRoot() ? frame.XamlRoot().RasterizationScale() : GetDpiForWindow(candidate.taskbar)/96.0;
                if (!(scale > 0.0 && scale < 10.0)) { candidate.Restore(); Publish(request, -2); continue; }
                if (!candidate.ownsMargin || candidate.owner != request.owner_pid || candidate.generation != request.generation
                    || !SameMargin(target.Margin(), candidate.applied)) {
                    candidate.Restore();
                    candidate.original = target.Margin();
                    auto slot = wux::Controls::Primitives::LayoutInformation::GetLayoutSlot(target);
                    // Effective width inside the actual XAML layout slot. This
                    // is capacity reserved from the app-button container,
                    // including when the current buttons do not fill it yet.
                    candidate.beforeWidth = std::max(0.0, static_cast<double>(slot.Width) - candidate.original.Left - candidate.original.Right);
                    if (candidate.beforeWidth <= request.width_px / scale + 32.0) { Publish(request, -2); continue; }
                    candidate.applied = candidate.original;
                    if (request.side == 1) candidate.applied.Left += request.width_px / scale;
                    else candidate.applied.Right += request.width_px / scale;
                    candidate.owner = request.owner_pid;
                    candidate.generation = request.generation;
                    candidate.ownsMargin = true;
                    target.Margin(candidate.applied);
                    frame.UpdateLayout();
                    candidate.settleTicks = 0;
                    Publish(request, 0);
                }
                auto slot = wux::Controls::Primitives::LayoutInformation::GetLayoutSlot(target);
                const auto actualMargin = target.Margin();
                const double after = std::max(0.0, static_cast<double>(slot.Width) - actualMargin.Left - actualMargin.Right);
                const auto reduction = (candidate.beforeWidth - after) * scale;
                if (SameMargin(actualMargin, candidate.applied) && reduction >= request.width_px - 2.0
                    && after >= 16.0 && ++candidate.settleTicks >= 2) {
                    RECT bar{};
                    GetWindowRect(candidate.taskbar, &bar);
                    RECT nativeSlot = bar;
                    HWND tray = FindWindowExW(candidate.taskbar, nullptr, L"TrayNotifyWnd", nullptr);
                    if (!tray) tray = FindWindowExW(candidate.taskbar, nullptr, L"ClockButton", nullptr);
                    RECT trayRect{};
                    const bool knownTray = tray && GetWindowRect(tray, &trayRect);
                    if (knownTray) nativeSlot.right = trayRect.left;
                    if (request.side == 1) {
                        nativeSlot.right = nativeSlot.left + request.width_px;
                    } else if (knownTray) nativeSlot.left = nativeSlot.right - request.width_px;
                    else nativeSlot = {}; // the caller must use its measured tray boundary
                    Publish(request, 1, candidate.beforeWidth * scale, after * scale, nativeSlot);
                } else if (candidate.settleTicks++ > 8) {
                    candidate.Restore(); Publish(request, -2);
                }
            } catch (...) { candidate.Restore(); if (active) Publish(request, -2); }
        }
        if (readable && !active) {
            // Only one context writes the inactive acknowledgement: the one
            // on the requested taskbar thread. All contexts still restore.
            if (GetWindowThreadProcessId(reinterpret_cast<HWND>(request.taskbar_hwnd), nullptr) == GetCurrentThreadId()) Publish(request, -3);
        }
        (void)matched;
    }
};

LRESULT CALLBACK TimerProc(HWND window, UINT message, WPARAM w, LPARAM l) noexcept {
    auto context = reinterpret_cast<UiContext*>(GetWindowLongPtrW(window, GWLP_USERDATA));
    if (message == WM_NCCREATE) {
        context = static_cast<UiContext*>(reinterpret_cast<CREATESTRUCTW*>(l)->lpCreateParams);
        SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(context));
    }
    if (message == WM_TIMER && context) { context->Tick(); return 0; }
    if (message == WM_NCDESTROY && context) {
        for (auto& candidate : context->candidates) candidate.Restore();
        context->timer = nullptr;
    }
    return DefWindowProcW(window, message, w, l);
}

std::once_flag g_timerClass;
thread_local UiContext* g_ui = nullptr;
UiContext& CurrentUi(const winrt::com_ptr<IXamlDiagnostics>& diagnostics) {
    if (!g_ui) {
        // UI contexts and their weak references are process-lifetime objects.
        // XAML diagnostics pins this DLL; never free a callback's code while
        // Explorer can still call it.
        g_ui = new UiContext(diagnostics);
        std::call_once(g_timerClass, [] {
            WNDCLASSW cls{};
            cls.hInstance = g_module;
            cls.lpszClassName = kTimerClass;
            cls.lpfnWndProc = TimerProc;
            RegisterClassW(&cls);
        });
        g_ui->timer = CreateWindowExW(0, kTimerClass, L"", 0, 0, 0, 0, 0, HWND_MESSAGE, nullptr, g_module, g_ui);
        if (g_ui->timer) SetTimer(g_ui->timer, 1, 250, nullptr);
    }
    return *g_ui;
}

struct Strings {
    VisualElement value;
    ~Strings() {
        SysFreeString(value.Name);
        SysFreeString(value.Type);
        SysFreeString(value.SrcInfo.FileName);
        SysFreeString(value.SrcInfo.Hash);
    }
};

struct Watcher : winrt::implements<Watcher, IVisualTreeServiceCallback2, winrt::non_agile> {
    winrt::com_ptr<IXamlDiagnostics> diagnostics;
    explicit Watcher(winrt::com_ptr<IXamlDiagnostics> value) : diagnostics(std::move(value)) {}
    HRESULT STDMETHODCALLTYPE OnVisualTreeChange(ParentChildRelation, VisualElement element, VisualMutationType mutation) noexcept final {
        try {
            auto& context = CurrentUi(diagnostics);
            if (mutation == Remove) {
                for (auto& candidate : context.candidates) if (candidate.handle == element.Handle) candidate.Restore();
                std::erase_if(context.candidates, [&](auto& c) { return c.handle == element.Handle; });
                std::erase_if(context.sources, [&](auto& s) { return s.handle == element.Handle; });
                return S_OK;
            }
            Strings owned{element};
            const std::wstring_view metaName(element.Name ? element.Name : L"", element.Name ? SysStringLen(element.Name) : 0);
            const std::wstring_view metaType(element.Type ? element.Type : L"", element.Type ? SysStringLen(element.Type) : 0);
            if (g_treeLogCount.fetch_add(1) < 35 || metaName.find(L"Repeater") != std::wstring_view::npos
                || metaName.find(L"TaskbarFrame") != std::wstring_view::npos || metaType.find(L"DesktopWindowXamlSource") != std::wstring_view::npos) {
                Logf(L"Tree Add handle=%llu type=%.*s name=%.*s", element.Handle,
                    static_cast<int>(metaType.size()), metaType.data(), static_cast<int>(metaName.size()), metaName.data());
            }
            auto inspectable = context.FromHandle<wf::IInspectable>(element.Handle);
            if (auto source = inspectable.try_as<wuxh::DesktopWindowXamlSource>()) {
                if (std::none_of(context.sources.begin(), context.sources.end(), [&](auto& s){ return s.handle == element.Handle; }))
                    context.sources.push_back({element.Handle, winrt::make_weak(source)});
                Logf(L"Source captured handle=%llu sources=%zu", element.Handle, context.sources.size());
            } else if (auto framework = inspectable.try_as<wux::FrameworkElement>()) {
                const auto name = framework.Name();
                if (name == L"TaskbarFrameRepeater" || winrt::get_class_name(framework) == L"Taskbar.TaskbarFrameRepeater") {
                    if (std::any_of(context.candidates.begin(), context.candidates.end(), [&](auto& c){ return c.handle == element.Handle; })) return S_OK;
                    Candidate candidate{};
                    candidate.handle = element.Handle;
                    candidate.element = winrt::make_weak(framework);
                    context.candidates.push_back(std::move(candidate));
                    Logf(L"Repeater captured handle=%llu candidates=%zu", element.Handle, context.candidates.size());
                    context.Tick();
                }
            }
        } catch (const winrt::hresult_error& error) { Logf(L"Tree exception HRESULT=0x%08X message=%s", static_cast<unsigned>(error.code().value), error.message().c_str()); }
        catch (...) { Debug(L"visual-tree element was unavailable: unknown exception"); }
        // An error here can stop diagnostics from sending later events.
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE OnElementStateChanged(InstanceHandle, VisualElementState, LPCWSTR) noexcept final { return S_OK; }
};

std::mutex g_watcherMutex;
winrt::com_ptr<Watcher> g_watcher;

struct Tap : winrt::implements<Tap, IObjectWithSite> {
    winrt::com_ptr<IUnknown> site;
    HRESULT STDMETHODCALLTYPE SetSite(IUnknown* value) noexcept final {
        try {
            site.copy_from(value);
            if (!value) return S_OK;
            std::lock_guard lock(g_watcherMutex);
            if (g_watcher) return S_OK;
            if (!g_leader) {
                const auto mutexName = L"Local\\CodexCapper.TaskbarBridge.Owner.v2." + std::to_wstring(GetCurrentProcessId());
                SetLastError(ERROR_SUCCESS);
                g_leader = CreateMutexW(nullptr, FALSE, mutexName.c_str());
                const auto error = GetLastError();
                if (!g_leader) return HRESULT_FROM_WIN32(error);
                if (error == ERROR_ALREADY_EXISTS) {
                    CloseHandle(g_leader); g_leader = nullptr;
                    return S_OK;
                }
            }
            g_watcher = winrt::make_self<Watcher>(site.as<IXamlDiagnostics>());
            std::thread([watcher = g_watcher] {
                const auto initialized = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
                Logf(L"Advise worker CoInitializeEx HRESULT=0x%08X", static_cast<unsigned>(initialized));
                bool subscribed = false;
                uint32_t observed = 0;
                uint64_t since = 0;
                for (;;) {
                    if (!subscribed) {
                        try {
                            // This API marshals its non-agile callback to each
                            // XAML UI thread. Never call it on SetSite's thread.
                            const auto result = watcher->diagnostics.as<IVisualTreeService3>()->AdviseVisualTreeChange(watcher.get());
                            Logf(L"AdviseVisualTreeChange HRESULT=0x%08X", static_cast<unsigned>(result));
                            winrt::check_hresult(result);
                            subscribed = true;
                        } catch (const winrt::hresult_error& error) {
                            Logf(L"Advise exception HRESULT=0x%08X message=%s; retrying", static_cast<unsigned>(error.code().value), error.message().c_str());
                        } catch (...) { Debug(L"Advise unknown exception; retrying"); }
                        if (!subscribed) { Sleep(2000); continue; }
                    }
                    Sleep(500);
                    capper::Reservation request{};
                    if (!ReadRequest(request) || !OwnerAlive(request)) continue;
                    if (observed != request.generation) { observed = request.generation; since = GetTickCount64(); }
                    if (GetTickCount64() - since > 3500 && g_matchingGeneration.load() != request.generation)
                        Publish(request, -1);
                }
            }).detach();
            return S_OK;
        } catch (...) { return winrt::to_hresult(); }
    }
    HRESULT STDMETHODCALLTYPE GetSite(REFIID id, void** value) noexcept final {
        if (!value) return E_POINTER;
        *value = nullptr;
        return site ? site->QueryInterface(id, value) : E_FAIL;
    }
};

struct Factory : winrt::implements<Factory, IClassFactory> {
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer, REFIID id, void** value) noexcept final {
        if (!value) return E_POINTER;
        *value = nullptr;
        if (outer) return CLASS_E_NOAGGREGATION;
        try { return winrt::make_self<Tap>()->QueryInterface(id, value); }
        catch (...) { return winrt::to_hresult(); }
    }
    HRESULT STDMETHODCALLTYPE LockServer(BOOL) noexcept final { return S_OK; }
};
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID iid, void** value) {
    if (!value) return E_POINTER;
    *value = nullptr;
    if (clsid != capper::kTapClsid) return CLASS_E_CLASSNOTAVAILABLE;
    try { return winrt::make_self<Factory>()->QueryInterface(iid, value); }
    catch (...) { return winrt::to_hresult(); }
}
STDAPI DllCanUnloadNow() { return S_FALSE; }
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, void*) {
    if (reason == DLL_PROCESS_ATTACH) { g_module = instance; DisableThreadLibraryCalls(instance); }
    return TRUE;
}
