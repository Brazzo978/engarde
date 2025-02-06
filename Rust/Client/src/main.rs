use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use if_addrs::get_if_addrs;
use serde::Deserialize;
use tokio::{net::UdpSocket, time};
use tokio::sync::RwLock;
use log::{info, warn};

// Per embeddare i file statici (la web UI)
use rust_embed::RustEmbed;
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
    // I nomi corrispondono al file YAML; usa gli attributi serde per mappare se necessario.
    #[serde(rename = "description")]
    description: Option<String>,
    #[serde(rename = "listenAddr")]
    listen_addr: String,
    #[serde(rename = "dstAddr")]
    dst_addr: String,
    #[serde(rename = "writeTimeout")]
    write_timeout: Option<u64>, // in millisecondi
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
// SENDING ROUTINE
//

#[derive(Clone)]
struct SendingRoutine {
    src_sock: Arc<UdpSocket>,
    src_addr: String,
    dst_addr: SocketAddr,
    last_rec: Arc<Mutex<Instant>>,
    is_closing: Arc<Mutex<bool>>,
}

// Gestione delle routine (chiave = nome interfaccia)
type SendingChannels = Arc<Mutex<HashMap<String, SendingRoutine>>>;

//
// Variabili globali per le esclusioni (swap)
//
static mut EXCLUSION_SWAPS: Option<Mutex<HashMap<String, bool>>> = None;

//
// FUNZIONI DI UTILITÀ
//

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

/// Restituisce la prima IPv4 "consentita" per l'interfaccia (non 127.x o 169.254.x.x)
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

/// Se esiste un override per l'interfaccia, restituisce il relativo dst_addr,
/// altrimenti restituisce il dst_addr globale.
fn get_dst_by_ifname(ifname: &str, cfg: &ClientConfig) -> String {
    for ov in &cfg.dst_overrides {
        if ov.if_name == ifname {
            return ov.dst_addr.clone();
        }
    }
    cfg.dst_addr.clone()
}

/// Controlla se l'interfaccia esiste tra quelle disponibili
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
// Creazione della socket UDP (semplice binding su "indirizzo:0")
//
async fn create_udp_socket(source_addr: &str) -> Option<Arc<UdpSocket>> {
    let bind_addr = format!("{}:0", source_addr);
    match UdpSocket::bind(&bind_addr).await {
        Ok(sock) => Some(Arc::new(sock)),
        Err(e) => {
            warn!("Non posso creare la socket su {}: {}", bind_addr, e);
            None
        }
    }
}

/// Crea la routine di invio per una nuova interfaccia
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
            warn!("Non riesco a risolvere l'indirizzo di destinazione {}: {}", dst_str, e);
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

    sending_channels.lock().unwrap().insert(ifname.to_string(), routine);
}

/// Legge dalla socket dell'interfaccia e scrive su Wireguard
async fn wg_write_back(
    ifname: &str,
    routine: SendingRoutine,
    wg_sock: Arc<UdpSocket>,
    wg_addr: Arc<RwLock<Option<SocketAddr>>>,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        let n = match routine.src_sock.recv(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                warn!("Errore in lettura dall'interfaccia {}: {}", ifname, e);
                break;
            }
        };
        *routine.last_rec.lock().unwrap() = Instant::now();
        if let Some(addr) = *wg_addr.read().await {
            if let Err(e) = wg_sock.send_to(&buf[..n], addr).await {
                warn!("Errore scrivendo a Wireguard: {}", e);
            }
        }
    }
    sending_channels_cleanup(ifname).await;
}

/// (Placeholder) Rimuove la sending routine per l'interfaccia
async fn sending_channels_cleanup(_ifname: &str) {
    // Implementa qui la rimozione dal map globale se necessario.
}

