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
    struct SessionStats;
    struct SessionSettings;

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
        rust::String resume_data
    );

    std::unique_ptr<torrent_handle> bridge_add_torrent_file(
        session &ses,
        rust::Str torrent_path,
        rust::Str save_path,
        bool sequential_download,
        int32_t max_connections,
        int32_t max_uploads,
        rust::String resume_data
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
    rust::String bridge_get_resume_data(const torrent_handle &hdl);

    rust::String bridge_get_libtorrent_version();
    rust::String bridge_info_hash_to_string(const torrent_handle &hdl);
    bool bridge_torrent_is_valid(const torrent_handle &hdl);

    void bridge_set_file_priority(const torrent_handle &hdl, int32_t file_index, int32_t priority);
    rust::Vec<int32_t> bridge_get_file_priorities(const torrent_handle &hdl);
}
