#include "bridge.h"

// Include cxx-generated struct definitions for shared types
#include "rustor/src/bridge.rs.h"

#include <sstream>
#include <stdexcept>

namespace rustbridge {

// ─── Session Management ────────────────────────────────────────────────────

std::unique_ptr<lt::session> bridge_create_session(
    rust::String listen_interfaces,
    int32_t alert_mask,
    int32_t max_uploads,
    int32_t max_connections,
    int32_t download_rate_limit,
    int32_t upload_rate_limit,
    rust::String user_agent
) {
    lt::settings_pack pack;

    // Alert mask
    pack.set_int(lt::settings_pack::alert_mask, alert_mask);

    // Listen interfaces
    if (listen_interfaces.size() > 0) {
        pack.set_str(lt::settings_pack::listen_interfaces, std::string(listen_interfaces));
    } else {
        pack.set_str(lt::settings_pack::listen_interfaces, "0.0.0.0:6881,[::]:6881");
    }

    // User agent
    if (user_agent.size() > 0) {
        pack.set_str(lt::settings_pack::user_agent, std::string(user_agent));
    }

    // Rate limits (0 = unlimited)
    pack.set_int(lt::settings_pack::download_rate_limit, download_rate_limit);
    pack.set_int(lt::settings_pack::upload_rate_limit, upload_rate_limit);

    // Connection limits
    pack.set_int(lt::settings_pack::connections_limit, max_connections);
    pack.set_int(lt::settings_pack::unchoke_slots_limit, max_uploads);

    // Enable DHT, LSD, UPNP, NAT-PMP
    pack.set_bool(lt::settings_pack::enable_dht, true);
    pack.set_bool(lt::settings_pack::enable_lsd, true);
    pack.set_bool(lt::settings_pack::enable_upnp, true);
    pack.set_bool(lt::settings_pack::enable_natpmp, true);

    // DHT settings
    pack.set_str(lt::settings_pack::dht_bootstrap_nodes,
        "dht.libtorrent.org:25401,router.bittorrent.com:6881,"
        "dht.transmissionbt.com:6881,router.utorrent.com:6881");

    // Performance settings
    pack.set_int(lt::settings_pack::active_downloads, 8);
    pack.set_int(lt::settings_pack::active_seeds, 4);
    pack.set_int(lt::settings_pack::active_limit, 16);
    pack.set_int(lt::settings_pack::active_tracker_limit, 100);
    pack.set_int(lt::settings_pack::active_dht_limit, 100);
    pack.set_int(lt::settings_pack::active_lsd_limit, 100);

    // Enable utp
    pack.set_bool(lt::settings_pack::enable_incoming_utp, true);
    pack.set_bool(lt::settings_pack::enable_outgoing_utp, true);

    // Create session with params
    lt::session_params params;
    params.settings = std::move(pack);

    auto ses = std::make_unique<lt::session>(std::move(params));

    return ses;
}

void bridge_session_apply_settings(
    lt::session &ses,
    int32_t max_uploads,
    int32_t max_connections,
    int32_t download_rate_limit,
    int32_t upload_rate_limit
) {
    lt::settings_pack pack;
    pack.set_int(lt::settings_pack::download_rate_limit, download_rate_limit);
    pack.set_int(lt::settings_pack::upload_rate_limit, upload_rate_limit);
    pack.set_int(lt::settings_pack::connections_limit, max_connections);
    pack.set_int(lt::settings_pack::unchoke_slots_limit, max_uploads);
    ses.apply_settings(std::move(pack));
}

// ─── Torrent Management ────────────────────────────────────────────────────

std::unique_ptr<lt::torrent_handle> bridge_add_torrent_magnet(
    lt::session &ses,
    rust::Str magnet_uri,
    rust::Str save_path,
    bool sequential_download,
    int32_t max_connections,
    int32_t max_uploads,
    rust::String resume_data
) {
    lt::add_torrent_params p;

    // Parse magnet URI
    lt::error_code ec;
    lt::parse_magnet_uri(std::string(magnet_uri), p, ec);
    if (ec) {
        throw std::runtime_error("Failed to parse magnet URI: " + ec.message());
    }

    p.save_path = std::string(save_path);
    if (sequential_download) {
        p.flags |= lt::torrent_flags::sequential_download;
    }
    p.max_connections = max_connections;
    p.max_uploads = max_uploads;

    // Apply resume data if provided
    if (resume_data.size() > 0) {
        lt::error_code resume_ec;
        auto resume_params = lt::read_resume_data(
            lt::span<char const>(resume_data.data(), resume_data.size()),
            resume_ec
        );
        if (!resume_ec) {
            p = std::move(resume_params);
            p.save_path = std::string(save_path);
        }
    }

    lt::torrent_handle h = ses.add_torrent(std::move(p), ec);
    if (ec) {
        throw std::runtime_error("Failed to add torrent: " + ec.message());
    }

    return std::make_unique<lt::torrent_handle>(std::move(h));
}

std::unique_ptr<lt::torrent_handle> bridge_add_torrent_file(
    lt::session &ses,
    rust::Str torrent_path,
    rust::Str save_path,
    bool sequential_download,
    int32_t max_connections,
    int32_t max_uploads,
    rust::String resume_data
) {
    lt::add_torrent_params p;

    // Load torrent file
    lt::error_code ec;
    lt::torrent_info ti(std::string(torrent_path), ec);
    if (ec) {
        throw std::runtime_error("Failed to load torrent file: " + ec.message());
    }
    p.ti = std::make_shared<lt::torrent_info>(std::move(ti));

    p.save_path = std::string(save_path);
    if (sequential_download) {
        p.flags |= lt::torrent_flags::sequential_download;
    }
    p.max_connections = max_connections;
    p.max_uploads = max_uploads;

    // Apply resume data if provided
    if (resume_data.size() > 0) {
        lt::error_code resume_ec;
        auto resume_params = lt::read_resume_data(
            lt::span<char const>(resume_data.data(), resume_data.size()),
            resume_ec
        );
        if (!resume_ec) {
            p = std::move(resume_params);
            p.save_path = std::string(save_path);
        }
    }

    lt::torrent_handle h = ses.add_torrent(std::move(p), ec);
    if (ec) {
        throw std::runtime_error("Failed to add torrent: " + ec.message());
    }

    return std::make_unique<lt::torrent_handle>(std::move(h));
}

void bridge_remove_torrent(lt::session &ses, const lt::torrent_handle &hdl, bool remove_files) {
    if (remove_files) {
        ses.remove_torrent(hdl, lt::session::delete_files);
    } else {
        ses.remove_torrent(hdl);
    }
}

void bridge_pause_torrent(const lt::torrent_handle &hdl) {
    hdl.pause();
}

void bridge_resume_torrent(const lt::torrent_handle &hdl) {
    hdl.resume();
}

void bridge_torrent_force_recheck(const lt::torrent_handle &hdl) {
    hdl.force_recheck();
}

void bridge_torrent_pause(const lt::torrent_handle &hdl) {
    hdl.pause();
}

void bridge_torrent_resume(const lt::torrent_handle &hdl) {
    hdl.resume();
}

// ─── Torrent Status ────────────────────────────────────────────────────────

static std::string state_to_string(lt::torrent_status::state_t s) {
    switch (s) {
        case lt::torrent_status::checking_files:        return "checking_files";
        case lt::torrent_status::downloading_metadata:  return "downloading_metadata";
        case lt::torrent_status::downloading:           return "downloading";
        case lt::torrent_status::finished:              return "finished";
        case lt::torrent_status::seeding:               return "seeding";
        case lt::torrent_status::allocating:            return "allocating";
        case lt::torrent_status::checking_resume_data:  return "checking_resume_data";
        default:                                        return "unknown";
    }
}

rustbridge::TorrentStatus bridge_get_torrent_status(const lt::torrent_handle &hdl) {
    rustbridge::TorrentStatus ts;

    if (!hdl.is_valid()) {
        ts.name = rust::String("invalid");
        ts.state = rust::String("invalid");
        return ts;
    }

    lt::torrent_status st = hdl.status();

    ts.name = rust::String(st.name.c_str());
    ts.info_hash = rust::String(st.info_hash.to_string().c_str());
    ts.state = rust::String(state_to_string(st.state).c_str());
    ts.save_path = rust::String(st.save_path.c_str());
    ts.progress = st.progress;
    ts.total_download = st.total_download;
    ts.total_upload = st.total_upload;
    ts.total_done = st.total_done;
    ts.total_wanted = st.total_wanted;
    ts.download_rate = st.download_rate;
    ts.upload_rate = st.upload_rate;
    ts.total_peers = st.num_peers;
    ts.connected_peers = st.num_peers - st.num_seeds;
    ts.total_seeds = st.num_seeds;
    ts.connected_seeds = st.num_seeds;
    ts.num_pieces = st.num_pieces;
    ts.num_completed_pieces = st.num_pieces;
    ts.error = rust::String(st.errc.message().c_str());
    ts.is_paused = st.paused;
    ts.is_finished = st.is_finished;
    ts.is_seeding = st.state == lt::torrent_status::seeding;
    ts.has_metadata = hdl.torrent_file() != nullptr;
    ts.added_time = st.added_time;
    ts.completed_time = st.completed_time;
    ts.list_peers = st.list_peers;
    ts.list_seeds = st.list_seeds;

    return ts;
}

// ─── File Info ─────────────────────────────────────────────────────────────

rust::Vec<rustbridge::TorrentFile> bridge_get_torrent_files(const lt::torrent_handle &hdl) {
    rust::Vec<rustbridge::TorrentFile> files;

    if (!hdl.is_valid()) return files;

    auto ti = hdl.torrent_file();
    if (!ti) return files;

    auto num_files = ti->num_files();
    auto &fs = ti->files();

    for (int i = 0; i < num_files; ++i) {
        rustbridge::TorrentFile tf;
        tf.path = rust::String(fs.file_path(i).c_str());
        tf.size = fs.file_size(i);
        tf.offset = fs.file_offset(i);
        files.push_back(std::move(tf));
    }

    return files;
}

// ─── Peer Info ─────────────────────────────────────────────────────────────

rust::Vec<rustbridge::PeerInfo> bridge_get_torrent_peers(const lt::torrent_handle &hdl) {
    rust::Vec<rustbridge::PeerInfo> peers;

    if (!hdl.is_valid()) return peers;

    std::vector<lt::peer_info> peer_list;
    hdl.get_peer_info(peer_list);

    for (auto &p : peer_list) {
        rustbridge::PeerInfo pi;
        pi.ip = rust::String(p.ip.address().to_string().c_str());
        pi.port = p.ip.port();
        pi.download_rate = p.down_speed;
        pi.upload_rate = p.up_speed;
        pi.client = rust::String(p.client.c_str());
        pi.progress = p.progress;
        pi.flags = rust::String(""); // flags is a bitfield enum in libtorrent 2.0
        peers.push_back(std::move(pi));
    }

    return peers;
}

// ─── Alert Polling ─────────────────────────────────────────────────────────

rust::Vec<rustbridge::AlertInfo> bridge_pop_alerts(lt::session &ses) {
    rust::Vec<rustbridge::AlertInfo> alerts;

    std::vector<lt::alert *> popped;
    ses.pop_alerts(&popped);

    for (auto *a : popped) {
        rustbridge::AlertInfo ai;
        ai.timestamp = a->timestamp().time_since_epoch().count();
        ai.message = rust::String(a->message().c_str());
        ai.alert_type = rust::String(a->what());

        // Categorize using the alert type directly
        int cat = a->type();
        if (cat >= lt::alert::status_notification) {
            ai.category = rust::String("status");
        } else if (cat >= lt::alert::error_notification) {
            ai.category = rust::String("error");
        } else if (cat >= lt::alert::peer_notification) {
            ai.category = rust::String("peer");
        } else if (cat >= lt::alert::port_mapping_notification) {
            ai.category = rust::String("port_mapping");
        } else if (cat >= lt::alert::storage_notification) {
            ai.category = rust::String("storage");
        } else if (cat >= lt::alert::tracker_notification) {
            ai.category = rust::String("tracker");
        } else if (cat >= lt::alert::progress_notification) {
            ai.category = rust::String("progress");
        } else if (cat >= lt::alert::ip_block_notification) {
            ai.category = rust::String("ip_block");
        } else if (cat >= lt::alert::performance_warning) {
            ai.category = rust::String("performance");
        } else if (cat >= lt::alert::dht_notification) {
            ai.category = rust::String("dht");
        } else {
            ai.category = rust::String("other");
        }

        alerts.push_back(std::move(ai));
    }

    return alerts;
}

// ─── Session Stats ─────────────────────────────────────────────────────────

rustbridge::SessionStats bridge_get_session_stats(const lt::session &ses) {
    rustbridge::SessionStats ss;
    ss.total_download = 0;
    ss.total_upload = 0;
    ss.download_rate = 0;
    ss.upload_rate = 0;
    ss.num_torrents = 0;
    ss.active_torrents = 0;
    ss.paused_torrents = 0;
    ss.total_dht_nodes = 0;
    ss.num_peers = 0;
    ss.dht_running = false;
    ss.lsd_running = false;
    ss.upnp_running = false;
    ss.natpmp_running = false;

    // libtorrent 2.0 uses session_status()
    lt::session_status st = ses.status();
    ss.total_dht_nodes = st.dht_nodes;
    ss.num_peers = st.num_peers;
    ss.dht_running = true;

    return ss;
}

// ─── Resume Data ───────────────────────────────────────────────────────────

rust::String bridge_get_resume_data(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return rust::String("");

    lt::entry rd = hdl.write_resume_data();

    std::vector<char> buf;
    lt::bencode(std::back_inserter(buf), rd);

    return rust::String(buf.data(), buf.size());
}

// ─── Utility ───────────────────────────────────────────────────────────────

rust::String bridge_get_libtorrent_version() {
    return rust::String(LIBTORRENT_VERSION);
}

rust::String bridge_info_hash_to_string(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return rust::String("");
    return rust::String(hdl.info_hash().to_string().c_str());
}

bool bridge_torrent_is_valid(const lt::torrent_handle &hdl) {
    return hdl.is_valid();
}

// ─── File Priority ─────────────────────────────────────────────────────────

void bridge_set_file_priority(const lt::torrent_handle &hdl, int32_t file_index, int32_t priority) {
    if (!hdl.is_valid()) return;
    hdl.file_priority(file_index, priority);
}

rust::Vec<int32_t> bridge_get_file_priorities(const lt::torrent_handle &hdl) {
    rust::Vec<int32_t> priorities;
    if (!hdl.is_valid()) return priorities;

    auto prio_vec = hdl.file_priorities();
    for (auto p : prio_vec) {
        priorities.push_back(static_cast<int32_t>(p));
    }
    return priorities;
}

} // namespace rustbridge
