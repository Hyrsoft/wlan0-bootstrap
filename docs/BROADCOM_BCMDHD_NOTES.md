# Broadcom bcmdhd Wi-Fi 兼容性说明

本文记录 RK3576 + Broadcom `bcmdhd`/`bcmsdh_sdmmc` 网卡在 `wlan0-bootstrap` 调试中暴露的问题，以及当前项目采用的解决措施。

## 受影响环境

已确认受影响设备：

- 板卡：Purple Pi OH2 RK3576 Board
- 兼容字段：`purplepi,oh2`、`rockchip,rk3576`
- 内核：Linux 6.1.99
- Wi-Fi 驱动：`bcmdhd`
- sysfs driver：`bcmsdh_sdmmc`
- 总线：SDIO
- 接口：`wlan0`

项目会在启动时采集设备画像。如果识别到 RK + Broadcom SDIO 网卡，会自动附加 quirk：

```text
rockchip_bcmdhd_ap_mode_reset
```

## 现象

在设备刚启动、没有进入 Soft AP 之前，手动连接目标 Wi-Fi 可以成功：

```sh
wpa_supplicant -B -dd -f /tmp/wpa.log -i wlan0 -c /tmp/wpa.conf
```

典型成功状态：

```text
wpa_state=COMPLETED
ssid=Moonlight_701
key_mgmt=WPA2-PSK
ip_address=192.168.110.133
```

但如果设备先进入 Soft AP 配网页，再从 AP 模式切回 STA 模式连接上游 Wi-Fi，可能失败：

```text
SCANNING -> ASSOCIATING -> DISCONNECTED
```

`wpa_supplicant` 日志中常见：

```text
CTRL-EVENT-ASSOC-REJECT status_code=1
CTRL-EVENT-SSID-TEMP-DISABLED reason=CONN_FAILED
```

内核/驱动日志中可能出现：

```text
wl_handle_assoc_fail : assoc fail Reason: WLC_E_SET_SSID
IAPSTA-ERROR) wl_ext_in4way_sync_sta : connect failed
```

这类失败容易被误判为密码错误，但同一个 SSID/密码在干净状态下可以手动连接成功。

## 根因判断

当前判断不是应用层密码处理问题，也不是 `wpa_supplicant` 配置文件格式问题。

复现中已经验证：

- 同一 SSID/密码在设备冷启动或接口干净状态下可以连接。
- 从 AP 模式切回 STA 后，手动 `wpa_supplicant` 也会失败。
- 执行一次接口 down/up 复位后，手动连接立即恢复成功。

因此问题更接近 Broadcom `bcmdhd` 固件/驱动在 AP/STA 模式切换后保留了部分状态，导致后续 STA 关联阶段被 AP 拒绝。

## 解决措施

当前项目对识别出的 RK + Broadcom `bcmdhd` 设备执行接口级复位：

```sh
ip link set wlan0 down
sleep <delay>
ip link set wlan0 up
sleep <delay>
```

默认延迟来自配置：

```toml
[platform]
auto_driver_quirks = true
ap_mode_reset_delay_ms = 600
```

该复位会在两个方向执行：

- 进入 Soft AP 前：避免 `hostapd` 二次启动时出现 beacon/security 参数错误。
- 从 Soft AP 切回 STA 前：避免 `wpa_supplicant` 在 `ASSOCIATING` 后被拒绝。

代码入口：

- `DeviceProfile::detect_quirks`
- `WpaCtrlBackend::apply_bcmdhd_mode_switch_reset_quirk`

## 调试流程

先确认当前设备画像：

```sh
cat /proc/device-tree/model
readlink -f /sys/class/net/wlan0/device/driver
lsmod | grep -E 'bcmdhd|dhd'
```

手动连接验证：

```sh
killall wpa_supplicant 2>/dev/null || true
ip addr flush dev wlan0
rm -f /run/wpa_supplicant/wlan0
mkdir -p /run/wpa_supplicant

cat >/tmp/wpa.conf <<'EOF'
ctrl_interface=/run/wpa_supplicant
update_config=1

network={
    ssid="Moonlight_701"
    psk="asdfghjkl123"
    key_mgmt=WPA-PSK
}
EOF

wpa_supplicant -B -dd -f /tmp/wpa.log -i wlan0 -c /tmp/wpa.conf
wpa_cli -i wlan0 status
```

如果 AP->STA 后失败，执行接口复位后再试：

```sh
killall wpa_supplicant 2>/dev/null || true
ip addr flush dev wlan0
rm -f /run/wpa_supplicant/wlan0
ip link set wlan0 down
sleep 1
ip link set wlan0 up
sleep 1
```

若复位后手动连接恢复成功，基本可以确认命中了 `bcmdhd` 模式切换状态残留问题。

## 注意事项

- 不要清理或停止 `usb0` 上的系统 `dnsmasq`，否则可能影响 ADB 调试网络。
- `wpa_passphrase` 命令不是当前项目的 STA 配网依赖；STA 配置通过 `wpa-ctrl` 或 `wpa_supplicant.conf` 交给系统 `wpa_supplicant`。
- `hostapd.conf` 中的 `wpa_passphrase=` 是 Soft AP 配置字段，不是外部命令调用。
- 该 quirk 只应对自动识别出的 RK + Broadcom `bcmdhd` 设备启用，避免影响其他 Wi-Fi 芯片。
