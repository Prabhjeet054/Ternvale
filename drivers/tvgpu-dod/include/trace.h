/* WPP tracing for tvgpu-dod. Control GUID:
 * {A9C3E271-4B18-4D6F-8E01-2C5A7B9D0E11}
 */
#pragma once

#define WPP_CONTROL_GUIDS \
    WPP_DEFINE_CONTROL_GUID( \
        TvgpuDodTraceGuid, \
        (A9C3E271, 4B18, 4D6F, 8E01, 2C5A7B9D0E11), \
        WPP_DEFINE_BIT(MYDRIVER_ALL_INFO) \
        WPP_DEFINE_BIT(TRACE_DRIVER) \
        WPP_DEFINE_BIT(TRACE_DEVICE) \
        WPP_DEFINE_BIT(TRACE_VIDPN) \
        WPP_DEFINE_BIT(TRACE_PRESENT) \
        WPP_DEFINE_BIT(TRACE_VIRTIO))

#define WPP_FLAG_LEVEL_LOGGER(flag, level) WPP_LEVEL_LOGGER(flag)
#define WPP_FLAG_LEVEL_ENABLED(flag, level) \
    (WPP_LEVEL_ENABLED(flag) && WPP_CONTROL(WPP_BIT_##flag).Level >= level)

#define WPP_LEVEL_FLAGS_LOGGER(lvl, flags) WPP_LEVEL_LOGGER(flags)
#define WPP_LEVEL_FLAGS_ENABLED(lvl, flags) \
    (WPP_LEVEL_ENABLED(flags) && WPP_CONTROL(WPP_BIT_##flags).Level >= lvl)

// begin_wpp config
// FUNC TraceEvents(LEVEL, FLAGS, MSG, ...);
// end_wpp
