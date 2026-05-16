################################################################################
#
# wlan0-bootstrap
#
################################################################################

WLAN0_BOOTSTRAP_VERSION = 0.1.0
WLAN0_BOOTSTRAP_SITE = $(TOPDIR)/../wlan0-bootstrap
WLAN0_BOOTSTRAP_SITE_METHOD = local
WLAN0_BOOTSTRAP_DEPENDENCIES = hostapd wpa_supplicant dnsmasq

WLAN0_BOOTSTRAP_CARGO_ENV = \
	CARGO_HOME=$(HOST_DIR)/share/cargo

define WLAN0_BOOTSTRAP_BUILD_CMDS
	cd $(@D) && $(WLAN0_BOOTSTRAP_CARGO_ENV) cargo build --release --target=$(RUSTC_TARGET_NAME)
endef

define WLAN0_BOOTSTRAP_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/target/$(RUSTC_TARGET_NAME)/release/wlan0-bootstrap \
		$(TARGET_DIR)/usr/bin/wlan0-bootstrap
	$(INSTALL) -D -m 0644 $(@D)/buildroot/package/wlan0-bootstrap/config.toml \
		$(TARGET_DIR)/etc/wlan0-bootstrap/config.toml
	$(INSTALL) -D -m 0755 $(@D)/buildroot/package/wlan0-bootstrap/S40wlan0-bootstrap \
		$(TARGET_DIR)/etc/init.d/S40wlan0-bootstrap
endef

$(eval $(generic-package))
