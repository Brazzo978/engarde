#Author: TALMASH

include $(TOPDIR)/rules.mk
PKG_NAME:=luci-app-engarde-easyconf
LUCI_TITLE:=Engarde Gui with configuration for owrt
LUCI_PKGARCH:=all
LUCI_DEPENDS:=+libuci
PKG_VERSION:=1.0
PKG_RELEASE:=1.0

PKG_MAINTAINER:=Brazzo978
PKG_LICENSE:=BSD
PKG_LICENSE_FILES:=LICENSE


include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
$(call BuildPackage,$(PKG_NAME))
