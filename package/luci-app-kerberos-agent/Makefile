#
# luci-app-kerberos-agent — LuCI web interface for the Kerberos.io Agent
#
# Provides a page under Services → Kerberos Agent for configuring:
#   Camera     — RTSP URL, ONVIF discovery and credentials
#   MQTT       — broker URI, username, password (for OptimACS/EMQX)
#   Storage    — Kerberos Vault URI and access credentials
#
# Dependencies:
#   luci-base       (LuCI core — view engine, CBI, rpcd glue)
#   kerberos-agent  (provides /etc/config/kerberos-agent and the daemon)
#

include $(TOPDIR)/rules.mk

PKG_NAME    := luci-app-kerberos-agent
PKG_VERSION := 0.1.0
PKG_RELEASE := 1

PKG_BUILD_DIR := $(BUILD_DIR)/$(PKG_NAME)

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-kerberos-agent
  SECTION    := luci
  CATEGORY   := LuCI
  SUBMENU    := 3. Applications
  TITLE      := LuCI Support for Kerberos.io Agent
  URL        := https://github.com/optim-enterprises-bv/ac-client
  DEPENDS    := +luci-base +kerberos-agent
  MAINTAINER := Optim Enterprises BV
endef

define Package/luci-app-kerberos-agent/description
  LuCI web interface for the Kerberos.io video surveillance agent.

  Provides a Services → Kerberos Agent page with sections for:
    Camera   — RTSP stream URL, ONVIF discovery and PTZ control
    MQTT     — OptimACS EMQX broker connection
    Storage  — Kerberos Vault recording upload

  All settings are saved to /etc/config/kerberos-agent via UCI and take
  effect after the service is restarted.
endef

# ── Build (nothing to compile — pure JS + JSON) ───────────────────────────────
define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
endef

define Build/Compile
endef

# ── Install ───────────────────────────────────────────────────────────────────
define Package/luci-app-kerberos-agent/install
	# Sidebar menu entry
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-kerberos-agent.json \
		$(1)/usr/share/luci/menu.d/luci-app-kerberos-agent.json

	# rpcd ACL — grants read+write on the 'kerberos-agent' UCI package
	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./root/usr/share/rpcd/acl.d/luci-app-kerberos-agent.json \
		$(1)/usr/share/rpcd/acl.d/luci-app-kerberos-agent.json

	# JavaScript view
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view/kerberos
	$(INSTALL_DATA) ./root/www/luci-static/resources/view/kerberos/agent.js \
		$(1)/www/luci-static/resources/view/kerberos/agent.js
endef

$(eval $(call BuildPackage,luci-app-kerberos-agent))
