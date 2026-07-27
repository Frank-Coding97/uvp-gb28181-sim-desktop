// Command uvp-daemon 是 UVP GB28181 桌面模拟器的业务后端。
//
// M1 阶段仅支持 --once 一次性 REGISTER 模式,读 JSON 配置向上级平台注册。
// 完整生命周期(心跳/续约/注销)与 stdio IPC 会在 M2/M3 加入。
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/device"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/sip"
)

// Version 由 build tag 注入,默认与 apps/daemon 语义版本对齐。
const Version = "0.2.0"

func main() {
	var (
		showVersion = flag.Bool("version", false, "打印版本号后退出")
		mode        = flag.String("mode", "once", "运行模式: once (M1) | daemon (M2+ 未实现)")
		configPath  = flag.String("config", "", "SIP 配置 JSON 文件路径 (mode=once 必填)")
		logLevel    = flag.String("log-level", "info", "日志级别: debug|info|warn|error")
	)
	flag.Parse()

	if *showVersion {
		fmt.Printf("uvp-daemon v%s\n", Version)
		return
	}

	initLogger(*logLevel)
	slog.Info("uvp-daemon start", "version", Version, "mode", *mode)

	ctx, cancel := signalContext()
	defer cancel()

	switch *mode {
	case "once":
		if err := runOnce(ctx, *configPath); err != nil {
			slog.Error("run once failed", "error", err)
			os.Exit(1)
		}
	case "daemon":
		slog.Error("daemon mode not implemented in M1 (M2+ 通过 stdio IPC 支持)")
		os.Exit(2)
	default:
		slog.Error("unknown mode", "mode", *mode)
		os.Exit(2)
	}
}

// initLogger 初始化 slog 输出到 stderr,不污染 stdio(M2 stdio 用于 Tauri IPC)。
func initLogger(level string) {
	var lvl slog.Level
	switch level {
	case "debug":
		lvl = slog.LevelDebug
	case "warn":
		lvl = slog.LevelWarn
	case "error":
		lvl = slog.LevelError
	default:
		lvl = slog.LevelInfo
	}
	handler := slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: lvl})
	slog.SetDefault(slog.New(handler))
}

// signalContext 派生一个响应 SIGINT/SIGTERM 的 context。
func signalContext() (context.Context, context.CancelFunc) {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	return ctx, cancel
}

// runOnce 读配置发一次 REGISTER 然后退出。
// 用于 M1 独立验证:向 WVP-Pro Docker 或 mock 平台注册,观察日志确认闭环。
func runOnce(ctx context.Context, configPath string) error {
	if configPath == "" {
		return fmt.Errorf("--config 必填 (mode=once)")
	}

	cfg, err := loadConfig(configPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	// 先建 transport,拿本机对外 IP 用于 Client Hostname
	tmpTransport, err := sip.NewTransport(sip.TransportConfig{Protocol: cfg.Transport})
	if err != nil {
		return fmt.Errorf("new transport (discover): %w", err)
	}
	localHost := tmpTransport.DiscoverLocalIP(fmt.Sprintf("%s:%d", cfg.ServerHost, cfg.ServerPort))
	tmpTransport.Close()

	transport, err := sip.NewTransport(sip.TransportConfig{
		Protocol:  cfg.Transport,
		LocalAddr: "0.0.0.0:0",
		LocalHost: localHost, // 只设 Hostname,不固定端口(让 sipgo 自选端口 share transaction)
	})
	if err != nil {
		return fmt.Errorf("new transport: %w", err)
	}
	defer transport.Close()

	slog.Info("local endpoint", "host", localHost, "port", "auto")

	client := sip.NewClient(transport, sip.ClientConfig{
		Username: cfg.DeviceID,
		Password: cfg.Password,
	})

	session := device.NewRegistrationSession(client, cfg)

	// 15 秒超时:注册包括 401 + Auth 一来一回,正常应该 <3 秒。
	registerCtx, registerCancel := context.WithTimeout(ctx, 15*time.Second)
	defer registerCancel()

	if err := session.Start(registerCtx); err != nil {
		return fmt.Errorf("register: %w", err)
	}

	slog.Info("register success",
		"state", session.State().String(),
		"registered_expires_secs", session.RegisteredExpires())

	// M1 就绪:注册成功即退出(不启动心跳/续约)。M3 会加长驻循环。
	return nil
}

func loadConfig(path string) (*gb28181.SipConfig, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var cfg gb28181.SipConfig
	if err := json.Unmarshal(raw, &cfg); err != nil {
		return nil, fmt.Errorf("parse json: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return nil, fmt.Errorf("validate: %w", err)
	}
	cfg.ApplyDefaults()
	return &cfg, nil
}
