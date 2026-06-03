#include "bridge.h"
#include "monsoon/src/bridge.rs.h"

inline rust::String safe_rust_string(const std::string& s) {
    return rustbridge::string_from_lossy(rust::Slice<const uint8_t>(reinterpret_cast<const uint8_t*>(s.data()), s.size()));
}

#include <fstream>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <unordered_map>
#include <vector>

#include <libtorrent/session_stats.hpp>

namespace rustbridge {

// ─── shared state for the async migrations ─────────────────────────────────
// session stats and resume data are now pulled asynchronously from alerts.
// the alert loop populates these structures; reader functions drain them
// under a mutex.

namespace state {
    static std::mutex stats_mutex;
    struct StatsSnapshot {
        bool initialised = false;
        // cumulative counters from libtorrent (in bytes / per session)
        int64_t prev_recv_payload = 0;
        int64_t prev_sent_payload = 0;
        // last-snapshot wall-clock for delta calculation
        std::chrono::steady_clock::time_point prev_time;
        // last delivered rates (recomputed each alert)
        int64_t download_rate = 0;
        int64_t upload_rate = 0;
        int64_t total_download = 0;
        int64_t total_upload = 0;
        int64_t total_dht_nodes = 0;
        int32_t num_peers = 0;
    };
    static StatsSnapshot snapshot;

    // metric indices resolved once at first use
    static std::mutex metric_mutex;
    static bool metrics_resolved = false;
    static int idx_recv_payload = -1;
    static int idx_sent_payload = -1;
    static int idx_dht_nodes = -1;
    static int idx_num_peers_connected = -1;

    static std::mutex resume_mutex;
    static std::vector<rustbridge::PendingResume> pending_resume;

    static void resolve_metric_indices() {
        std::lock_guard<std::mutex> lock(metric_mutex);
        if (metrics_resolved) return;
        auto metrics = lt::session_stats_metrics();
        for (auto const &metric : metrics) {
            std::string name(metric.name);
            if (name == "net.recv_payload_bytes") idx_recv_payload = metric.value_index;
            else if (name == "net.sent_payload_bytes") idx_sent_payload = metric.value_index;
            else if (name == "dht.dht_nodes") idx_dht_nodes = metric.value_index;
            else if (name == "peer.num_peers_connected") idx_num_peers_connected = metric.value_index;
        }
        metrics_resolved = true;
    }
}

// suppress deprecated warnings for apis we keep using only inside compat
// shims (status(), write_resume_data()). new code paths use the async
// alert-based replacements above.
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"

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

    // proxy. type 0 is "none" — settings_pack still requires the field to be set.
    pack.set_int(lt::settings_pack::proxy_type, s.proxy_type);
    pack.set_str(lt::settings_pack::proxy_hostname, std::string(s.proxy_hostname));
    pack.set_int(lt::settings_pack::proxy_port, s.proxy_port);
    pack.set_str(lt::settings_pack::proxy_username, std::string(s.proxy_username));
    pack.set_str(lt::settings_pack::proxy_password, std::string(s.proxy_password));
    pack.set_bool(lt::settings_pack::proxy_peer_connections, s.proxy_peer_connections);
    pack.set_bool(lt::settings_pack::proxy_tracker_connections, s.proxy_tracker_connections);
}

// return best available info hash as a hex string (v1 preferred, v2 fallback).
// note: sha1_hash::to_string() returns the raw 20-byte digest as a std::string,
// NOT hex — feeding that into rust::String trips its utf-8 validator and aborts
// the daemon. format via ostream (operator<< prints 40 hex digits) instead.
template <typename Hash>
static std::string hash_to_hex(const Hash &h) {
    std::ostringstream oss;
    oss << h;
    return oss.str();
}

