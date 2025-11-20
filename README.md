# Soft AP Wi-Fi Provisioner

一个轻量级的 Wi-Fi 配网程序，通过启动一个临时的 Soft AP 和 Web 界面，来为嵌入式 Linux 设备配置 `wpa_supplicant`。

## 使用说明

### 交叉编译 

本项目依赖 `cross` 进行交叉编译。

```bash
# 例如，编译为 armv7 musleabihf 目标 (静态链接)
cross build \
   --target=armv7-unknown-linux-musleabihf \
   --release \
   --config 'target.armv7-unknown-linux-musleabihf.rustflags=["-C", "target-feature=+crt-static"]'
```
如果要开启音频播报功能，加上`--features "audio"`

### 运行调试 

直接运行编译好的二进制文件：

```bash
./provisioner
```

如果需要显示详细的调试日志：

```bash
RUST_LOG="debug,tower_http=debug" ./provisioner
```

### 开机自启

仅供参考：当开机后，如果检测到已经有wpa_supplicant.conf这类配置文件，就利用它尝试连接Wi-Fi，如果不存在或者链接失败，就启动本项目的可执行文件进入配网流程。

设置 `/etc/init.d/S99wifi_check`
```bash
#!/bin/sh
# /etc/init.d/S99wifi_check
# Optimized by Nexus for Luckfox embedded environment

# --- 配置区域 ---
WPA_CONF="/etc/provisioner_wpa.conf"
IFACE="wlan0"
# 建议使用绝对路径，防止 PATH 未就绪
PROVISIONER_BIN="/root/provisioner/provisioner"
# 日志文件，用于重启后排查问题
LOG_FILE="/var/log/wifi_check.log"

# --- 辅助函数 ---
log_msg() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') - $1" | tee -a "$LOG_FILE"
}

start() {
    log_msg "Starting Wi-Fi check service..."

    # 0. 确保接口已启动
    ip link set "$IFACE" up
    sleep 1

    # 1. 检查配置文件是否存在且包含有效网络定义
    if [ -f "$WPA_CONF" ] && grep -q "network={" "$WPA_CONF"; then
        log_msg "Valid config found ($WPA_CONF). Attempting to connect..."
        
        # 启动 wpa_supplicant
        # -B: 后台运行
        wpa_supplicant -B -i "$IFACE" -c "$WPA_CONF"
        if [ $? -ne 0 ]; then
            log_msg "Error: Failed to start wpa_supplicant."
            fallback_to_provision
            return
        fi
        
        # 给予 wpa_supplicant 一点时间进行关联
        log_msg "Waiting for Wi-Fi association..."
        # 循环检查连接状态，最多等待 10 秒
        cnt=0
        while [ $cnt -lt 10 ]; do
            # 使用 wpa_cli 检查状态更准确，如果没有 wpa_cli，可以用 iwconfig
            status=$(wpa_cli -i "$IFACE" status 2>/dev/null | grep "wpa_state=COMPLETED")
            if [ -n "$status" ]; then
                log_msg "Wi-Fi Associated! Requesting IP..."
                break
            fi
            sleep 1
            cnt=$((cnt+1))
        done

        # 请求 DHCP
        # -i: 接口
        # -n: 如果获取失败立刻退出 (now)，不转入后台，方便我们判断成败
        # -q: 安静模式
        # -t 5: 重试 5 次
        udhcpc -i "$IFACE" -n -t 5 -q
        
        # 检查是否获取到 IP (通过 ip addr 检查 global 地址)
        if ip addr show "$IFACE" | grep -q "inet .* global"; then
            log_msg "SUCCESS: Wi-Fi Connected and IP obtained."
            return 0
        else
            log_msg "FAILURE: Association or DHCP failed."
            # 清理环境
            killall wpa_supplicant 2>/dev/null
            killall udhcpc 2>/dev/null
        fi
    else
        log_msg "No valid Wi-Fi config found."
    fi

    # 2. 进入失败/配网模式
    fallback_to_provision
}

fallback_to_provision() {
    log_msg "Starting Provisioning Mode..."
    
    # 再次确保清理
    killall wpa_supplicant 2>/dev/null
    
    if [ -x "$PROVISIONER_BIN" ]; then
        # 将配网程序的输出重定向，防止堵塞 init 进程，也便于调试
        "$PROVISIONER_BIN" >> "$LOG_FILE" 2>&1 &
        log_msg "Provisioner started with PID $!"
    else
        log_msg "CRITICAL: Provisioner binary not found or not executable at $PROVISIONER_BIN"
    fi
}

stop() {
    log_msg "Stopping Wi-Fi services..."
    # 这里的名字要和实际运行的进程名一致
    killall provisioner 2>/dev/null 
    killall wpa_supplicant 2>/dev/null
    killall udhcpc 2>/dev/null
}

case "$1" in
    start)
        start
        ;;
    stop)
        stop
        ;;
    restart)
        stop
        sleep 1
        start
        ;;
    *)
        echo "Usage: $0 {start|stop|restart}"
        exit 1
esac
```
给予可执行权限
```bash
chmod +x /etc/init.d/S99wifi_check
```
手动执行进行测试
```bash
# 先停止当前可能存在的进程
/etc/init.d/S99wifi_check stop

# 手动运行 start
/etc/init.d/S99wifi_check start
```

## 当前实现的功能

1.  启动 Soft AP 热点
2.  扫描 Wi-Fi
3.  启动 WebServer
4.  连接 Wi-Fi
5.  执行dhcp从路由器获取ip

本项目**不会**插手 Wi-Fi 自动连接、配网触发时机等应由操作系统或上层应用处理的事务。

## 待实现清单 (Roadmap)

  * [x] 为 Wi-Fi 自动连接（持久化）提供配置选项。
  * [x] 添加可选的配网过程语音播报。
  * [ ] 减少对系统shell命令的依赖，不再依赖hostapd和dnsmsaq这两个系统工具

-----