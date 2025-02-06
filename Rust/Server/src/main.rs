use rust_embed::RustEmbed;
use warp::http::Response;
use warp::Filter;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, task};

use serde::Deserialize;

//
// Configurazione
//

#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
}

#[derive(Debug, Deserialize)]
struct ServerConfig {
    description: Option<String>,
    listenAddr: String,
    dstAddr: String,
    // in millisecondi
    writeTimeout: Option<u64>,
    // in secondi
    clientTimeout: Option<u64>,
    webManager: Option<WebManagerConfig>,
}

#[derive(Debug, Deserialize)]
struct WebManagerConfig {
    listenAddr: String,
    username: String,
    password: String,
}

//
// Stato dei client
//

#[derive(Clone)]
struct ConnectedClient {
    addr: SocketAddr,
    last: Instant,
}

type Clients = Arc<Mutex<HashMap<String, ConnectedClient>>>;

//
// Embedding dei file statici
//

#[derive(RustEmbed)]
#[folder = "dist/webmanager/"]
struct Asset;

async fn serve_embedded_file(path: warp::path::Tail) -> Result<impl warp::Reply, warp::Rejection> {
    // Se il percorso è vuoto, serve index.html
    let path_str = if path.as_str().is_empty() {
        "index.html"
    } else {
        path.as_str()
    };

    if let Some(content) = Asset::get(path_str) {
        let mime = mime_guess::from_path(path_str).first_or_octet_stream();
        Ok(Response::builder()
            .header("Content-Type", mime.as_ref())
            .body(content.data.into_owned()))
    } else {
        Err(warp::reject::not_found())
    }
}

//
// Webserver
//

async fn run_webserver(web_conf: WebManagerConfig, clients: Clients) {
    // Route per i file statici embedded:
    let static_route = warp::path::tail().and_then(serve_embedded_file);

    // Route per l'API get-list:
    let clients_filter = warp::any().map(move || clients.clone());
    let get_list = warp::path!("api" / "v1" / "get-list")
        .and(clients_filter)
        .and_then(handle_get_list);

    let routes = static_route.or(get_list);

    log::info!("Webserver in ascolto su {}", web_conf.listenAddr);
    warp::serve(routes)
        .run(web_conf.listenAddr.parse::<SocketAddr>().unwrap())
        .await;
}

async fn handle_get_list(clients: Clients) -> Result<impl warp::Reply, warp::Rejection> {
    let now = Instant::now();
    let clients_guard = clients.lock().unwrap();
    let mut sockets = Vec::new();
    for (key, client) in clients_guard.iter() {
        let elapsed = now.duration_since(client.last).as_secs();
        sockets.push(serde_json::json!({
            "address": key,
            "last": elapsed,
        }));
    }
    let reply = serde_json::json!({
        "type": "server",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Engarde Server in Rust",
        "listenAddress": "", // Puoi inserire qui il valore se necessario
        "dstAddress": "",    // Puoi inserire qui il valore se necessario
        "sockets": sockets
    });
    Ok(warp::reply::json(&reply))
}

//
// UDP Server per la comunicazione
//

async fn receive_from_wireguard(
    wg_socket: Arc<UdpSocket>,
    client_socket: Arc<UdpSocket>,
    wg_addr: SocketAddr,
    clients: Clients,
    client_timeout: Duration,
    write_timeout: Duration,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        match wg_socket.recv_from(&mut buf).await {
            Ok((n, _)) => {
                let now = Instant::now();
                let mut to_remove = Vec::new();
                // Creiamo una snapshot dei client per non tenere il lock durante gli await
                let clients_snapshot = {
                    let guard = clients.lock().unwrap();
                    guard
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<Vec<_>>()
                };

                for (key, client) in clients_snapshot {
                    if now.duration_since(client.last) < client_timeout {
                        let send_fut = client_socket.send_to(&buf[..n], client.addr);
                        match tokio::time::timeout(write_timeout, send_fut).await {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                log::warn!("Errore scrivendo al client {}: {}", key, e);
                                to_remove.push(key);
                            }
                            Err(_) => {
                                log::warn!("Timeout scrivendo al client {}", key);
                                to_remove.push(key);
                            }
                        }
                    } else {
                        log::info!("Client {} timed out", key);
                        to_remove.push(key);
                    }
                }

                if !to_remove.is_empty() {
                    let mut guard = clients.lock().unwrap();
                    for key in to_remove {
                        guard.remove(&key);
                    }
                }
            }
            Err(e) => {
                log::warn!("Errore in recv_from Wireguard: {}", e);
            }
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();

    // Legge il file di configurazione (default "engarde.yml")
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "engarde.yml".to_string());
    let config_str = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("Errore leggendo {}: {}", config_path, e));
    let config: Config = serde_yaml::from_str(&config_str)
        .unwrap_or_else(|e| panic!("Errore parseando config: {}", e));

    let server = config.server;
    log::info!("Server: {:?}", server.description);

    let client_timeout = Duration::from_secs(server.clientTimeout.unwrap_or(30));
    let write_timeout = Duration::from_millis(server.writeTimeout.unwrap_or(10));

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));

    // Socket UDP per i client
    let client_socket = Arc::new(
        UdpSocket::bind(&server.listenAddr)
            .await
            .unwrap_or_else(|e| panic!("Errore bind client socket: {}", e)),
    );
    log::info!("Listening on {}", server.listenAddr);

    // Socket UDP per Wireguard (bind su "0.0.0.0:0")
    let wg_socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap_or_else(|e| panic!("Errore bind Wireguard socket: {}", e)),
    );
    let wg_addr: SocketAddr = server.dstAddr.parse().expect("Invalid dstAddr");

    // Avvia task: ricezione da Wireguard
    {
        let clients = clients.clone();
        let client_socket = client_socket.clone();
        let wg_socket = wg_socket.clone();
        tokio::spawn(async move {
            receive_from_wireguard(
                wg_socket,
                client_socket,
                wg_addr,
                clients,
                client_timeout,
                write_timeout,
            )
            .await;
        });
    }

    // Avvia il webserver se configurato
    if let Some(web_conf) = server.webManager {
        let clients_web = clients.clone();
        tokio::spawn(async move {
            run_webserver(web_conf, clients_web).await;
        });
    }

    // Loop principale: ricezione dai client e inoltro a Wireguard
    let mut buf = vec![0u8; 1500];
    loop {
        match client_socket.recv_from(&mut buf).await {
            Ok((n, src_addr)) => {
                let key = src_addr.to_string();
                let now = Instant::now();
                {
                    let mut map = clients.lock().unwrap();
                    map.insert(key.clone(), ConnectedClient { addr: src_addr, last: now });
                }
                if let Err(e) = wg_socket.send_to(&buf[..n], &wg_addr).await {
                    log::warn!("Errore inoltrando a Wireguard: {}", e);
                }
            }
            Err(e) => {
                log::warn!("Errore in recv_from client: {}", e);
            }
        }
    }
}
