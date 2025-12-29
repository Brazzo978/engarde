use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use if_addrs::get_if_addrs;
use log::{info, warn};
use mime_guess;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::{net::UdpSocket, time};
use warp::Filter;

//
// CONFIGURAZIONE
//

#[derive(Debug, Deserialize, Clone)]
struct Config {
    client: ClientConfig,
}

#[derive(Debug, Deserialize, Clone)]
struct ClientConfig {
    #[serde(rename = "description")]
    description: Option<String>,
    #[serde(rename = "listenAddr")]
    listen_addr: String,
    #[serde(rename = "dstAddr")]
    dst_addr: String,
    #[serde(rename = "writeTimeout")]
    write_timeout: Option<u64>, // in milliseconds
    #[serde(rename = "excludedInterfaces")]
    excluded_interfaces: Vec<String>,
    #[serde(rename = "dstOverrides")]
    dst_overrides: Vec<DstOverride>,
    #[serde(rename = "webManager")]
    web_manager: Option<WebManagerConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct DstOverride {
    #[serde(rename = "ifName")]
    if_name: String,
    #[serde(rename = "dstAddr")]
    dst_addr: String,
}

#[derive(Debug, Deserialize, Clone)]
struct WebManagerConfig {
    #[serde(rename = "listenAddr")]
    listen_addr: String,
    username: String,
    password: String,
}

//
// SENDING ROUTINE (per ogni interfaccia)
//

#[derive(Clone)]
struct SendingRoutine {
    src_sock: Arc<UdpSocket>,
    src_addr: String,
    dst_addr: SocketAddr,
    last_rec: Arc<Mutex<Instant>>,
    // Campo presente per compatibilità con Go
    is_closing: Arc<Mutex<bool>>,
}

type SendingChannels = Arc<Mutex<HashMap<String, SendingRoutine>>>;

//
// Strutture per la Web API
//

#[derive(Serialize)]
struct WebInterface {
    name: String,
    status: String,
    senderAddress: String,
    dstAddress: String,
    last: Option<u64>,
}

#[derive(Serialize)]
struct GetListResponse {
    r#type: String,
    version: String,
    description: String,
    listenAddress: String,
    interfaces: Vec<WebInterface>,
}

static VERSION: &str = "0.1.0-beta02";

//
// Custom rejection per Warp
//

#[derive(Debug)]
struct CustomReject;
impl warp::reject::Reject for CustomReject {}

//
// Gestione delle esclusioni
//
static mut EXCLUSION_SWAPS: Option<Mutex<HashMap<String, bool>>> = None;

fn is_swapped(name: &str) -> bool {
    unsafe {
        if let Some(ref m) = EXCLUSION_SWAPS {
            let swaps = m.lock().unwrap();
            swaps.get(name).copied().unwrap_or(false)
        } else {
            false
        }
    }
}

fn is_excluded(name: &str, excl: &[String]) -> bool {
    for ex in excl {
        if ex == name {
            return !is_swapped(name);
        }
    }
    is_swapped(name)
}

fn swap_exclusion(ifname: &str) {
    unsafe {
        if let Some(ref m) = EXCLUSION_SWAPS {
            let mut swaps = m.lock().unwrap();
            if swaps.get(ifname).copied().unwrap_or(false) {
                swaps.remove(ifname);
            } else {
                swaps.insert(ifname.to_string(), true);
            }
        }
    }
}

fn reset_exclusions() {
    unsafe {
        if let Some(ref m) = EXCLUSION_SWAPS {
            let mut swaps = m.lock().unwrap();
            swaps.clear();
        }
    }
}

//
// Funzioni per le interfacce
//

fn get_address_by_interface(ifname: &str) -> Option<String> {
    if let Ok(ifaces) = get_if_addrs() {
        for iface in ifaces {
            if iface.name == ifname {
                if let std::net::IpAddr::V4(ipv4) = iface.ip() {
                    let ip_str = ipv4.to_string();
                    if ip_str.starts_with("169.254.") || ip_str.starts_with("127.") {
                        continue;
                    }
                    return Some(ip_str);
                }
            }
        }
    }
    None
}

fn get_dst_by_ifname(ifname: &str, cfg: &ClientConfig) -> String {
    for ov in &cfg.dst_overrides {
        if ov.if_name == ifname {
            return ov.dst_addr.clone();
        }
    }
    cfg.dst_addr.clone()
}

fn interface_exists(ifname: &str) -> bool {
    if let Ok(ifaces) = get_if_addrs() {
        for iface in ifaces {
            if iface.name == ifname {
                return true;
            }
        }
    }
    false
}

//
// Socket UDP
//

async fn create_udp_socket(source_addr: &str) -> Option<Arc<UdpSocket>> {
    let bind_addr = format!("{}:0", source_addr);
    match UdpSocket::bind(&bind_addr).await {
        Ok(sock) => Some(Arc::new(sock)),
        Err(e) => {
            warn!("Cannot create socket on {}: {}", bind_addr, e);
            None
        }
    }
}

//
// Routine per ciascuna interfaccia
//

async fn create_send_thread(
    ifname: &str,
    source_addr: &str,
    wg_sock: Arc<UdpSocket>,
    wg_addr: Arc<RwLock<Option<SocketAddr>>>,
    sending_channels: SendingChannels,
    cfg: &ClientConfig,
) {
    let dst_str = get_dst_by_ifname(ifname, cfg);
    let dst_addr: SocketAddr = match dst_str.parse() {
        Ok(addr) => addr,
        Err(e) => {
            warn!("Cannot resolve destination address {}: {}", dst_str, e);
            return;
        }
    };
    let src_sock = match create_udp_socket(source_addr).await {
        Some(s) => s,
        None => return,
    };
    let routine = SendingRoutine {
        src_sock: src_sock.clone(),
        src_addr: source_addr.to_string(),
        dst_addr,
        last_rec: Arc::new(Mutex::new(Instant::now())),
        is_closing: Arc::new(Mutex::new(false)),
    };
    let routine_clone = routine.clone();
    let ifname_owned = ifname.to_string();
    tokio::spawn(async move {
        wg_write_back(&ifname_owned, routine_clone, wg_sock, wg_addr).await;
    });
    sending_channels
        .lock()
        .unwrap()
        .insert(ifname.to_string(), routine);
}

async fn wg_write_back(
    ifname: &str,
    routine: SendingRoutine,
    wg_sock: Arc<UdpSocket>,
    wg_addr: Arc<RwLock<Option<SocketAddr>>>,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        let (n, src_addr) = match routine.src_sock.recv_from(&mut buf).await {
            Ok(res) => res,
            Err(e) => {
                warn!("Error reading from interface {}: {}", ifname, e);
                break;
            }
        };
        if src_addr != routine.dst_addr {
            warn!(
                "Ignoring packet on interface {} from unexpected source {}",
                ifname, src_addr
            );
            continue;
        }
        *routine.last_rec.lock().unwrap() = Instant::now();
        if let Some(addr) = *wg_addr.read().await {
            if let Err(e) = wg_sock.send_to(&buf[..n], addr).await {
                warn!("Error writing to WireGuard: {}", e);
            }
        }
    }
    // Qui potresti rimuovere la routine dalla mappa se necessario
}

