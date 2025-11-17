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
# 这是一个简化的启动逻辑示例

# 对应configs.toml中的内容
WPA_CONF=""/etc/provisioner_wpa.conf"
IFACE="wlan0"

# 可执行文件的路径
PROVISIONER_BIN="/root/provisioner"

start() {
    echo "Checking Wi-Fi configuration..."

    # 1. 检查配置文件是否存在且非空
    if [ -f "$WPA_CONF" ] && grep -q "network={" "$WPA_CONF"; then
        echo "Valid config found. Attempting to connect..."
        
        # 启动 wpa_supplicant
        wpa_supplicant -B -i "$IFACE" -c "$WPA_CONF"
        
        # 等待关联并获取 IP 
        sleep 5
        udhcpc -i "$IFACE" -b -q
        
        # 检查是否真的连上了 
        if route -n | grep -q "^0.0.0.0"; then
            echo "Wi-Fi Connected!"
            exit 0
        else
            echo "Connection failed with existing config."
            # 失败处理：杀掉进程，准备进入配网模式
            killall wpa_supplicant
        fi
    else
        echo "No valid Wi-Fi config found."
    fi

    # 2. 如果上面的流程没能成功连接，启动配网程序
    echo "Starting Provisioning Mode..."
    # 注意：provisioner 内部已经处理了 killall wpa_supplicant 等清理工作
    $PROVISIONER_BIN &
}

stop() {
    killall provisioner # 注意可执行文件的名称
    killall wpa_supplicant
    killall udhcpc
    killall hostapd
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
        start
        ;;
    *)
        echo "Usage: $0 {start|stop|restart}"
        exit 1
esac
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