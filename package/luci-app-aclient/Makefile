#
# luci-app-aclient — LuCI web interface for the ac-client USP Agent
#
# Provides two pages under Services → OptimACS:
#   Overview      — service status, active connection info, start / stop / restart
#   Configuration — tabbed CBI form for all UCI options in /etc/config/optimacs
#
# Dependencies:
#   luci-base   (LuCI core — view engine, CBI, rpcd glue)
#   ac-client   (provides /etc/config/optimacs and the ac-client daemon)
#

include $(TOPDIR)/rules.mk

PKG_NAME    := luci-app-aclient
PKG_VERSION := 0.1.0
PKG_RELEASE := 1

PKG_BUILD_DIR := $(BUILD_DIR)/$(PKG_NAME)

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-aclient
  SECTION   := luci
  CATEGORY  := LuCI
  SUBMENU   := 3. Applications
  TITLE     := LuCI Support for OptimACS ac-client
  URL       := https://github.com/optim-enterprises-bv/ac-client
  DEPENDS   := +luci-base +ac-client
  MAINTAINER:= Optim Enterprises BV
endef

define Package/luci-app-aclient/description
  LuCI web interface for the ac-client USP Agent (TR-369 / USP 1.3).

  Provides a Services → OptimACS menu with two pages:

    Overview      — live service status (running / stopped / enabled at boot),
                    active connection settings, and start / stop / restart
                    / enable / disable buttons.

    Configuration — tabbed form covering all UCI options in /etc/config/optimacs:
                      Connection   (server host, port, MTP, URLs, controller ID)
                      TLS          (CA cert, init cert+key, provisioned cert+key)
                      Device       (MAC, USP endpoint ID, arch, model)
                      Intervals    (status heartbeat, config poll, camera scan)
                      GNSS / GPS   (serial device, baud rate)
                      Storage      (firmware dir, image dir, PID file)
                      Process      (syslog flag)

  All settings are saved to /etc/config/optimacs via UCI and take effect
  after the service is restarted.
endef

# ── Build (nothing to compile — pure JS + JSON) ───────────────────────────────
define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
endef

define Build/Compile
endef

# ── Install ───────────────────────────────────────────────────────────────────
define Package/luci-app-aclient/install
	# Sidebar menu entry
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-aclient.json \
		$(1)/usr/share/luci/menu.d/luci-app-aclient.json

	# rpcd ACL — grants read+write on the 'optimacs' UCI package and
	# read access on the 'service' and 'rc' rpcd objects.
	$(INSTALL_DIR) $(1)/usr/share/rpcd/acls.d
	$(INSTALL_DATA) ./root/usr/share/rpcd/acls.d/luci-app-aclient.json \
		$(1)/usr/share/rpcd/acls.d/luci-app-aclient.json

	# JavaScript views
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view/aclient
	$(INSTALL_DATA) ./root/www/luci-static/resources/view/aclient/overview.js \
		$(1)/www/luci-static/resources/view/aclient/overview.js
	$(INSTALL_DATA) ./root/www/luci-static/resources/view/aclient/config.js \
		$(1)/www/luci-static/resources/view/aclient/config.js
endef

$(eval $(call BuildPackage,luci-app-aclient))
