// This file is part of the Diffractor photo and video organizer.
// Copyright 2026 Zac Walker
// SPDX-License-Identifier: LGPL-2.1-or-later
// Purpose: Stable C boundary for the Linux native frontend's media workers.

#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct diffractor_image_info
{
    uint32_t width;
    uint32_t height;
    uint16_t orientation;
    uint8_t container_transform;
} diffractor_image_info;

// 0: success; 1: invalid argument; 2: file cannot be read; 3: unsupported or malformed image.
// The caller owns `out`. This function does I/O and must run on a worker thread.
int diffractor_probe_image(const char* path, diffractor_image_info* out);

typedef struct diffractor_decoded_image diffractor_decoded_image;

// Decodes to a bounded BGRA buffer. The returned object belongs to the caller and must be freed.
// The pointer is NULL when the image cannot be decoded or exceeds the memory budget.
diffractor_decoded_image* diffractor_decode_image(const char* path, uint32_t target_width, uint32_t target_height);
const uint8_t* diffractor_image_pixels(const diffractor_decoded_image* image);
uint32_t diffractor_image_width(const diffractor_decoded_image* image);
uint32_t diffractor_image_height(const diffractor_decoded_image* image);
uint16_t diffractor_image_orientation(const diffractor_decoded_image* image);
void diffractor_image_free(diffractor_decoded_image* image);

typedef struct diffractor_metadata diffractor_metadata;

// Read embedded EXIF, IPTC and XMP on a worker. Strings are UTF-8 and borrowed until free.
// Field IDs: 0 title, 1 camera make, 2 camera model, 3 description, 4 keywords, 5 lens.
diffractor_metadata* diffractor_metadata_read(const char* path);
const char* diffractor_metadata_text(const diffractor_metadata* metadata, int field);
// Returns 1 when a rating is present and writes it to out (-1 rejected, 0 unrated, 1..5 stars).
int diffractor_metadata_rating(const diffractor_metadata* metadata, int* out);
void diffractor_metadata_free(diffractor_metadata* metadata);

#ifdef __cplusplus
}
#endif
