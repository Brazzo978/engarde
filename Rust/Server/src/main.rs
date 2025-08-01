use rust_embed::RustEmbed;
use warp::http::Response;
use warp::Filter;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::{UdpSocket, TcpListener, tcp::{OwnedReadHalf, OwnedWriteHalf}}, io::{AsyncReadExt, AsyncWriteExt}, task};

use serde::Deserialize;

//
// Configurazione
//

#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Udp,
    Tcp,
}

impl Default for Mode {
    fn default() -> Self { Mode::Udp }
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
    #[serde(default, rename = "mode")]
    mode: Mode,
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
    #[allow(dead_code)]
    writer: Option<Arc<tokio::sync::Mutex<OwnedWriteHalf>>>,
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

                let sends = clients_snapshot.into_iter().map(|(key, client)| {
                    let socket = client_socket.clone();
                    let addr = client.addr;
                    let alive = now.duration_since(client.last) < client_timeout;
                    let data = buf[..n].to_vec();
                    async move {
                        let send_fut = socket.send_to(&data, addr);
                        (key, alive, tokio::time::timeout(write_timeout, send_fut).await)
                    }
                });

                let results = futures::future::join_all(sends).await;

                for (key, still_valid, result) in results {
                    if still_valid {
                        match result {
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

async fn receive_from_wireguard_tcp(
    wg_socket: Arc<UdpSocket>,
    clients: Clients,
    wg_addr: SocketAddr,
    client_timeout: Duration,
    write_timeout: Duration,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        match wg_socket.recv_from(&mut buf).await {
            Ok((n, _)) => {
                let now = Instant::now();
                let snapshot = {
                    let guard = clients.lock().unwrap();
                    guard.iter().map(|(k,v)| (k.clone(), v.clone())).collect::<Vec<_>>()
                };
                let mut to_remove = Vec::new();
                for (key, client) in snapshot {
                    if now.duration_since(client.last) >= client_timeout {
                        to_remove.push(key);
                        continue;
                    }
                    if let Some(writer) = &client.writer {
                        let data = buf[..n].to_vec();
                        let writer = writer.clone();
                        let res = tokio::time::timeout(write_timeout, async {
                            let mut w = writer.lock().await;
                            w.write_all(&(data.len() as u16).to_be_bytes()).await?;
                            w.write_all(&data).await
                        }).await;
                        match res {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => { log::warn!("Errore scrivendo al client {}: {}", key, e); to_remove.push(key); }
                            Err(_) => { log::warn!("Timeout scrivendo al client {}", key); to_remove.push(key); }
                        }
                    }
                }
                if !to_remove.is_empty() {
                    let mut guard = clients.lock().unwrap();
                    for k in to_remove { guard.remove(&k); }
                }
            }
            Err(e) => { log::warn!("Errore in recv_from Wireguard: {}", e); }
        }
    }
}

async fn handle_client_tcp_read(
    mut reader: OwnedReadHalf,
    clients: Clients,
    key: String,
    wg_socket: Arc<UdpSocket>,
    wg_addr: SocketAddr,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        let mut len_buf = [0u8;2];
        if let Err(e) = reader.read_exact(&mut len_buf).await { log::warn!("Errore lettura len da {}: {}", key, e); break; }
        let len = u16::from_be_bytes(len_buf) as usize;
        if len > buf.len() { buf.resize(len,0); }
        if let Err(e) = reader.read_exact(&mut buf[..len]).await { log::warn!("Errore lettura dati da {}: {}", key, e); break; }
        {
            let mut guard = clients.lock().unwrap();
            if let Some(c) = guard.get_mut(&key) { c.last = Instant::now(); }
        }
        if let Err(e) = wg_socket.send_to(&buf[..len], &wg_addr).await { log::warn!("Errore inoltrando a Wireguard: {}", e); }
    }
    clients.lock().unwrap().remove(&key);
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

    let wg_socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap_or_else(|e| panic!("Errore bind Wireguard socket: {}", e)),
    );
    let wg_addr: SocketAddr = server.dstAddr.parse().expect("Invalid dstAddr");

    match server.mode {
        Mode::Udp => {
            let client_socket = Arc::new(
                UdpSocket::bind(&server.listenAddr)
                    .await
                    .unwrap_or_else(|e| panic!("Errore bind client socket: {}", e)),
            );
            log::info!("Listening on {}", server.listenAddr);

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

            if let Some(web_conf) = server.webManager {
                let clients_web = clients.clone();
                tokio::spawn(async move {
                    run_webserver(web_conf, clients_web).await;
                });
            }

            let mut buf = vec![0u8; 1500];
            loop {
                match client_socket.recv_from(&mut buf).await {
                    Ok((n, src_addr)) => {
                        let key = src_addr.to_string();
                        let now = Instant::now();
                        {
                            let mut map = clients.lock().unwrap();
                            map.insert(key.clone(), ConnectedClient { addr: src_addr, last: now, writer: None });
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
        Mode::Tcp => {
            let listener = TcpListener::bind(&server.listenAddr)
                .await
                .unwrap_or_else(|e| panic!("Errore bind tcp listener: {}", e));
            log::info!("Listening (TCP) on {}", server.listenAddr);

            {
                let clients = clients.clone();
                let wg_socket = wg_socket.clone();
                tokio::spawn(async move {
                    receive_from_wireguard_tcp(
                        wg_socket,
                        clients,
                        wg_addr,
                        client_timeout,
                        write_timeout,
                    )
                    .await;
                });
            }

            if let Some(web_conf) = server.webManager {
                let clients_web = clients.clone();
                tokio::spawn(async move {
                    run_webserver(web_conf, clients_web).await;
                });
            }

            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        let key = addr.to_string();
                        let (read_half, write_half) = stream.into_split();
                        {
                            let mut map = clients.lock().unwrap();
                            map.insert(key.clone(), ConnectedClient { addr, last: Instant::now(), writer: Some(Arc::new(tokio::sync::Mutex::new(write_half))) });
                        }
                        let clients_clone = clients.clone();
                        let wg_socket_clone = wg_socket.clone();
                        tokio::spawn(async move {
                            handle_client_tcp_read(read_half, clients_clone, key, wg_socket_clone, wg_addr).await;
                        });
                    }
                    Err(e) => log::warn!("Errore accept: {}", e),
                }
            }
        }
    }
}
