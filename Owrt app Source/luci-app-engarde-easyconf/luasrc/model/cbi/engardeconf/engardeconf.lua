config = Map("engardeconf")

view = config:section(NamedSection,"Setup", "config",  translate("Engarde Configuration (cloud-init)"))
enabled = view:option(Flag, "enabled", "Enable", "Enables Engarde and Wireguard, disabling Speedify and TinyFEC VPN.<br>Wait 20-30 seconds for Engarde & UI change to commence."); view.optional=false; view.rmempty = false;
pass = view:option(Value, "pass", "Password:", "Password for Engarde Web Manager.");
pass.optional=false; pass.rmempty = false; pass.password=true;
server = view:option(Value, "dstAddr", "Server Address:",
    "IP address or hostname of the Engarde server.");
server.optional=false; server.rmempty = false;
user = view:option(Value, "username", "Username:",
    "Username for Engarde Web Manager.");
user.optional=false; user.rmempty = false;

function config.on_commit(self)
    luci.sys.exec("sh -c 'sleep 2 && /etc/init.d/engardeconf restart' &")
end

return config
