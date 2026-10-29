#pragma once
#include <windows.h>
#include <cstdint>
#include <cstddef>

namespace capper {
inline constexpr wchar_t kMappingName[] = L"Local\\CodexCapper.TaskbarReservation.v2";
inline constexpr uint32_t kMagic = 0x43435052;
inline constexpr CLSID kTapClsid{0x44726614, 0x45ac, 0x4cc0, {0x9a, 0x44, 0xb4, 0x33, 0x1b, 0xb7, 0x6f, 0x52}};

struct alignas(8) Reservation {
    uint32_t magic;
    uint32_t version;
    uint32_t owner_pid;
    uint32_t enabled;
    uint64_t taskbar_hwnd;
    uint32_t width_px;
    uint32_t generation;
    uint64_t heartbeat_ms;
    uint32_t applied_generation;
    int32_t status;
    float before_width;
    float after_width;
    int32_t slot_left;
    int32_t slot_top;
    int32_t slot_right;
    int32_t slot_bottom;
    uint32_t side;
    uint32_t reserved;
};
static_assert(sizeof(Reservation) == 80);
static_assert(offsetof(Reservation, heartbeat_ms) == 32);
static_assert(offsetof(Reservation, applied_generation) == 40);
static_assert(offsetof(Reservation, side) == 72);
}
