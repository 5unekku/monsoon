//! pure path logic shared by the tui (rename input resolution) and the daemon
//! (content layout + folder-rename planning). no libtorrent or filesystem
//! access — just string work over `/`-separated torrent paths.
