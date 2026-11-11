// This file is part of the Diffractor photo and video organizer.
// Copyright 2026 Zac Walker
// SPDX-License-Identifier: LGPL-2.1-or-later
// Purpose: Linux C ABI exposing the existing libfiles image scanner to native frontends.

#include "platform_linux_bridge.h"

#include "libfiles/decode.h"
#include "libfiles/properties.h"
#include "libfiles/raw.h"

#include <algorithm>
#include <fstream>
#include <limits>
#include <memory>
#include <cctype>
#include <vector>

struct diffractor_decoded_image
{
    std::vector<std::uint8_t> pixels;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::uint16_t orientation = 1;
    bool opaque = false;
};

struct diffractor_metadata
{
    libfiles::properties properties;
    std::string keywords;
};

namespace
{
bool is_raw_path(const std::filesystem::path& path)
{
    auto ext = path.extension().string();
    std::ranges::transform(ext, ext.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    constexpr std::string_view extensions[] = {".cr2", ".cr3", ".nef", ".nrw", ".arw", ".dng",
        ".raf", ".orf", ".rw2", ".pef", ".srw", ".3fr", ".iiq", ".erf", ".kdc", ".mos", ".mrw", ".x3f"};
    return std::ranges::find(extensions, ext) != std::end(extensions);
}

class file_stream final : public libfiles::stream
{
    std::ifstream _file;
    std::uint64_t _size = 0;

public:
    explicit file_stream(const char* path) : _file(path, std::ios::binary | std::ios::ate)
    {
        if (_file)
        {
            const auto end = _file.tellg();
            if (end >= 0) _size = static_cast<std::uint64_t>(end);
            else _file.setstate(std::ios::failbit);
        }
    }

    bool valid() const { return _file.is_open() && !_file.fail(); }
    std::uint64_t size() override { return _size; }

    std::size_t read(const std::uint64_t offset, void* data, const std::size_t size) override
    {
        if (offset > _size || offset > static_cast<std::uint64_t>(std::numeric_limits<std::streamoff>::max())) return 0;
        const auto count = std::min<std::uint64_t>(size, _size - offset);
        if (count > static_cast<std::uint64_t>(std::numeric_limits<std::streamsize>::max())) return 0;
        _file.clear();
        _file.seekg(static_cast<std::streamoff>(offset));
        if (!_file) return 0;
        _file.read(static_cast<char*>(data), static_cast<std::streamsize>(count));
        return static_cast<std::size_t>(_file.gcount());
    }
};

class image_sink final : public libfiles::image_sink
{
    static constexpr std::uint64_t max_bytes = 256ull * 1024 * 1024;
    diffractor_decoded_image& _image;

public:
    explicit image_sink(diffractor_decoded_image& image) : _image(image) {}

    bool accept(const libfiles::frame_info& frame) override
    {
        return frame.source_width && frame.source_height &&
               static_cast<std::uint64_t>(frame.source_width) * frame.source_height <= max_bytes / 4 &&
               frame.working_bytes <= max_bytes;
    }

    libfiles::pixel_buffer allocate(const libfiles::frame_info& frame) override
    {
        const auto pixels = static_cast<std::uint64_t>(frame.width) * frame.height;
        if (!frame.width || !frame.height || pixels > max_bytes / 4 || frame.working_bytes > max_bytes ||
            (frame.format != libfiles::pixel_format::bgra8 && frame.format != libfiles::pixel_format::bgrx8)) return {};
        _image.width = frame.width;
        _image.height = frame.height;
        _image.orientation = frame.orientation;
        _image.opaque = frame.format == libfiles::pixel_format::bgrx8;
        _image.pixels.resize(static_cast<std::size_t>(pixels * 4));
        return {_image.pixels.data(), static_cast<std::size_t>(frame.width) * 4};
    }
};
}

extern "C" int diffractor_probe_image(const char* path, diffractor_image_info* out)
{
    if (!path || !*path || !out) return 1;
    *out = {};

    try
    {
        file_stream file(path);
        if (!file.valid()) return 2;
        libfiles::reader reader(file);
        const auto result = libfiles::scan(reader);
        if (!result.success) return 3;
        out->width = result.width;
        out->height = result.height;
        out->orientation = result.orientation.value_or(1);
        out->container_transform = result.container_transform ? 1 : 0;
        return 0;
    }
    catch (const std::exception&)
    {
        return 2;
    }
    catch (...)
    {
        return 2;
    }
}

extern "C" diffractor_decoded_image* diffractor_decode_image(const char* path, const std::uint32_t target_width,
                                                              const std::uint32_t target_height)
{
    if (!path || !*path) return nullptr;
    try
    {
        file_stream file(path);
        if (!file.valid()) return nullptr;
        libfiles::reader reader(file);
        auto image = std::make_unique<diffractor_decoded_image>();
        image_sink sink(*image);
        libfiles::decode_options options;
        options.target_width = target_width;
        options.target_height = target_height;
        const auto result = libfiles::decode(reader, sink, options);
        if (!result.success)
        {
            const auto raw = libfiles::load_raw(std::filesystem::path(path), sink, true);
            if (!raw.success) return nullptr;
            if (!raw.jpeg.empty())
            {
                libfiles::reader preview{std::span<const std::uint8_t>(raw.jpeg)};
                if (!libfiles::decode(preview, sink, options).success) return nullptr;
            }
            image->orientation = raw.orientation;
        }
        if (image->opaque)
            for (std::size_t i = 3; i < image->pixels.size(); i += 4) image->pixels[i] = 255;
        return image.release();
    }
    catch (...)
    {
        return nullptr;
    }
}

extern "C" const std::uint8_t* diffractor_image_pixels(const diffractor_decoded_image* image)
{
    return image ? image->pixels.data() : nullptr;
}

extern "C" std::uint32_t diffractor_image_width(const diffractor_decoded_image* image) { return image ? image->width : 0; }
extern "C" std::uint32_t diffractor_image_height(const diffractor_decoded_image* image) { return image ? image->height : 0; }
extern "C" std::uint16_t diffractor_image_orientation(const diffractor_decoded_image* image) { return image ? image->orientation : 1; }
extern "C" void diffractor_image_free(diffractor_decoded_image* image) { delete image; }

extern "C" diffractor_metadata* diffractor_metadata_read(const char* path)
{
    if (!path || !*path) return nullptr;
    try
    {
        file_stream file(path);
        if (!file.valid()) return nullptr;
        libfiles::reader reader(file);
        auto result = std::make_unique<diffractor_metadata>();
        result->properties = libfiles::read_properties(reader);
        const auto native_path = std::filesystem::path(path);
        if (is_raw_path(native_path))
        {
            // Use the same LibRaw scanner and XMP reader as the application.
            const auto scanned = libfiles::scan_raw(native_path, libfiles::scan_intent::index, nullptr);
            if (!scanned.success) return nullptr;
            auto stem_sidecar = native_path;
            stem_sidecar.replace_extension(".xmp");
            auto full_sidecar = native_path;
            full_sidecar += ".xmp";
            std::error_code error;
            const bool has_stem = std::filesystem::exists(stem_sidecar, error);
            if (error) return nullptr;
            const bool has_full = std::filesystem::exists(full_sidecar, error);
            if (error) return nullptr;
            std::string packet(scanned.xmp.begin(), scanned.xmp.end());
            if (has_stem || has_full)
            {
                const auto sidecar = has_stem && has_full
                    ? std::max(stem_sidecar, full_sidecar)
                    : (has_stem ? stem_sidecar : full_sidecar);
                std::ifstream input(sidecar, std::ios::binary | std::ios::ate);
                if (!input) return nullptr;
                const auto length = input.tellg();
                if (length < 0 || length > 16 * 1024 * 1024) return nullptr;
                packet.resize(static_cast<std::size_t>(length));
                input.seekg(0);
                if (!input.read(packet.data(), length)) return nullptr;
            }
            libfiles::xmp::meta xmp;
            xmp.parse(packet);
            auto properties = libfiles::properties_from_xmp(xmp);
            if (properties.camera_make.empty()) properties.camera_make = scanned.camera_make;
            if (properties.camera_model.empty()) properties.camera_model = scanned.camera_model;
            result->properties = std::move(properties);
        }
        for (const auto& keyword : result->properties.keywords)
        {
            if (!result->keywords.empty()) result->keywords += ", ";
            result->keywords += keyword;
        }
        return result.release();
    }
    catch (...)
    {
        return nullptr;
    }
}

extern "C" const char* diffractor_metadata_text(const diffractor_metadata* metadata, const int field)
{
    if (!metadata) return nullptr;
    switch (field)
    {
    case 0: return metadata->properties.title.c_str();
    case 1: return metadata->properties.camera_make.c_str();
    case 2: return metadata->properties.camera_model.c_str();
    case 3: return metadata->properties.description.c_str();
    case 4: return metadata->keywords.c_str();
    case 5: return metadata->properties.lens.c_str();
    default: return nullptr;
    }
}

extern "C" int diffractor_metadata_rating(const diffractor_metadata* metadata, int* out)
{
    if (!metadata || !out || !metadata->properties.rating) return 0;
    *out = *metadata->properties.rating;
    return 1;
}

extern "C" void diffractor_metadata_free(diffractor_metadata* metadata) { delete metadata; }
