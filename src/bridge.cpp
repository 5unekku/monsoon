#include "bridge.h"
#include "rustor/src/bridge.rs.h"

#include <sstream>
#include <stdexcept>

// suppress deprecated warnings for apis we have to keep using until async
// session stats and async resume saves are wired up properly
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"

namespace rustbridge {

// ─── Helpers ───────────────────────────────────────────────────────────────

static void apply_settings_pack(lt::settings_pack &pack, const SessionSettings &s) {
    pack.set_int(lt::settings_pack::download_rate_limit, s.download_rate_limit);
    pack.set_int(lt::settings_pack::upload_rate_limit, s.upload_rate_limit);
    pack.set_int(lt::settings_pack::connections_limit, s.max_connections);
    pack.set_int(lt::settings_pack::unchoke_slots_limit, s.max_uploads);

    pack.set_bool(lt::settings_pack::enable_dht, s.enable_dht);
    pack.set_bool(lt::settings_pack::enable_lsd, s.enable_lsd);
    pack.set_bool(lt::settings_pack::enable_upnp, s.enable_upnp);
    pack.set_bool(lt::settings_pack::enable_natpmp, s.enable_natpmp);

    pack.set_bool(lt::settings_pack::anonymous_mode, s.anonymous_mode);

    pack.set_int(lt::settings_pack::out_enc_policy, s.encryption_out_policy);
    pack.set_int(lt::settings_pack::in_enc_policy, s.encryption_in_policy);

    pack.set_bool(lt::settings_pack::enable_incoming_utp, s.enable_incoming_utp);
    pack.set_bool(lt::settings_pack::enable_outgoing_utp, s.enable_outgoing_utp);

    pack.set_bool(lt::settings_pack::announce_to_all_trackers, s.announce_to_all_trackers);
    pack.set_bool(lt::settings_pack::announce_to_all_tiers, s.announce_to_all_tiers);

    pack.set_bool(lt::settings_pack::ssrf_mitigation, s.ssrf_mitigation);
    pack.set_bool(lt::settings_pack::validate_https_trackers, s.validate_https_trackers);

    pack.set_int(lt::settings_pack::active_downloads, s.max_active_downloads);
    pack.set_int(lt::settings_pack::active_seeds, s.max_active_uploads);
    pack.set_int(lt::settings_pack::active_limit, s.max_active_torrents);

    // libtorrent expresses ratio as integer * 100 (so 1.5 → 150). 0 disables.
    pack.set_int(lt::settings_pack::share_ratio_limit,
        static_cast<int>(s.seed_ratio_limit * 100.0));
    // libtorrent seed_time_limit is in seconds; we accept minutes for ergonomics
    pack.set_int(lt::settings_pack::seed_time_limit, s.seed_time_limit * 60);
}

// return best available info hash as a hex string (v1 preferred, v2 fallback)
static std::string info_hash_str(const lt::info_hash_t &ih) {
    if (ih.has_v1()) return ih.v1.to_string();
    if (ih.has_v2()) return ih.v2.to_string();
    return "";
}

// ─── Session Management ────────────────────────────────────────────────────

std::unique_ptr<lt::session> bridge_create_session(
    rust::String listen_interfaces,
    int32_t alert_mask,
    rust::String user_agent,
    const SessionSettings &settings
) {
    lt::settings_pack pack;

    pack.set_int(lt::settings_pack::alert_mask, alert_mask);

    pack.set_str(lt::settings_pack::listen_interfaces,
        listen_interfaces.size() > 0
            ? std::string(listen_interfaces)
            : std::string("0.0.0.0:6881,[::]:6881"));

    if (user_agent.size() > 0)
        pack.set_str(lt::settings_pack::user_agent, std::string(user_agent));

    pack.set_str(lt::settings_pack::dht_bootstrap_nodes,
        "dht.libtorrent.org:25401,router.bittorrent.com:6881,"
        "dht.transmissionbt.com:6881,router.utorrent.com:6881");

    pack.set_int(lt::settings_pack::active_tracker_limit, 100);
    pack.set_int(lt::settings_pack::active_dht_limit, 100);
    pack.set_int(lt::settings_pack::active_lsd_limit, 100);

    apply_settings_pack(pack, settings);

    lt::session_params params;
    params.settings = std::move(pack);
    return std::make_unique<lt::session>(std::move(params));
}

void bridge_session_apply_settings(lt::session &ses, const SessionSettings &settings) {
    lt::settings_pack pack;
    apply_settings_pack(pack, settings);
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
    rust::Slice<const uint8_t> resume_data
) {
    lt::add_torrent_params p;
    lt::error_code ec;

    lt::parse_magnet_uri(std::string(magnet_uri), p, ec);
    if (ec) throw std::runtime_error("parse magnet URI: " + ec.message());

    p.save_path = std::string(save_path);
    if (sequential_download) p.flags |= lt::torrent_flags::sequential_download;
    p.max_connections = max_connections;
    p.max_uploads = max_uploads;

    if (resume_data.size() > 0) {
        lt::error_code resume_ec;
        auto resumed = lt::read_resume_data(
            lt::span<char const>(reinterpret_cast<char const*>(resume_data.data()), resume_data.size()),
            resume_ec);
        if (!resume_ec) {
            p = std::move(resumed);
            p.save_path = std::string(save_path);
        }
    }

    lt::torrent_handle h = ses.add_torrent(std::move(p), ec);
    if (ec) throw std::runtime_error("add torrent: " + ec.message());
    return std::make_unique<lt::torrent_handle>(std::move(h));
}

std::unique_ptr<lt::torrent_handle> bridge_add_torrent_file(
    lt::session &ses,
    rust::Str torrent_path,
    rust::Str save_path,
    bool sequential_download,
    int32_t max_connections,
    int32_t max_uploads,
    rust::Slice<const uint8_t> resume_data
) {
    lt::add_torrent_params p;
    lt::error_code ec;

    lt::torrent_info ti(std::string(torrent_path), ec);
    if (ec) throw std::runtime_error("load torrent file: " + ec.message());
    p.ti = std::make_shared<lt::torrent_info>(std::move(ti));
    p.save_path = std::string(save_path);

    if (sequential_download) p.flags |= lt::torrent_flags::sequential_download;
    p.max_connections = max_connections;
    p.max_uploads = max_uploads;

    if (resume_data.size() > 0) {
        lt::error_code resume_ec;
        auto resumed = lt::read_resume_data(
            lt::span<char const>(reinterpret_cast<char const*>(resume_data.data()), resume_data.size()),
            resume_ec);
        if (!resume_ec) {
            p = std::move(resumed);
            p.save_path = std::string(save_path);
        }
    }

    lt::torrent_handle h = ses.add_torrent(std::move(p), ec);
    if (ec) throw std::runtime_error("add torrent: " + ec.message());
    return std::make_unique<lt::torrent_handle>(std::move(h));
}

void bridge_remove_torrent(lt::session &ses, const lt::torrent_handle &hdl, bool remove_files) {
    if (remove_files) ses.remove_torrent(hdl, lt::session::delete_files);
    else ses.remove_torrent(hdl);
}

void bridge_torrent_force_recheck(const lt::torrent_handle &hdl) { hdl.force_recheck(); }
void bridge_torrent_pause(const lt::torrent_handle &hdl) { hdl.pause(); }
void bridge_torrent_resume(const lt::torrent_handle &hdl) { hdl.resume(); }

// ─── Torrent Status ────────────────────────────────────────────────────────

static std::string state_to_string(lt::torrent_status::state_t state) {
    switch (state) {
        case lt::torrent_status::checking_files:        return "checking_files";
        case lt::torrent_status::downloading_metadata:  return "downloading_metadata";
        case lt::torrent_status::downloading:           return "downloading";
        case lt::torrent_status::finished:              return "finished";
        case lt::torrent_status::seeding:               return "seeding";
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
    ts.info_hash = rust::String(info_hash_str(hdl.info_hashes()).c_str());
    ts.state = rust::String(state_to_string(st.state).c_str());
    ts.save_path = rust::String(st.save_path.c_str());
    ts.progress = st.progress;
    ts.total_download = st.total_download;
    ts.total_upload = st.total_upload;
    ts.total_done = st.total_done;
    ts.total_wanted = st.total_wanted;
    ts.download_rate = st.download_rate;
    ts.upload_rate = st.upload_rate;
    ts.connected_peers = st.num_peers;
    ts.connected_seeds = st.num_seeds;
    ts.total_peers = st.list_peers;
    ts.total_seeds = st.list_seeds;
    ts.num_pieces = st.num_pieces;
    ts.num_completed_pieces = (int32_t)st.pieces.count();
    ts.error = rust::String(st.errc.message().c_str());
    ts.is_paused = bool(st.flags & lt::torrent_flags::paused);
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
    auto &fs = ti->files();
    for (int i = 0; i < ti->num_files(); ++i) {
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
        std::string flags;
        if (p.flags & lt::peer_info::seed)                flags += "S";
        if (p.flags & lt::peer_info::optimistic_unchoke)  flags += "O";
        if (p.flags & lt::peer_info::snubbed)             flags += "s";
        if (p.flags & lt::peer_info::upload_only)         flags += "U";
        if (p.flags & lt::peer_info::holepunched)         flags += "H";
        if (p.flags & lt::peer_info::rc4_encrypted)       flags += "E";
        if (p.flags & lt::peer_info::plaintext_encrypted) flags += "e";
        pi.flags = rust::String(flags.c_str());
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
        int cat = a->category();
        if      (cat & lt::alert::error_notification)        ai.category = rust::String("error");
        else if (cat & lt::alert::status_notification)       ai.category = rust::String("status");
        else if (cat & lt::alert::storage_notification)      ai.category = rust::String("storage");
        else if (cat & lt::alert::tracker_notification)      ai.category = rust::String("tracker");
        else if (cat & lt::alert::peer_notification)         ai.category = rust::String("peer");
        else if (cat & lt::alert::dht_notification)          ai.category = rust::String("dht");
        else if (cat & lt::alert::port_mapping_notification) ai.category = rust::String("port_mapping");
        else                                                 ai.category = rust::String("other");
        alerts.push_back(std::move(ai));
    }
    return alerts;
}

// ─── Session Stats ─────────────────────────────────────────────────────────
// TODO: migrate to async post_session_stats() + session_stats_alert
// the synchronous session_status() api is deprecated in libtorrent 2.x but
// the async replacement requires wiring up a stats accumulation loop

rustbridge::SessionStats bridge_get_session_stats(const lt::session &ses) {
    rustbridge::SessionStats ss = {};
    lt::session_status st = ses.status();
    ss.download_rate = st.download_rate;
    ss.upload_rate = st.upload_rate;
    ss.total_download = st.total_download;
    ss.total_upload = st.total_upload;
    ss.total_dht_nodes = st.dht_nodes;
    ss.num_peers = st.num_peers;
    // dht/lsd/upnp/natpmp running flags are deliberately not populated —
    // they were dead-read everywhere and would need to come from listen_succeeded
    // / dht_bootstrap alerts instead. add back if a consumer materializes.
    return ss;
}

// ─── Resume Data ───────────────────────────────────────────────────────────
// TODO: migrate to async save_resume_data() + save_resume_data_alert
// write_resume_data() is deprecated; returning raw bytes avoids utf-8 corruption

rust::Vec<uint8_t> bridge_get_resume_data(const lt::torrent_handle &hdl) {
    rust::Vec<uint8_t> result;
    if (!hdl.is_valid()) return result;
    lt::entry rd = hdl.write_resume_data();
    std::vector<char> buf;
    lt::bencode(std::back_inserter(buf), rd);
    for (char c : buf) result.push_back(static_cast<uint8_t>(c));
    return result;
}

// ─── Utility ───────────────────────────────────────────────────────────────

rust::String bridge_get_libtorrent_version() { return rust::String(LIBTORRENT_VERSION); }

rust::String bridge_info_hash_to_string(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return rust::String("");
    return rust::String(info_hash_str(hdl.info_hashes()).c_str());
}

bool bridge_torrent_is_valid(const lt::torrent_handle &hdl) { return hdl.is_valid(); }

// ─── File Priority ─────────────────────────────────────────────────────────

void bridge_set_file_priority(const lt::torrent_handle &hdl, int32_t file_index, int32_t priority) {
    if (hdl.is_valid())
        hdl.file_priority(lt::file_index_t{file_index}, lt::download_priority_t{static_cast<uint8_t>(priority)});
}

rust::Vec<int32_t> bridge_get_file_priorities(const lt::torrent_handle &hdl) {
    rust::Vec<int32_t> result;
    if (!hdl.is_valid()) return result;
    for (int p : hdl.file_priorities()) result.push_back(p);
    return result;
}

// ─── Rename ────────────────────────────────────────────────────────────────
// the actual outcome (file_renamed_alert or file_rename_failed_alert) is
// surfaced asynchronously through bridge_pop_alerts.

void bridge_torrent_rename_file(const lt::torrent_handle &hdl, int32_t file_index, rust::Str new_name) {
    if (!hdl.is_valid()) return;
    hdl.rename_file(lt::file_index_t{file_index}, std::string(new_name));
}

// ─── Reannounce / Move / Trackers / File Progress ──────────────────────────

void bridge_torrent_force_reannounce(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return;
    // 0s delay, all tiers; libtorrent rate-limits internally
    hdl.force_reannounce();
}

void bridge_torrent_move_storage(const lt::torrent_handle &hdl, rust::Str new_save_path) {
    if (!hdl.is_valid()) return;
    hdl.move_storage(std::string(new_save_path));
}

rust::Vec<rustbridge::TorrentTracker> bridge_get_torrent_trackers(const lt::torrent_handle &hdl) {
    rust::Vec<rustbridge::TorrentTracker> trackers;
    if (!hdl.is_valid()) return trackers;
    for (auto const &announce : hdl.trackers()) {
        rustbridge::TorrentTracker tracker;
        tracker.url = rust::String(announce.url.c_str());
        tracker.tier = static_cast<int32_t>(announce.tier);
        tracker.verified = announce.verified;
        // updating / fails / message are per-endpoint — surface the worst across endpoints
        bool any_updating = false;
        int worst_fails = 0;
        std::string worst_message;
        for (auto const &endpoint : announce.endpoints) {
            for (auto const &info : endpoint.info_hashes) {
                if (info.updating) any_updating = true;
                if (info.fails > worst_fails) worst_fails = info.fails;
                if (!info.last_error.message().empty() && worst_message.empty())
                    worst_message = info.last_error.message();
            }
        }
        tracker.updating = any_updating;
        tracker.fails = worst_fails;
        tracker.message = rust::String(worst_message.c_str());
        trackers.push_back(std::move(tracker));
    }
    return trackers;
}

rust::String bridge_make_magnet_uri(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return rust::String("");
    return rust::String(lt::make_magnet_uri(hdl).c_str());
}

void bridge_torrent_set_sequential(const lt::torrent_handle &hdl, bool enabled) {
    if (!hdl.is_valid()) return;
    if (enabled) hdl.set_flags(lt::torrent_flags::sequential_download);
    else hdl.unset_flags(lt::torrent_flags::sequential_download);
}

rust::Vec<float> bridge_get_file_progress(const lt::torrent_handle &hdl) {
    rust::Vec<float> result;
    if (!hdl.is_valid()) return result;
    auto ti = hdl.torrent_file();
    if (!ti) return result;
    std::vector<int64_t> bytes;
    hdl.file_progress(bytes, lt::torrent_handle::piece_granularity);
    auto &fs = ti->files();
    for (size_t i = 0; i < bytes.size(); ++i) {
        int64_t total = fs.file_size(static_cast<lt::file_index_t>(static_cast<int>(i)));
        float fraction = total > 0 ? static_cast<float>(bytes[i]) / static_cast<float>(total) : 0.0f;
        result.push_back(fraction);
    }
    return result;
}

} // namespace rustbridge