async fn update_available_interfaces(
    wg_sock: Arc<UdpSocket>,
    wg_addr: Arc<RwLock<Option<SocketAddr>>>,
    sending_channels: SendingChannels,
    cfg: ClientConfig,
) {
    loop {
        let ifaces = get_if_addrs().unwrap_or_default();
        {
            let mut channels = sending_channels.lock().unwrap();
            let keys: Vec<String> = channels.keys().cloned().collect();
            for ifname in keys {
                if !interface_exists(&ifname) || is_excluded(&ifname, &cfg.excluded_interfaces) {
                    info!(
                        "Interface '{}' not available or excluded, removing routine",
                        ifname
                    );
                    channels.remove(&ifname);
                } else if let Some(current_ip) = get_address_by_interface(&ifname) {
                    if current_ip != channels.get(&ifname).unwrap().src_addr {
                        info!("Interface '{}' changed address, recreating routine", ifname);
                        channels.remove(&ifname);
                    }
                }
            }
        }
        for iface in ifaces {
            let ifname = iface.name;
            if is_excluded(&ifname, &cfg.excluded_interfaces) {
                continue;
            }
            if sending_channels.lock().unwrap().contains_key(&ifname) {
                continue;
            }
            if let Some(ip) = get_address_by_interface(&ifname) {
                info!("New interface '{}' with IP '{}'", ifname, ip);
                create_send_thread(
                    &ifname,
                    &ip,
                    wg_sock.clone(),
                    wg_addr.clone(),
                    sending_channels.clone(),
                    &cfg,
                )
                .await;
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn receive_from_wireguard(
    wg_sock: Arc<UdpSocket>,
    sending_channels: SendingChannels,
    wg_addr: Arc<RwLock<Option<SocketAddr>>>,
    write_timeout: Duration,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        let (n, src_addr) = match wg_sock.recv_from(&mut buf).await {
            Ok(res) => res,
            Err(e) => {
                warn!("Error reading from WireGuard: {}", e);
                continue;
            }
        };
        {
            let mut wg_addr_lock = wg_addr.write().await;
            *wg_addr_lock = Some(src_addr);
        }
        let channels_snapshot = sending_channels.lock().unwrap().clone();
        let sends = channels_snapshot.into_iter().map(|(ifname, routine)| {
            let src_sock = routine.src_sock.clone();
            let dst_addr = routine.dst_addr;
            let data = buf[..n].to_vec();
            async move {
                let fut = src_sock.send_to(&data, dst_addr);
                (ifname, tokio::time::timeout(write_timeout, fut).await)
            }
        });
        let results = futures::future::join_all(sends).await;
        for (ifname, result) in results {
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    warn!("Error writing to {}: {}", ifname, e);
                }
                Err(_) => {
                    warn!("Timeout writing to {}", ifname);
                }
            }
        }
    }
}

//
// Embedded Web Management Server
//

#[derive(RustEmbed)]
#[folder = "dist/webmanager/"]
struct Asset;

async fn serve_embedded_file(path: warp::path::Tail) -> Result<impl warp::Reply, warp::Rejection> {
    let path_str = if path.as_str().is_empty() {
        "index.html"
    } else {
        path.as_str()
    };

    if let Some(content) = Asset::get(path_str) {
        let mime = mime_guess::from_path(path_str).first_or_octet_stream();
        Ok(warp::http::Response::builder()
            .header("Content-Type", mime.as_ref())
            .body(content.data.into_owned()))
    } else {
        Err(warp::reject::not_found())
    }
}

async fn handle_get_list(
    sending_channels: SendingChannels,
    cfg: ClientConfig,
) -> Result<impl warp::Reply, warp::Rejection> {
    let now = Instant::now();
    let channels = sending_channels.lock().unwrap();
    let mut interfaces = Vec::new();
    let ifaces = get_if_addrs().unwrap_or_default();
    let mut seen = HashSet::new();
    for iface in ifaces {
        let ifname = iface.name;
        if !seen.insert(ifname.clone()) {
            continue;
        }
        let address = get_address_by_interface(&ifname).unwrap_or_else(|| "".to_string());
        let status;
        let dst = get_dst_by_ifname(&ifname, &cfg);
        let last;
        if is_excluded(&ifname, &cfg.excluded_interfaces) {
            status = "excluded".to_string();
            last = None;
        } else if let Some(routine) = channels.get(&ifname) {
            status = "active".to_string();
            let elapsed = now
                .duration_since(*routine.last_rec.lock().unwrap())
                .as_secs();
            last = Some(elapsed);
        } else {
            status = "idle".to_string();
            last = None;
        }
        interfaces.push(WebInterface {
            name: ifname,
            status,
            senderAddress: address,
            dstAddress: dst,
            last,
        });
    }
    let response = GetListResponse {
        r#type: "client".to_string(),
        version: VERSION.to_string(),
        description: cfg.description.unwrap_or_default(),
        listenAddress: cfg.listen_addr,
        interfaces,
    };
    Ok(warp::reply::json(&response))
}

async fn handle_swap_exclusion(
    body: serde_json::Value,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Some(iface) = body.get("interface").and_then(|v| v.as_str()) {
        swap_exclusion(iface);
        let resp = serde_json::json!({ "status": "ok" });
        Ok(warp::reply::json(&resp))
    } else {
        Err(warp::reject::custom(CustomReject))
    }
}

async fn handle_reset_exclusions() -> Result<impl warp::Reply, warp::Rejection> {
    reset_exclusions();
    let resp = serde_json::json!({ "status": "ok" });
    Ok(warp::reply::json(&resp))
}

async fn handle_include(body: serde_json::Value) -> Result<impl warp::Reply, warp::Rejection> {
    if let Some(iface) = body.get("interface").and_then(|v| v.as_str()) {
        if is_swapped(iface) {
            swap_exclusion(iface); // toggle to include
            let resp = serde_json::json!({ "status": "ok" });
            Ok(warp::reply::json(&resp))
        } else {
            let resp = serde_json::json!({ "status": "already-included" });
            Ok(warp::reply::json(&resp))
        }
    } else {
        Err(warp::reject::custom(CustomReject))
    }
}

async fn handle_exclude(body: serde_json::Value) -> Result<impl warp::Reply, warp::Rejection> {
    if let Some(iface) = body.get("interface").and_then(|v| v.as_str()) {
        if !is_swapped(iface) {
            swap_exclusion(iface); // toggle to exclude
            let resp = serde_json::json!({ "status": "ok" });
            Ok(warp::reply::json(&resp))
        } else {
            let resp = serde_json::json!({ "status": "already-excluded" });
            Ok(warp::reply::json(&resp))
        }
    } else {
        Err(warp::reject::custom(CustomReject))
    }
}

fn with_sending_channels(
    sending_channels: SendingChannels,
) -> impl Filter<Extract = (SendingChannels,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || sending_channels.clone())
}

