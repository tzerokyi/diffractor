// This file is part of the Diffractor photo and video organizer.
// Copyright 2026 Zac Walker
// SPDX-License-Identifier: LGPL-2.1-or-later
// Purpose: Headless Linux search and single-file rename workers using the existing collection index.

#include "pch.h"
#include "model.h"
#include "model_index.h"
#include "model_locations.h"
#include "model_db.h"
#include "crypto_sha.h"

#include <cerrno>
#include <fcntl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

namespace
{
// This process owns its index and database on the same worker thread. Database callbacks are
// deferred until index work leaves its locks, then flushed on that owning thread.
class index_worker_async final : public async_strategy
{
    location_cache& _locations;
    std::deque<std::function<void(database&)>> _database_tasks;

public:
    explicit index_worker_async(location_cache& locations) : _locations(locations) {}
    void invalidate_view(view_invalid) override {}
    void queue_ui(std::function<void()> f) override { f(); }
    void queue_async(async_queue, std::function<void()> f) override { f(); }
    void queue_async_after(async_queue, uint32_t, std::function<void()> f) override { f(); }
    void queue_location(std::function<void(location_cache&)> f) override { f(_locations); }
    void queue_database(std::function<void(database&)> f) override { _database_tasks.emplace_back(std::move(f)); }
    void queue_tile_db(std::function<void(tile_cache_db&)>) override {}
    void queue_media_preview(std::function<void(media_preview_state&)>, bool) override {}
    void web_service_cache(std::string, std::function<void(const std::string&)>) override {}
    void web_service_cache(std::string, std::string) override {}

    void flush_database(database& db)
    {
        while (!_database_tasks.empty())
        {
            auto task = std::move(_database_tasks.front());
            _database_tasks.pop_front();
            task(db);
            db.perform_writes();
        }
        db.perform_writes();
    }
};

// The lock spans database open, reconciliation, query and close; separate folders use separate keys.
class cache_lock final
{
    int _fd = -1;
public:
    explicit cache_lock(const std::string& path)
    {
        _fd = ::open(path.c_str(), O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0600);
        if (_fd < 0) throw std::runtime_error("Cannot open the search cache lock.");
        if (::flock(_fd, LOCK_EX) != 0)
        {
            ::close(_fd);
            _fd = -1;
            throw std::runtime_error("Cannot lock the search cache.");
        }
    }
    ~cache_lock() { if (_fd >= 0) ::close(_fd); }
    cache_lock(const cache_lock&) = delete;
    cache_lock& operator=(const cache_lock&) = delete;
};
}

int run_linux_index_query(const char* folder, const char* query)
{
    try
    {
        const df::folder_path root(folder);
        if (!root.is_valid() || !root.exists())
        {
            std::fprintf(stderr, "Search folder does not exist or is unavailable.\n");
            return 2;
        }

        auto search = df::search_t::parse(query);
        if (!search.selectors().empty())
        {
            std::fprintf(stderr, "Search expression cannot add a separate folder scope.\n");
            return 2;
        }
        search.add_selector(df::item_selector(root, true));

        const auto* override_path = std::getenv("DIFFRACTOR_INDEX_CACHE_DIR");
        const auto cache = override_path && *override_path
            ? df::folder_path(override_path)
            : platform::known_path(platform::known_folder::app_cache_data).combine("rust-index-v1");
        const auto cache_parent = cache.parent();
        if (platform::create_folder(cache_parent).failed())
            throw std::runtime_error("Cannot create the search cache parent.");
        const auto cache_name = std::string(cache.text());
        if (::mkdir(cache_name.c_str(), 0700) != 0 && errno != EEXIST)
            throw std::runtime_error("Cannot create the search cache directory.");
        const auto cache_key = crypto::to_sha256(root.text().sv());
        cache_lock lock(cache.combine_file(cache_key + ".lock").pack());

        location_cache locations;
        locations.load_index();
        index_worker_async async(locations);
        index_state index(async, locations);
        df::index_roots roots;
        roots.folders.emplace(root);
        index.index_roots(std::move(roots));
        database db(index);
        db.open(cache, cache_key);
        if (!db.is_open()) throw std::runtime_error("Cannot open the search cache database.");
        const df::cancel_token token;
        if (!index.index_folders(token))
        {
            std::fprintf(stderr, "Could not enumerate the complete search folder.\n");
            return 3;
        }
        index.scan_uncached(token);
        if (token.is_cancelled()) return 3;
        async.flush_database(db);
        if (std::getenv("DIFFRACTOR_INDEX_REPORT_STATS"))
            std::fprintf(stderr, "scanned=%llu\n", static_cast<unsigned long long>(df::file_perf.scans.load()));

        index_state::query_item_results matches;
        bool completed = false;
        index.query_items(search, [&](index_state::query_item_results found, bool done)
        {
            matches = std::move(found);
            completed = done;
        }, token);
        if (!completed) return 3;

        // NUL framing preserves newlines in Linux filenames. Nothing is written before success.
        constexpr char header[] = "DIFIDX1";
        if (std::fwrite(header, 1, sizeof(header), stdout) != sizeof(header)) return 4;
        for (const auto& match : matches)
        {
            if (match.kind != index_state::query_item_kind::file) continue;
            const auto text = match.path.pack();
            if (std::fwrite(text.data(), 1, text.size(), stdout) != text.size() || std::fputc(0, stdout) == EOF)
                return 4;
        }
        return std::fflush(stdout) == 0 ? 0 : 4;
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "Search worker failed: %s\n", e.what());
        return 3;
    }
}

