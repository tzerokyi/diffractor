// This file is part of the Diffractor photo and video organizer.
// Copyright 2026 Zac Walker
// SPDX-License-Identifier: LGPL-2.1-or-later
// Purpose: Isolated Linux video frame extraction through Diffractor's existing media decoder.

#include "pch.h"
#include "av_format.h"

namespace
{
bool write_u32(const uint32_t value)
{
    const uint8_t bytes[] = {static_cast<uint8_t>(value), static_cast<uint8_t>(value >> 8),
                             static_cast<uint8_t>(value >> 16), static_cast<uint8_t>(value >> 24)};
    return std::fwrite(bytes, 1, sizeof(bytes), stdout) == sizeof(bytes);
}
}

int run_linux_video_frame(const char* path)
{
    try
    {
        const df::file_path file(path);
        if (!file.is_valid() || !file.exists())
        {
            std::fprintf(stderr, "Video file is unavailable.\n");
            return 2;
        }
        av_format_decoder decoder;
        if (!decoder.open(file, media_intent::thumbnail))
        {
            std::fprintf(stderr, "Cannot open video.\n");
            return 3;
        }
        decoder.init_streams(-1, -1, false, true, false);
        ui::surface_ptr image;
        if (!decoder.has_video() || !decoder.extract_thumbnail(image, {1280, 720}, 10, 100, false) ||
            !ui::is_valid(image) || (image->format() != ui::texture_format::RGB && image->format() != ui::texture_format::ARGB))
        {
            std::fprintf(stderr, "Cannot decode a video frame.\n");
            return 3;
        }
        const auto width = image->width();
        const auto height = image->height();
        if (width == 0 || height == 0 || width > 1280 || height > 1280 ||
            static_cast<uint64_t>(width) * height > 1280ull * 1280ull)
        {
            std::fprintf(stderr, "Video frame exceeds the preview limit.\n");
            return 3;
        }
        constexpr char header[] = "DIFVID1";
        if (std::fwrite(header, 1, sizeof(header), stdout) != sizeof(header) || !write_u32(width) ||
            !write_u32(height) || !write_u32(static_cast<uint32_t>(image->orientation()))) return 4;
        for (uint32_t y = 0; y < height; ++y)
        {
            const auto bytes = static_cast<size_t>(width) * 4;
            if (std::fwrite(image->pixels_line(static_cast<int>(y)), 1, bytes, stdout) != bytes) return 4;
        }
        return std::fflush(stdout) == 0 ? 0 : 4;
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "Video preview failed: %s\n", e.what());
        return 3;
    }
}
