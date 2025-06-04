## Server Setup
For the server/vps part there is a script called [vps-wizard.sh](vps-wizard.sh) that does everything for you installing wireguard and engarde with minimal input needed from the user.

So simply use this command on the vps and follow the wizard. 

```bash
wget https://raw.githubusercontent.com/Brazzo978/engarde/refs/heads/master/vps-wizard.sh
bash ./vps-wizard.sh
```

The script on the vps will ask : 
what version of engard you want to use : 1 = Go (the original version of engarde with less bug) 2 = Rust (my own port on rust , faster but with less testing )

Then should suggest to the user the public ip of the vps and the interface the vps use as wan , you should press enter usually 

then it will ask the user for the wireguard tunnel subnet , defaulting to 10.0.0.0/24 that you can change if that is already in use in your network 

and lastly it will ask for the wireguard server port , you can leave 51820 since its gona be used only by engarde in this case.

Once installed if rerun the script will present to you a menù with the following options: 

      1) Show the status of Engarde service
      2) Restart Engarde service
      3) Show wireguard service status 
      4) Restart Wireguard service
      5) toggle port forwarding to Client
      6) Regenerate client config file
      7) Removes everything the script did from the system 
      8) exit from the menù

You should find in the root of the system a file named client_config.sh , we will need this file on the client before setup so copy it locally and upload to the root directory of the client before running the client installation script.



## Client Setup 
Before running the Client setup helper you are gona need some things , all the interface that are gona be used need to be with static ip .

You need to create 1 routing table foreach additional connection you want to use with engarde , so edit /etc/iproute2/rt_tables and add them , Example :

nano /etc/iproute2/rt_tables  and add the needed routing table from >

```bash
#
# reserved values
#
255     local
254     main
253     default
0       unspec
#
# local
#
#1 
```
to > 

```bash
#
# reserved values
#
255     local
254     main
253     default
0       unspec
100 wan1
200 wan2
XXX wanX
#
# local
#
#1 
```

Then on your interface configuration you need to put each interface in the corrisponding table 
we do this by editing the /etc/network/interface file this way , FROM > 
```bash

# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary WAN
allow-hotplug ens33
iface ens33 inet static
        address 10.10.10.188/24
        gateway 10.10.10.1
        dns-nameserver 1.1.1.1

# The Secondary WAN
allow-hotplug ens36
iface ens36 inet static
        address 192.168.182.80
        netmask 255.255.255.0
        gateway 192.168.107.1
```

To This adding all the part for associating an interface to its own routing table ( you can copy the rules and put your own address and interface ) > 

```bash
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
allow-hotplug ens33
iface ens33 inet static
        address 10.10.10.188/24
        gateway 10.10.10.1
        dns-nameserver 1.1.1.1


allow-hotplug ens36
iface ens36 inet static
        address 192.168.1.80

    up ip route add 192.168.1.0/24 dev ens36 src 192.168.1.80 table wan2
    up ip route add default via 192.168.1.1 dev ens36 table wan2
    up ip rule add from 192.168.1.80/32 table wan2
    up ip rule add to 192.168.1.1/32 table wan2

```
This is how the rule should be adapted: 
```bash

up ip route add "INSERTWANSUBNET/SUBNETMASK" dev "INSERTWANINTERFACENAME" src "INSERTINTERFACEIP" table "INSERTCORRESPONDINGROUTINGTABLE"
    up ip route add default via "INSERTWANGATEWAY" dev "INSERTWANINTERFACENAME" table "INSERTCORRESPONDINGROUTINGTABLE"
    up ip rule add from "INSERTINTERFACEIP/32" table "INSERTCORRESPONDINGROUTINGTABLE"
    up ip rule add to "INSERTWANGATEWAY/32" table "INSERTCORRESPONDINGROUTINGTABLE"

```

Tips: 
for optimize speed with connection with really different bandwidth (Ex link1=50MB/s link2=400MB/s)
You can try changing  "writeTimeout: 10" inside the /etc/engarde.yml file , that is the time engarde waits for all the connection to send out a packet before proceeding with the next one , lower value means less time is wasted waiting for lower link to send the packet higher value provides better link stability , default is 10ms.
You can change the default debian write and read buffers , in my case the default was 208KB i got the best result with 32MB , you can use that command to set 4MB as default 

```bash
echo "net.core.rmem_default=4194304" >> /etc/sysctl.conf
echo "net.core.rmem_max=4194304" >> /etc/sysctl.conf
echo "net.core.wmem_default=4194304" >> /etc/sysctl.conf
echo "net.core.wmem_max=4194304" >> /etc/sysctl.conf
```

Or this one to set 32MB

```bash
echo "net.core.wmem_max=33554432" >> /etc/sysctl.conf
echo "net.core.wmem_default=33554432" >> /etc/sysctl.conf
echo "net.core.rmem_max=33554432" >> /etc/sysctl.conf
echo "net.core.rmem_default=33554432" >> /etc/sysctl.conf
```
You might also try changing the txqueuelen for the wireguard interface and/or on the wan interfaces 

```bash
ip link set wg0 txqueuelen 10000

ip link set "interfacenamehere" txqueuelen 10000
```
or to make it permanent you can add this in the /etc/network/interface file 

```bash
pre-up ip link set "interfacenamehere" txqueuelen 10000

```
