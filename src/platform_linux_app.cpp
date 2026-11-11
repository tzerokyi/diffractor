// This file is part of the Diffractor photo and video organizer
// Copyright 2026  Zac Walker
// 
// This program is free software; you can redistribute it and / or modify it
// under the terms of the LGPL License either version 2.1 or later.
// License details are available at https://www.gnu.org/licenses/lgpl-2.1.html
// This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY

// Purpose: The application's Linux glue: no hardware video decode for FFmpeg to use - the counterpart
// of platform_win_av.cpp - no platform tests to register, since the Windows-only subject file is not
// built here, and the entry point, which runs the application's headless suite.

#include "pch.h"
#include "av_format.h"
#include "av_sound.h"
#include "files.h"
#include "model.h"
#include "test_runner.h"
#include "libdesktop/util_base64.h"

#include <pthread.h>
#include <charconv>
#include <filesystem>

int run_linux_index_query(const char* folder, const char* query);
int run_linux_video_frame(const char* path);
int run_linux_rename(const char* path, const char* name, bool execute, int expected_count, const char* const* expected);

// Use the application's registered format traits as the single authority. AV formats
// that write only an external sidecar are withheld until the native reader can read it.
static bool native_metadata_editable(const file_type_ref type)
{
	return type && type->has_trait(file_traits::edit) &&
		(type->has_trait(file_traits::bitmap) || type->has_trait(file_traits::embedded_xmp));
}

static int print_native_metadata_capabilities()
{
	constexpr char header[] = "DIFCAP1";
	if (std::fwrite(header, 1, sizeof(header), stdout) != sizeof(header)) return 1;
	bool ok = true;
	for (const auto* const type : all_file_types())
	{
		str::split2(type->extension, true, [&](const std::string_view extension)
		{
			if (!ok || !native_metadata_editable(files::file_type_from_name(extension))) return;
			ok = std::fwrite(extension.data(), 1, extension.size(), stdout) == extension.size() &&
				std::fputc(0, stdout) != EOF;
		});
	}
	return ok && std::fflush(stdout) == 0 ? 0 : 1;
}