// Discover the original index's companion association before offering a rename. The run
// rebuilds the plan and requires the exact reviewed companion names before moving anything.
int run_linux_rename(const char* path_arg, const char* name_arg, const bool execute,
                     const int expected_count, const char* const* expected)
{
    try
    {
        const df::file_path source{std::string_view(path_arg)};
        const std::string_view name{name_arg};
        if (!source.is_valid() || !source.exists() || !platform::is_valid_file_name(name))
            throw std::runtime_error("The source or proposed name is invalid.");
        const df::file_path destination(source.folder(), name, source.extension());
        if (!destination.is_save_valid() || source.pack() == destination.pack())
            throw std::runtime_error("The proposed name is invalid or unchanged.");

        location_cache locations;
        index_worker_async async(locations);
        index_state index(async, locations);
        // validate_folder performs the original companion association for this folder only;
        // walking the whole subtree to rename one file would make large libraries unusable.
        const auto node = index.validate_folder(source.folder(), true, platform::now());
        if (node.enumeration_failed || node.deferred || !node.folder)
            throw std::runtime_error("Cannot enumerate the complete source folder.");
        const auto& files = node.folder->files;
        auto found = std::ranges::find_if(files, [&](const auto& item)
        {
            return source.folder().combine_file(item.name).pack() == source.pack();
        });
        if (found == files.end()) throw std::runtime_error("The selected file is not in the folder index.");

        const auto metadata = found->metadata.load();
        std::vector<std::string> sidecars;
        if (metadata)
            for (const auto part : split(metadata->sidecars, true)) sidecars.emplace_back(part);
        std::ranges::sort(sidecars);

        // A shared companion cannot be moved without changing an unselected item's metadata.
        for (const auto& other : files)
        {
            if (source.folder().combine_file(other.name).pack() == source.pack()) continue;
            const auto other_metadata = other.metadata.load();
            if (!other_metadata) continue;
            for (const auto other_sidecar : split(other_metadata->sidecars, true))
                if (std::ranges::find(sidecars, other_sidecar) != sidecars.end())
                    throw std::runtime_error("A sidecar is shared with another file; rename is blocked.");
        }
        if (execute)
        {
            if (static_cast<size_t>(expected_count) != sidecars.size())
                throw std::runtime_error("Sidecars changed after review; analyze again.");
            for (size_t i = 0; i < sidecars.size(); ++i)
                if (sidecars[i] != expected[i]) throw std::runtime_error("Sidecars changed after review; analyze again.");
        }

        std::vector<std::pair<df::file_path, df::file_path>> moves{{source, destination}};
        std::set<std::string> targets{destination.pack()};
        for (const auto& sidecar : sidecars)
        {
            const auto from = source.folder().combine_file(sidecar);
            const auto to = source.folder().combine_file(name).extension(from.extension());
            if (!targets.insert(to.pack()).second) throw std::runtime_error("Multiple files would use one destination.");
            moves.emplace_back(from, to);
        }
        for (const auto& [from, to] : moves)
        {
            if (!from.exists()) throw std::runtime_error("A source file is unavailable.");
            if (to.exists()) throw std::runtime_error("A destination already exists; nothing was renamed.");
        }
        if (!execute)
        {
            constexpr char header[] = "DIFREN1";
            if (std::fwrite(header, 1, sizeof(header), stdout) != sizeof(header)) return 4;
            for (const auto& [from, to] : moves)
                for (const auto& path : {from.pack(), to.pack()})
                    if (std::fwrite(path.data(), 1, path.size(), stdout) != path.size() || std::fputc(0, stdout) == EOF) return 4;
            return std::fflush(stdout) == 0 ? 0 : 4;
        }
        df::item_element item(source, *found);
        const auto result = item.rename(index, name);
        if (result.failed()) throw std::runtime_error(result.format_error());
        return 0;
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "Rename failed: %s\n", e.what());
        return 1;
    }
}
