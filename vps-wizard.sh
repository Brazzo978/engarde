#!/bin/bash

ENGARDE_URL="https://engarde.linuxzogno.org/builds/master/linux/amd64/engarde-server"
ENGARDE_BIN="/usr/local/bin/engarde-server"
ENGARDE_CONFIG="/etc/engarde.yml"
WIREGUARD_CONFIG="/etc/wireguard/wg0.conf"
CLIENT_CONFIG="/root/wg-client.conf"

# Check if script is run as root
if [[ $(id -u) -ne 0 ]]; then
    echo "Error: You must run this script as root!"
    exit 1
fi

# Check if OS is Debian 10 or higher
OS_VERSION=$(grep -oP '(?<=^VERSION_ID=")\d+' /etc/os-release)
if [[ "$OS_VERSION" -lt 10 ]]; then
    echo "Error: This script requires Debian 10 or higher."
    exit 1
fi

# Check if Engarde is already installed
if systemctl list-units --full -all | grep -q "engarde.service"; then
    echo "Engarde is already installed. Launching management menu..."
    manage_services
    exit 0
fi

# Install dependencies
apt update
apt install -y wireguard iproute2 wget iptables

# Detect public IPv4 address
SERVER_PUB_IP=$(ip -4 addr | sed -ne 's|^.* inet \([^/]*\)/.* scope global.*$|\1|p' | awk '{print $1}' | head -1)

# Detect public interface
SERVER_NIC=$(ip -4 route ls | grep default | grep -Po '(?<=dev )(\S+)' | head -1)

# Ask user for parameters with sanity checks
while [[ -z $SERVER_PUB_IP ]]; do
    read -rp "Public IPv4 address: " -e -i "$SERVER_PUB_IP" SERVER_PUB_IP
    [[ -n $SERVER_PUB_IP ]] || echo "Invalid input. Please enter a valid IPv4 address."
done

while [[ -z $SERVER_NIC ]]; do
    read -rp "Public interface: " -e -i "$SERVER_NIC" SERVER_NIC
    [[ -n $SERVER_NIC ]] || echo "Invalid input. Please enter a valid interface name."
done

SERVER_WG_NIC="wg0"

while [[ ! $SERVER_WG_IPV4 =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; do
    read -rp "Server WireGuard IPv4: " -e -i "10.0.0.1" SERVER_WG_IPV4
    [[ $SERVER_WG_IPV4 =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || echo "Invalid IPv4 address. Please enter a valid IPv4."
done

while [[ ! $SERVER_PORT =~ ^[0-9]+$ ]]; do
    read -rp "Server WireGuard port: " -e -i "51820" SERVER_PORT
    [[ $SERVER_PORT =~ ^[0-9]+$ ]] || echo "Invalid port. Please enter a valid port number."
done

install_wireguard() {
    mkdir -p /etc/wireguard
    wg genkey | tee /etc/wireguard/server_private.key | wg pubkey > /etc/wireguard/server_public.key
    wg genkey | tee /etc/wireguard/client_private.key | wg pubkey > /etc/wireguard/client_public.key

    SERVER_PRIVATE_KEY=$(cat /etc/wireguard/server_private.key)
    CLIENT_PRIVATE_KEY=$(cat /etc/wireguard/client_private.key)
    CLIENT_PUBLIC_KEY=$(cat /etc/wireguard/client_public.key)

    # Configure WireGuard Server
    cat > "$WIREGUARD_CONFIG" <<EOF
[Interface]
Address = $SERVER_WG_IPV4/24
ListenPort = $SERVER_PORT
PrivateKey = $SERVER_PRIVATE_KEY
PostUp = iptables -A FORWARD -i $SERVER_NIC -o $SERVER_WG_NIC -j ACCEPT; iptables -A FORWARD -i $SERVER_WG_NIC -j ACCEPT; iptables -t nat -A POSTROUTING -o $SERVER_NIC -j MASQUERADE
PostDown = iptables -D FORWARD -i $SERVER_NIC -o $SERVER_WG_NIC -j ACCEPT; iptables -D FORWARD -i $SERVER_WG_NIC -j ACCEPT; iptables -t nat -D POSTROUTING -o $SERVER_NIC -j MASQUERADE

[Peer]
PublicKey = $CLIENT_PUBLIC_KEY
AllowedIPs = 10.0.0.2/32
EOF

    chmod 600 "$WIREGUARD_CONFIG"

    # Configure WireGuard Client
    cat > "/root/wg-client.conf" <<EOF
[Interface]
PrivateKey = $CLIENT_PRIVATE_KEY
Address = 10.0.0.2/24
DNS = 1.1.1.1

[Peer]
PublicKey = $(cat /etc/wireguard/server_public.key)
Endpoint = $SERVER_PUB_IP:$SERVER_PORT
AllowedIPs = 0.0.0.0/0,::/0
PersistentKeepalive = 25
EOF

    chmod 600 "/root/wg-client.conf"

    systemctl enable wg-quick@wg0
    systemctl start wg-quick@wg0

    echo "WireGuard server is configured."
    echo "Client configuration saved at /root/wg-client.conf"
}

install_wireguard

install_engarde() {
    wget -O "$ENGARDE_BIN" "$ENGARDE_URL"
    chmod +x "$ENGARDE_BIN"

    cat > "$ENGARDE_CONFIG" <<EOF
server:
  description: "My engarde-server instance"
  listenAddr: "0.0.0.0:59501"
  dstAddr: "127.0.0.1:$SERVER_PORT"
  clientTimeout: 30
  writeTimeout: 10
  webManager:
    listenAddr: "0.0.0.0:9001"
    username: "engarde"
    password: "engarde"
EOF

    cat > /etc/systemd/system/engarde.service <<EOF
[Unit]
Description=Engarde Server
After=network.target

[Service]
ExecStart=$ENGARDE_BIN $ENGARDE_CONFIG
Restart=always
User=root

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload
    systemctl enable engarde
    systemctl start engarde
}

install_engarde

manage_services() {
    while true; do
        echo "\nOptions:"
        echo "1) Check Engarde status"
        echo "2) Restart Engarde"
        echo "3) Check WireGuard status"
        echo "4) Restart WireGuard"
        echo "5) Remove Engarde and WireGuard"
        echo "6) Exit"
        read -rp "Select an option: " option

        case $option in
            1) systemctl status engarde ;;
            2) systemctl restart engarde ;;
            3) systemctl status wg-quick@wg0 ;;
            4) systemctl restart wg-quick@wg0 ;;
            5) rm -rf /etc/wireguard /usr/local/bin/engarde-server /etc/systemd/system/engarde.service && echo "Removed Engarde and WireGuard." ;;
            6) exit 0 ;;
            *) echo "Invalid choice." ;;
        esac
    done
}

manage_services
