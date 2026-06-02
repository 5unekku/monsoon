#pragma once

#include "rust/cxx.h"

#include <libtorrent/session.hpp>
#include <libtorrent/session_status.hpp>
#include <libtorrent/session_params.hpp>
#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/torrent_handle.hpp>
#include <libtorrent/torrent_status.hpp>
#include <libtorrent/alert.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/bencode.hpp>
#include <libtorrent/entry.hpp>
#include <libtorrent/read_resume_data.hpp>
#include <libtorrent/write_resume_data.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/peer_info.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/error_code.hpp>
#include <libtorrent/ip_filter.hpp>

#include <memory>
#include <string>
#include <vector>
#include <cstdint>

namespace rustbridge {
    using session = lt::session;
    using torrent_handle = lt::torrent_handle;

    struct TorrentStatus;
    struct AlertInfo;
    struct PeerInfo;
    struct TorrentFile;
    struct TorrentTracker;
    struct SessionStats;
    struct SessionSettings;
    struct PendingResume;

    std::unique_ptr<session> bridge_create_session(
        rust::String listen_interfaces,
        int32_t alert_mask,
        rust::String user_agent,
        const SessionSettings &settings
    );

    void bridge_session_apply_settings(session &ses, const SessionSettings &settings);

    std::unique_ptr<torrent_handle> bridge_add_torrent_magnet(
        session &ses,
        rust::Str magnet_uri,
        rust::Str save_path,
        bool sequential_download,
        int32_t max_connections,
        int32_t max_uploads,
        rust::Slice<const uint8_t> resume_data
    );

    std::unique_ptr<torrent_handle> bridge_add_torrent_file(
        session &ses,
        rust::Str torrent_path,
        rust::Str save_path,
        bool sequential_download,
        int32_t max_connections,
        int32_t max_uploads,
        rust::Slice<const uint8_t> resume_data
    );

    void bridge_remove_torrent(session &ses, const torrent_handle &hdl, bool remove_files);
    void bridge_torrent_force_recheck(const torrent_handle &hdl);
    void bridge_torrent_pause(const torrent_handle &hdl);
    void bridge_torrent_resume(const torrent_handle &hdl);

    TorrentStatus bridge_get_torrent_status(const torrent_handle &hdl);
    rust::Vec<TorrentFile> bridge_get_torrent_files(const torrent_handle &hdl);
    rust::Vec<PeerInfo> bridge_get_torrent_peers(const torrent_handle &hdl);
    rust::Vec<AlertInfo> bridge_pop_alerts(session &ses);
    SessionStats bridge_get_session_stats(const session &ses);
    rust::String bridge_get_libtorrent_version();
    rust::String bridge_info_hash_to_string(const torrent_handle &hdl);
    bool bridge_torrent_is_valid(const torrent_handle &hdl);

    void bridge_set_file_priority(const torrent_handle &hdl, int32_t file_index, int32_t priority);
    rust::Vec<int32_t> bridge_get_file_priorities(const torrent_handle &hdl);

    // submit an async rename. libtorrent emits file_renamed_alert on success or
    // file_rename_failed_alert on failure; both go through bridge_pop_alerts.
    void bridge_torrent_rename_file(const torrent_handle &hdl, int32_t file_index, rust::Str new_name);

    // force a tracker re-announce immediately (ignores the announce interval)
    void bridge_torrent_force_reannounce(const torrent_handle &hdl);

    // submit an async storage move. emits storage_moved_alert / storage_moved_failed_alert.
    void bridge_torrent_move_storage(const torrent_handle &hdl, rust::Str new_save_path);

    // tracker list for a torrent (one entry per tier endpoint)
    rust::Vec<TorrentTracker> bridge_get_torrent_trackers(const torrent_handle &hdl);

    // per-file completion fraction (0.0..=1.0), one entry per file in order
    rust::Vec<float> bridge_get_file_progress(const torrent_handle &hdl);

    // build a shareable magnet URI for an active torrent
    rust::String bridge_make_magnet_uri(const torrent_handle &hdl);

    // toggle the sequential_download flag at runtime (front-to-back piece order)
    void bridge_torrent_set_sequential(const torrent_handle &hdl, bool enabled);

    // bind this torrent's outgoing connections to a specific interface
    void bridge_torrent_use_interface(const torrent_handle &hdl, rust::Str interface);

    // add a tracker url to a torrent at the given tier
    void bridge_torrent_add_tracker(const torrent_handle &hdl, rust::Str url, int32_t tier);

    // remove a tracker by url from a torrent
    void bridge_torrent_remove_tracker(const torrent_handle &hdl, rust::Str url);

    // load an ip filter from disk. returns rules-loaded count, or -1 on error.
    int32_t bridge_session_load_ip_filter(session &ses, rust::Str path);

    // per-torrent rate limits (bytes/sec). -1 = inherit global, 0 = unlimited.
    void bridge_torrent_set_download_limit(const torrent_handle &hdl, int32_t limit);
    void bridge_torrent_set_upload_limit(const torrent_handle &hdl, int32_t limit);
    int32_t bridge_torrent_download_limit(const torrent_handle &hdl);
    int32_t bridge_torrent_upload_limit(const torrent_handle &hdl);

    // async session stats: triggers post_session_stats; the resulting alert
    // updates an internal snapshot read by bridge_get_session_stats.
    void bridge_session_post_stats(session &ses);

    // async resume save: triggers save_resume_data; the resulting alert
    // stashes the bencoded blob keyed by info_hash for later drain.
    void bridge_torrent_save_resume_data_async(const torrent_handle &hdl);
    rust::Vec<PendingResume> bridge_take_pending_resume_data();
}