static std::string info_hash_str(const lt::info_hash_t &ih) {
    if (ih.has_v1()) return hash_to_hex(ih.v1);
    if (ih.has_v2()) return hash_to_hex(ih.v2);
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
    ts.name = safe_rust_string(st.name);
    ts.info_hash = rust::String(info_hash_str(hdl.info_hashes()).c_str());
    ts.state = rust::String(state_to_string(st.state).c_str());
    ts.save_path = safe_rust_string(st.save_path);
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
    ts.error = safe_rust_string(st.errc.message());
    ts.is_paused = bool(st.flags & lt::torrent_flags::paused);
    ts.is_finished = st.is_finished;
    ts.is_seeding = st.state == lt::torrent_status::seeding;
    ts.has_metadata = hdl.torrent_file() != nullptr;
    ts.added_time = st.added_time;
    ts.completed_time = st.completed_time;
    ts.list_peers = st.list_peers;
    ts.list_seeds = st.list_seeds;
    {
        int64_t denominator = std::max<int64_t>(static_cast<int64_t>(st.total_done), int64_t(1));
        ts.ratio = static_cast<double>(st.all_time_upload) / static_cast<double>(denominator);
    }
    ts.seeding_time = static_cast<int64_t>(
        std::chrono::duration_cast<std::chrono::seconds>(st.seeding_duration).count());
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
        tf.path = safe_rust_string(fs.file_path(i));
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
        pi.client = safe_rust_string(p.client);
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
        // ── session_stats_alert: update the snapshot, derive rates ──
        if (auto *sa = lt::alert_cast<lt::session_stats_alert>(a)) {
            state::resolve_metric_indices();
            auto counters = sa->counters();
            auto now = std::chrono::steady_clock::now();
            std::lock_guard<std::mutex> lock(state::stats_mutex);
            auto &snap = state::snapshot;
            int64_t recv_payload = (state::idx_recv_payload >= 0)
                ? counters[state::idx_recv_payload] : 0;
            int64_t sent_payload = (state::idx_sent_payload >= 0)
                ? counters[state::idx_sent_payload] : 0;
            if (snap.initialised) {
                double dt = std::chrono::duration_cast<std::chrono::duration<double>>(
                    now - snap.prev_time).count();
                if (dt > 0.0) {
                    snap.download_rate = static_cast<int64_t>(
                        (recv_payload - snap.prev_recv_payload) / dt);
                    snap.upload_rate = static_cast<int64_t>(
                        (sent_payload - snap.prev_sent_payload) / dt);
                }
            }
            snap.prev_recv_payload = recv_payload;
            snap.prev_sent_payload = sent_payload;
            snap.prev_time = now;
            snap.total_download = recv_payload;
            snap.total_upload = sent_payload;
            if (state::idx_dht_nodes >= 0)
                snap.total_dht_nodes = counters[state::idx_dht_nodes];
            if (state::idx_num_peers_connected >= 0)
                snap.num_peers = static_cast<int32_t>(counters[state::idx_num_peers_connected]);
            snap.initialised = true;
            // don't propagate stats alerts further — they're not interesting
            // to the alert log
            continue;
        }

        // ── save_resume_data_alert: stash bencoded blob for Rust pickup ──
        if (auto *sra = lt::alert_cast<lt::save_resume_data_alert>(a)) {
            try {
                auto entry = lt::write_resume_data(sra->params);
                std::vector<char> buffer;
                lt::bencode(std::back_inserter(buffer), entry);
                rustbridge::PendingResume pending;
                pending.info_hash = rust::String(info_hash_str(sra->handle.info_hashes()).c_str());
                rust::Vec<uint8_t> bytes;
                for (char c : buffer) bytes.push_back(static_cast<uint8_t>(c));
                pending.bytes = std::move(bytes);
                std::lock_guard<std::mutex> lock(state::resume_mutex);
                state::pending_resume.push_back(std::move(pending));
            } catch (...) {
                // bencoding shouldn't fail; just swallow if it does
            }
            continue;
        }

        // ── regular alerts pass through to the Rust alert log ──
        rustbridge::AlertInfo ai;
        ai.timestamp = a->timestamp().time_since_epoch().count();
        ai.message = safe_rust_string(a->message());
        ai.alert_type = safe_rust_string(a->what());
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

// ─── Session Stats (async) ────────────────────────────────────────────────
// reads from the bridge-side snapshot populated by session_stats_alert in
// bridge_pop_alerts. callers must drive bridge_session_post_stats every
// poll cycle to keep the snapshot fresh.

rustbridge::SessionStats bridge_get_session_stats(const lt::session &ses) {
    (void)ses;
    rustbridge::SessionStats ss = {};
    std::lock_guard<std::mutex> lock(state::stats_mutex);
    auto const &snap = state::snapshot;
    ss.download_rate = snap.download_rate;
    ss.upload_rate = snap.upload_rate;
    ss.total_download = snap.total_download;
    ss.total_upload = snap.total_upload;
    ss.total_dht_nodes = snap.total_dht_nodes;
    ss.num_peers = snap.num_peers;
    return ss;
}

void bridge_session_post_stats(lt::session &ses) {
    ses.post_session_stats();
}

void bridge_torrent_save_resume_data_async(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return;
    // save_info_dict embeds the .torrent metadata so resume works even if
    // the original magnet/file is gone. only_if_modified avoids spurious
    // disk i/o when nothing changed since the last save.
    hdl.save_resume_data(
        lt::torrent_handle::save_info_dict
        | lt::torrent_handle::only_if_modified);
}

rust::Vec<rustbridge::PendingResume> bridge_take_pending_resume_data() {
    rust::Vec<rustbridge::PendingResume> result;
    std::lock_guard<std::mutex> lock(state::resume_mutex);
    for (auto &item : state::pending_resume) {
        result.push_back(std::move(item));
    }
    state::pending_resume.clear();
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
        tracker.url = safe_rust_string(announce.url);
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
        tracker.message = safe_rust_string(worst_message);
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

// TODO: libtorrent 2.1 adds lt::torrent_flags::download_first_last_pieces — replace this with set_flags/unset_flags when we upgrade
void bridge_torrent_set_first_last_prio(const lt::torrent_handle &hdl, bool enabled) {
    if (!hdl.is_valid()) return;
    auto ti = hdl.torrent_file();
    if (!ti) return;
    const auto &fs = ti->files();
    auto prio = enabled ? lt::download_priority_t{7} : lt::download_priority_t{4};
    int piece_size = ti->piece_length();
    for (auto fi = lt::file_index_t{0}; fi < lt::file_index_t{fs.num_files()}; ++fi) {
        if (fs.file_size(fi) == 0) continue;
        int64_t offset = fs.file_offset(fi);
        int64_t end = offset + fs.file_size(fi) - 1;
        auto first_piece = lt::piece_index_t{static_cast<int>(offset / piece_size)};
        auto last_piece  = lt::piece_index_t{static_cast<int>(end   / piece_size)};
        hdl.piece_priority(first_piece, prio);
        if (last_piece != first_piece) hdl.piece_priority(last_piece, prio);
    }
}

void bridge_torrent_use_interface(const lt::torrent_handle &hdl, rust::Str interface) {
    if (!hdl.is_valid()) return;
    // libtorrent expects a comma-separated list, empty clears the per-torrent override
    hdl.use_interface(std::string(interface).c_str());
}

// ─── tracker add / remove ──────────────────────────────────────────────────

void bridge_torrent_add_tracker(const lt::torrent_handle &hdl, rust::Str url, int32_t tier) {
    if (!hdl.is_valid()) return;
    lt::announce_entry entry{std::string(url)};
    entry.tier = static_cast<uint8_t>(tier < 0 ? 0 : tier);
    hdl.add_tracker(entry);
}

void bridge_torrent_remove_tracker(const lt::torrent_handle &hdl, rust::Str url) {
    if (!hdl.is_valid()) return;
    std::string target(url);
    auto current = hdl.trackers();
    std::vector<lt::announce_entry> filtered;
    for (auto const &entry : current) {
        if (entry.url != target)
            filtered.push_back(entry);
    }
    hdl.replace_trackers(filtered);
}

// ─── ip filter ─────────────────────────────────────────────────────────────
// supports both PeerGuardian P2P "name:start-end" lines and plain CIDR/range
// lines. blank lines and lines starting with '#' or ';' are ignored.

static bool parse_address(const std::string &text, lt::address &out) {
    lt::error_code ec;
    out = lt::make_address(text, ec);
    return !ec;
}

int32_t bridge_session_load_ip_filter(lt::session &ses, rust::Str path) {
    std::ifstream file(std::string(path).c_str());
    if (!file.is_open()) return -1;
    lt::ip_filter filter;
    int count = 0;
    std::string line;
    while (std::getline(file, line)) {
        if (line.empty() || line[0] == '#' || line[0] == ';') continue;
        // strip CR if any (CRLF files)
        if (line.back() == '\r') line.pop_back();
        // PeerGuardian: name:start-end
        auto colon = line.rfind(':');
        std::string range = (colon != std::string::npos) ? line.substr(colon + 1) : line;
        auto dash = range.find('-');
        if (dash == std::string::npos) continue;
        std::string start_str = range.substr(0, dash);
        std::string end_str = range.substr(dash + 1);
        // trim whitespace
        auto trim = [](std::string &s) {
            while (!s.empty() && (s.front() == ' ' || s.front() == '\t')) s.erase(s.begin());
            while (!s.empty() && (s.back() == ' ' || s.back() == '\t')) s.pop_back();
        };
        trim(start_str);
        trim(end_str);
        lt::address start_addr;
        lt::address end_addr;
        if (!parse_address(start_str, start_addr)) continue;
        if (!parse_address(end_str, end_addr)) continue;
        filter.add_rule(start_addr, end_addr, lt::ip_filter::blocked);
        ++count;
    }
    if (count > 0) ses.set_ip_filter(std::move(filter));
    return count;
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

// ─── per-torrent rate limits ───────────────────────────────────────────────

void bridge_torrent_set_download_limit(const lt::torrent_handle &hdl, int32_t limit) {
    if (!hdl.is_valid()) return;
    hdl.set_download_limit(limit);
}

void bridge_torrent_set_upload_limit(const lt::torrent_handle &hdl, int32_t limit) {
    if (!hdl.is_valid()) return;
    hdl.set_upload_limit(limit);
}

int32_t bridge_torrent_download_limit(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return -1;
    return hdl.download_limit();
}

int32_t bridge_torrent_upload_limit(const lt::torrent_handle &hdl) {
    if (!hdl.is_valid()) return -1;
    return hdl.upload_limit();
}

} // namespace rustbridge