fn with_client_config(
    cfg: ClientConfig,
) -> impl Filter<Extract = (ClientConfig,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || cfg.clone())
}

async fn run_webserver(
    listen_addr: &str,
    web_cfg: WebManagerConfig,
    sending_channels: SendingChannels,
    cfg: ClientConfig,
) {
    let static_route = warp::path::tail().and_then(serve_embedded_file);
    let get_list_route = warp::path!("api" / "v1" / "get-list")
        .and(with_sending_channels(sending_channels.clone()))
        .and(with_client_config(cfg.clone()))
        .and_then(handle_get_list);
    let swap_exclusion_route = warp::path!("api" / "v1" / "swap-exclusion")
        .and(warp::post())
        .and(warp::body::json())
        .and_then(handle_swap_exclusion);
    let reset_exclusions_route = warp::path!("api" / "v1" / "reset-exclusions")
        .and(warp::post())
        .and_then(handle_reset_exclusions);
    let include_route = warp::path!("api" / "v1" / "include")
        .and(warp::post())
        .and(warp::body::json())
        .and_then(handle_include);
    let exclude_route = warp::path!("api" / "v1" / "exclude")
        .and(warp::post())
        .and(warp::body::json())
        .and_then(handle_exclude);

    let routes = get_list_route
        .or(swap_exclusion_route)
        .or(reset_exclusions_route)
        .or(include_route)
        .or(exclude_route)
        .or(static_route);

    info!("Webserver (management) listening on {}", listen_addr);
    warp::serve(routes)
        .run(listen_addr.parse::<SocketAddr>().unwrap())
        .await;
}

