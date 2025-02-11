#!/bin/bash

ENGARDE_GO_URL="https://engarde.linuxzogno.org/builds/master/linux/amd64/engarde-server"
ENGARDE_RUST_URL="https://github.com/Brazzo978/engarde/releases/download/0.0.1/engarde_server"
ENGARDE_BIN="/usr/local/bin/engarde-server"
ENGARDE_CONFIG="/etc/engarde.yml"
WIREGUARD_CONFIG="/etc/wireguard/wg0.conf"
CLIENT_CONFIG="/root/wg-client.conf"

# Check if the script is run as root
if [[ $(id -u) -ne 0 ]]; then
    echo "Error: You must run this script as root!"
    exit 1
fi

# Check if the OS is Debian 10 or higher
OS_VERSION=$(grep -oP '(?<=^VERSION_ID=")\d+' /etc/os-release)
if [[ "$OS_VERSION" -lt 10 ]]; then
    echo "Error: This script requires Debian 10 or higher."
    exit 1
fi

# Ask the user which version of Engarde to install
while true; do
    read -rp "Which version of Engarde do you want to install? (1 = Go [Stable], 2 = Rust [Performance]): " ENGARDE_VERSION
    case $ENGARDE_VERSION in
        1) ENGARDE_URL=$ENGARDE_GO_URL; break ;;
        2) ENGARDE_URL=$ENGARDE_RUST_URL; break ;;
        *) echo "Invalid choice. Please enter 1 or 2." ;;
    esac
done

manage_services() {
    while true; do
        echo -e "\nOptions:"
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
            5) rm -rf /etc/wireguard /usr/local/bin/engarde-server /etc/systemd/system/engarde.service && echo "Engarde and WireGuard have been removed." ;;
            6) exit 0 ;;
            *) echo "Invalid choice." ;;
        esac
    done
}

# If Engarde is already installed, launch the management menu
if systemctl list-units --full -all | grep -q "engarde.service"; then
    echo "Engarde is already installed. Launching the management menu..."
    manage_services
    exit 0
fi

# Install dependencies
apt update
apt install -y wireguard iproute2 wget iptables

# Detect public IPv4 address and public network interface
SERVER_PUB_IP=$(ip -4 addr | sed -ne 's|^.* inet \([^/]*\)/.* scope global.*$|\1|p' | awk '{print $1}' | head -1)
SERVER_NIC=$(ip -4 route ls | grep default | grep -Po '(?<=dev )(\S+)' | head -1)

# Prompt user for missing parameters
while [[ -z $SERVER_PUB_IP ]]; do
    read -rp "Public IPv4 address: " -e -i "$SERVER_PUB_IP" SERVER_PUB_IP
    [[ -n $SERVER_PUB_IP ]] || echo "Invalid input. Please enter a valid IPv4 address."
done

while [[ -z $SERVER_NIC ]]; do
    read -rp "Public network interface: " -e -i "$SERVER_NIC" SERVER_NIC
    [[ -n $SERVER_NIC ]] || echo "Invalid input. Please enter a valid interface name."
done

SERVER_WG_NIC="wg0"

while [[ ! $SERVER_WG_IPV4 =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; do
    read -rp "WireGuard server IPv4 address: " -e -i "10.0.0.1" SERVER_WG_IPV4
    [[ $SERVER_WG_IPV4 =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || echo "Invalid IPv4 address. Please enter a valid IPv4 address."
done

# Set the WireGuard server port in the range 65500-65535, excluding 65522 (reserved for SSH)
while [[ ! $SERVER_PORT =~ ^[0-9]+$ ]] || [[ $SERVER_PORT -lt 65500 || $SERVER_PORT -gt 65535 || $SERVER_PORT -eq 65522 ]]; do
    read -rp "WireGuard server port (65500-65535, excluding 65522): " -e -i "65500" SERVER_PORT
    if [[ ! $SERVER_PORT =~ ^[0-9]+$ ]] || [[ $SERVER_PORT -lt 65500 || $SERVER_PORT -gt 65535 || $SERVER_PORT -eq 65522 ]]; then
        echo "Invalid port. It must be between 65500 and 65535 and cannot be 65522."
    fi
done

install_wireguard() {
    mkdir -p /etc/wireguard
    wg genkey | tee /etc/wireguard/server_private.key | wg pubkey > /etc/wireguard/server_public.key
    wg genkey | tee /etc/wireguard/client_private.key | wg pubkey > /etc/wireguard/client_public.key

    SERVER_PRIVATE_KEY=$(cat /etc/wireguard/server_private.key)
    CLIENT_PRIVATE_KEY=$(cat /etc/wireguard/client_private.key)
    CLIENT_PUBLIC_KEY=$(cat /etc/wireguard/client_public.key)

    # Enable IPv4 and IPv6 forwarding
    echo 'net.ipv4.ip_forward = 1' | tee -a /etc/sysctl.conf
    echo 'net.ipv6.conf.all.forwarding = 1' | tee -a /etc/sysctl.conf
    sysctl -p /etc/sysctl.conf

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

    echo "WireGuard server configured on port $SERVER_PORT."
    echo "Client configuration saved at /root/wg-client.conf"
    echo "IPv4 and IPv6 forwarding enabled."
}

install_wireguard

install_engarde() {
    wget -O "$ENGARDE_BIN" "$ENGARDE_URL"
    chmod +x "$ENGARDE_BIN"

    cat > "$ENGARDE_CONFIG" <<EOF
server:
  description: "Engarde Server Instance"
  # Engarde listens on port 65501 (within the range 65500-65535)
  listenAddr: "0.0.0.0:65501"
  # Forward traffic to WireGuard running on port $SERVER_PORT
  dstAddr: "127.0.0.1:$SERVER_PORT"
  clientTimeout: 30
  writeTimeout: 10
  webManager:
    # Web GUI on port 65502
    listenAddr: "0.0.0.0:65502"
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

# Change SSH port from 22 to 65522 if necessary
CURRENT_SSH_PORT=$(grep -E '^\s*Port\s+' /etc/ssh/sshd_config | awk '{print $2}' | head -1)
if [[ -z $CURRENT_SSH_PORT ]]; then
    # If no Port directive is found, assume default port 22
    CURRENT_SSH_PORT=22
fi

if [[ $CURRENT_SSH_PORT -ne 65522 ]]; then
    read -p "Warning! SSH port will be changed from $CURRENT_SSH_PORT to 65522. Proceed? (y/n): " confirm
    if [[ $confirm == [yY] ]]; then
        # Replace both commented and uncommented Port lines
        sed -i -E 's/^\s*#?\s*Port\s+[0-9]+/Port 65522/' /etc/ssh/sshd_config
        echo "SSH port changed to 65522."
        # Restart the SSH service (tries both service names)
        systemctl restart ssh || systemctl restart sshd
    else
        echo "SSH port change cancelled."
    fi
fi

manage_services
