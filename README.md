# engarde - Don't lose that packet! Rust porting for better speed and performance 

[Official Facebook page](https://www.facebook.com/engarde-Dont-lose-that-packet-110039227317920)

[Official Go Version](https://github.com/porech/engarde)
## What is engarde?
engarde is a network utility specifically designed to create a point-to-point tunnel over multiple network (typically Internet) connections, ensuring that the tunnel stays up and healty without a single delay or package loss, as long as at least one of the connections is working.

## How is it possible?
engarde relies on the encryption and the de-duplication technology of the underlying WireGuard connection. It takes every UDP packet that is emitted by WireGuard and sends it through every avaliable connection. So, the first package that reaches its destination wins, and the others are silently discarded by WireGuard itself. In the same way, every response packet is sent to all the connected sockets, reaching the origin through all the connections.

## Doesn't WireGuard already support roaming between different connections?
It does, it's awesome and it's one of the things engarde relies on. WireGuard, however, sends its UDP packets over the default system interface for a specific route, usually the one used to access the Internet. If this interface goes down or loses access to the network, it's up to the operating system to detect it and change the routing table accordingly - and it doesn't always do it right.

## So, is it a failover/bonding connection mechanism?
In some way, engarde is similar to a failover mechanism, but it doesn't switch the connection when a problem occurs: this would inevitably lead to a delay in the transmission. Instead, engarde constantly sends every single packet through all the available connections: if one of the links has problems, the packet will still fastly reach its destination through the other ones, and the user won't even notice it. It's what some commercial solutions call "Redundant Mode" bonding. Moreover, failover technologies often rely on expensive hardware and hard configurations: engarde, on the other side, is totally open source and really simple to configure.

## Wait... isn't this a terrible bandwidth waste?
Absolutely yes. The used bandwidth is the one you would normally use multiplied by the number of the connections you have. But hey, imagine you are transmitting real time audio to a national radio station: would you really prefer that a connection failure causes some moments of silence to the listeners, or would you happily waste your bandwidth to avoid it?

## For the rust version they are available on the release page!
Or if you want to compile them you can just download the project and build it , the angular project for the webui is the same as the one used on the go version because i have no idea on how to modify so i am providing a compiled version of the static binaries , for the code to build it yourself check (https://github.com/porech/engarde).

## How do i use it? 

### [SETUP GUIDE WIP](Docs/Setup.md)

## How can I check if everything is working?
There is an Angular web interface embedded in both the client and the server. Please have a look to the comments in the [example config file](https://github.com/porech/engarde/blob/master/engarde.yml.sample) for more information about how to enable it.

In the client, it shows the interfaces that are currently sending data and, for each of them, the last time a packet was received from the server on it. If an interface isn't receiving data from the server, while the other are, it's probably faulty. If all of them are not receiving data, it's probably because there's no traffic on the tunnel.  
You can also exclude an interface on-the-go, but keep in mind that those changes are temporary and they're lost when the client is restarted. To make them permanent, you need to edit the configuration file.

The server interface is pretty much the same, but instead of the interfaces it shows the addresses it's currently receiving (and sending) data on.

## Does it require root?
Yes

## Can I ask for help?
Of course! Feel free to open an issue for any necessity ;)

## All credits to https://github.com/porech
For having the idea building the project and making a fully working software , this is just a port to get faster speed for gamers out there

### Logo
The engarde logo is based on the Go gopher. The Go gopher was designed by Renee French. (http://reneefrench.blogspot.com/)
The design is licensed under the Creative Commons 3.0 Attributions license.
Read this article for more details: https://blog.golang.org/gopher