//
// MAIN
//
#[tokio::main]
async fn main() {
    env_logger::init();

    // Legge la configurazione (default "engarde.yml")
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "engarde.yml".to_string());
    let config_str = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("Error reading {}: {}", config_path, e));
    let config: Config =
        serde_yaml::from_str(&config_str).unwrap_or_else(|e| panic!("Error parsing config: {}", e));
    let cfg = config.client.clone();

    if cfg.listen_addr.is_empty() {
        panic!("No listen_addr specified");
    }
    if cfg.dst_addr.is_empty() {
        panic!("No dst_addr specified");
    }

    let write_timeout = Duration::from_millis(cfg.write_timeout.unwrap_or(10));

    unsafe {
        EXCLUSION_SWAPS = Some(Mutex::new(HashMap::new()));
    }
    let sending_channels: SendingChannels = Arc::new(Mutex::new(HashMap::new()));

    let wg_listen_addr: SocketAddr = cfg.listen_addr.parse().expect("Invalid listen_addr");
    let wg_sock = Arc::new(
        UdpSocket::bind(wg_listen_addr)
            .await
            .expect("Error binding WireGuard socket"),
    );
    info!("Client listening on {}", cfg.listen_addr);

    let wg_addr: Arc<RwLock<Option<SocketAddr>>> = Arc::new(RwLock::new(None));

    if let Some(web) = cfg.web_manager.clone() {
        let listen = web.listen_addr.clone();
        let sending_channels_clone = sending_channels.clone();
        let cfg_clone = cfg.clone();
        tokio::spawn(async move {
            run_webserver(&listen, web, sending_channels_clone, cfg_clone).await;
        });
    }

    let sending_channels_clone = sending_channels.clone();
    let cfg_clone = cfg.clone();
    let wg_sock_clone = wg_sock.clone();
    let wg_addr_clone = wg_addr.clone();
    tokio::spawn(async move {
        update_available_interfaces(
            wg_sock_clone,
            wg_addr_clone,
            sending_channels_clone,
            cfg_clone,
        )
        .await;
    });

    receive_from_wireguard(wg_sock, sending_channels, wg_addr, write_timeout).await;
}
