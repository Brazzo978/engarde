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

You should find in the root of the system the wireguard config for the client.



## Client Setup 
There will be sometimes a client setup helper script too.