// A single reviewed metadata edit, invoked by the native frontend on a worker.
// Reuse the same staging, in-place and sidecar handling as the application.
static int set_linux_metadata(const char* path_arg, const int count, const char* const* args)
{
	try
	{
		metadata_edits edits;
		if (count == 0 || count % 2 != 0) throw std::runtime_error("Invalid metadata edit.");
		for (int i = 0; i < count; i += 2)
		{
			const std::string_view key(args[i]);
			const char* value = args[i + 1];
			if (key == "--title" && !edits.title) edits.title = value;
			else if (key == "--description" && !edits.description) edits.description = value;
			else if (key == "--add-tag" && *value) edits.add_tags.add_one(value);
			else if (key == "--remove-tag" && *value) edits.remove_tags.add_one(value);
			else if (key == "--rating" && !edits.rating)
			{
				int rating = 0;
				const auto length = std::char_traits<char>::length(value);
				const auto [end, error] = std::from_chars(value, value + length, rating);
				if (error != std::errc{} || end != value + length || rating < -1 || rating > 5)
					throw std::runtime_error("Rating must be between -1 and 5.");
				edits.rating = rating;
			}
			else throw std::runtime_error("Unknown or duplicate metadata field.");
		}

		const std::filesystem::path native_path(path_arg);
		if (!native_path.is_absolute() || !std::filesystem::is_regular_file(native_path))
			throw std::runtime_error("The selected file is unavailable.");
		const df::file_path path{std::string_view(path_arg)};
		const auto* const type = files::file_type_from_name(path);
		if (!native_metadata_editable(type))
			throw std::runtime_error("This file type does not support metadata editing.");

		auto stem = native_path;
		stem.replace_extension(".xmp");
		auto full = native_path;
		full += ".xmp";
		std::error_code ec;
		const bool stem_exists = std::filesystem::exists(stem, ec);
		if (ec) throw std::runtime_error("Cannot inspect the XMP sidecar.");
		const bool full_exists = std::filesystem::exists(full, ec);
		if (ec) throw std::runtime_error("Cannot inspect the XMP sidecar.");
		if (stem_exists && full_exists)
			throw std::runtime_error("Two XMP sidecars exist; cannot choose which to edit.");
		const auto sidecar = stem_exists ? stem.filename().string()
			: full_exists ? full.filename().string() : std::string{};

		files scanner;
		const auto result = scanner.update(path, edits, image_edits{}, file_encode_params{}, false, sidecar);
		if (result.failed())
		{
			std::fprintf(stderr, "%s\n", result.format_error().c_str());
			return 1;
		}
		return 0;
	}
	catch (const std::exception& error)
	{
		std::fprintf(stderr, "%s\n", error.what());
		return 1;
	}
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Media
///////////////////////////////////////////////////////////////////////////////////////////////////

// No hardware decode until there is a renderer to present its surfaces. VAAPI belongs here, and
// only once the GPU backend can import an NV12 surface - a decoder whose output cannot be shown
// is slower than software, not faster.
av_hw_decode_target av_platform_hw_decode_target()
{
	return {};
}

// Never asked while av_platform_hw_decode_target answers that there is no hardware path.
bool av_platform_hw_decode_supported(AVBufferRef*, AVCodecID, int, int, int)
{
	return false;
}

bool av_platform_hw_device_usable(AVBufferRef*)
{
	return false;
}

// Never asked while av_platform_hw_decode_target answers that there is no hardware path.
AVBufferRef* av_platform_create_hw_device(int)
{
	return nullptr;
}

// No hardware picture is decoded here, so there is no device lock to lend.
void av_platform_decode_lock(const AVHWFramesContext*, ui::video_picture&)
{
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Tests. The Windows-only subject file is not built here, so the registration is empty.
///////////////////////////////////////////////////////////////////////////////////////////////////

void register_platform_tests(view_state&, test_registry&)
{
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Entry point
///////////////////////////////////////////////////////////////////////////////////////////////////

int main(const int argc, const char* const argv[])
{
	platform::set_ui_thread();

	if (!std::setlocale(LC_CTYPE, "C.UTF-8"))
	{
		std::fprintf(stderr, "Cannot initialize the UTF-8 character locale required for filenames.\n");
		return 1;
	}

	if (argc == 4 && std::string_view(argv[1]) == "--index-query")
	{
		load_file_types();
		return run_linux_index_query(argv[2], argv[3]);
	}

	if (argc == 3 && std::string_view(argv[1]) == "--video-frame")
	{
		load_file_types();
		return run_linux_video_frame(argv[2]);
	}

	if (argc >= 5 && std::string_view(argv[1]) == "--set-metadata")
	{
		load_file_types();
		return set_linux_metadata(argv[2], argc - 3, argv + 3);
	}

	if (argc == 2 && std::string_view(argv[1]) == "--metadata-capabilities")
	{
		load_file_types();
		return print_native_metadata_capabilities();
	}

	if (argc >= 4 && (std::string_view(argv[1]) == "--rename-plan" || std::string_view(argv[1]) == "--rename-run"))
	{
		load_file_types();
		return run_linux_rename(argv[2], argv[3], std::string_view(argv[1]) == "--rename-run", argc - 4, argv + 4);
	}

	// /test[:filter] runs the suite headless. The '-' forms are the Linux spelling; '/test' is
	// accepted too because it is what the Windows documentation and habits say.
	for (auto i = 1; i < argc; ++i)
	{
		std::string_view arg(argv[i]);

		if (arg.starts_with("--")) arg.remove_prefix(2);
		else if (arg.starts_with('-') || arg.starts_with('/')) arg.remove_prefix(1);
		else continue;

		if (arg == "test" || arg.starts_with("test:"))
		{
			const auto colon = arg.find(':');
			const auto filter = colon == std::string_view::npos ? "*"sv : arg.substr(colon + 1);
			load_file_types();
			return run_console_tests(filter);
		}
	}

	printf("Diffractor %s (linux stage-one build)\n", df::format_version(false).c_str());

	// Exercise the portable core so a successful run proves linkage, not only a successful build.
	const auto sample = "Diffractor"sv;
	const std::vector<uint8_t> sample_bytes(sample.begin(), sample.end());

	printf("  str::to_lower       %s\n", str::to_lower(sample).c_str());
	printf("  str::icmp           %d\n", str::icmp(sample, "DIFFRACTOR"sv));
	printf("  str::fnv1a_i        %u\n", str::fnv1a_i(sample));
	printf("  base64 round trip   %s\n", base64_decode(base64_encode(sample)) == sample_bytes ? "ok" : "FAILED");
	printf("  known app_data      %s\n",
	       std::string(platform::known_path(platform::known_folder::app_data).text()).c_str());
	printf("\nRun with /test to run the suite.\n");

	return 0;
}