/// Aggiorna periodicamente le interfacce: crea routine per quelle nuove,
/// e termina quelle non più valide o escluse.
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
                    info!("Interfaccia '{}' non disponibile o esclusa: rimuovo routine", ifname);
                    channels.remove(&ifname);
                } else if let Some(current_ip) = get_address_by_interface(&ifname) {
                    if current_ip != channels.get(&ifname).unwrap().src_addr {
                        info!("Interfaccia '{}' ha cambiato indirizzo: ricreo routine", ifname);
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
                info!("Nuova interfaccia '{}' con IP '{}'", ifname, ip);
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

/// Riceve pacchetti dalla socket Wireguard e li inoltra a tutte le routine
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
                warn!("Errore in lettura da Wireguard: {}", e);
                continue;
            }
        };
        {
            let mut wg_addr_lock = wg_addr.write().await;
            *wg_addr_lock = Some(src_addr);
        }
        let channels_snapshot = {
            sending_channels.lock().unwrap().clone()
        };
        for (ifname, routine) in channels_snapshot {
            let send_future = routine.src_sock.send_to(&buf[..n], routine.dst_addr);
            match time::timeout(write_timeout, send_future).await {
                Ok(Ok(_)) => {},
                Ok(Err(e)) => {
                    warn!("Errore scrivendo a {}: {}", ifname, e);
                },
                Err(_) => {
                    warn!("Timeout scrivendo a {}", ifname);
                },
            }
        }
    }
}

//
// EMBEDDED WEB SERVER (management)
//

// Embedding dei file statici dalla cartella generata da Angular (ad es. dist/webmanager/)
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

/// Definisce le route del webserver di management:
/// - Serve i file statici embedded (la GUI)
/// - Puoi aggiungere endpoint API se necessario
async fn run_webserver(listen_addr: &str, _username: &str, _password: &str) {
    // Route per file statici embedati
    let static_route = warp::path::tail().and_then(serve_embedded_file);
    // (Eventuali API di management possono essere aggiunte qui, ad esempio:)
    let routes = static_route;
    info!("Webserver (management) in ascolto su {}", listen_addr);
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
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "engarde.yml".to_string());
    let config_str = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("Errore leggendo {}: {}", config_path, e));
    let config: Config = serde_yaml::from_str(&config_str)
        .unwrap_or_else(|e| panic!("Errore parseando config: {}", e));
    let cfg = config.client.clone();

    if cfg.listen_addr.is_empty() {
        panic!("Nessun listen_addr specificato");
    }
    if cfg.dst_addr.is_empty() {
        panic!("Nessun dst_addr specificato");
    }

    let write_timeout = Duration::from_millis(cfg.write_timeout.unwrap_or(10));

    // Inizializza la variabile globale per le esclusioni
    unsafe {
        EXCLUSION_SWAPS = Some(Mutex::new(HashMap::new()));
    }
    let sending_channels: SendingChannels = Arc::new(Mutex::new(HashMap::new()));

    // Crea la socket Wireguard
    let wg_listen_addr: SocketAddr = cfg.listen_addr.parse().expect("Indirizzo listen non valido");
    let wg_sock = Arc::new(
        UdpSocket::bind(wg_listen_addr)
            .await
            .expect("Errore di binding della socket Wireguard"),
    );
    info!("Client in ascolto su {}", cfg.listen_addr);

    // Variabile condivisa per l'indirizzo Wireguard (inizialmente None)
    let wg_addr: Arc<RwLock<Option<SocketAddr>>> = Arc::new(RwLock::new(None));

    // Avvia il webserver di management, embeddando i file statici
    if let Some(web) = cfg.web_manager.clone() {
        let listen = web.listen_addr.clone();
        tokio::spawn(async move {
            run_webserver(&listen, &web.username, &web.password).await;
        });
    }

    // Avvia il task per aggiornare le interfacce
    {
        let sc_clone = sending_channels.clone();
        let cfg_clone = cfg.clone();
        let wg_sock_clone = wg_sock.clone();
        let wg_addr_clone = wg_addr.clone();
        tokio::spawn(async move {
            update_available_interfaces(wg_sock_clone, wg_addr_clone, sc_clone, cfg_clone).await;
        });
    }

    // Avvia il task di ricezione da Wireguard
    receive_from_wireguard(wg_sock, sending_channels, wg_addr, write_timeout).await;
}
