// Command uvp-daemon 是 UVP GB28181 桌面模拟器的业务后端。
//
// M1 支持 --mode=once 一次性 REGISTER 验证注册闭环。
// M2 起支持 --stdio,通过 stdin/stdout JSON-lines 与 Tauri 前端 IPC。
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/device"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/ipc"
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
		stdio       = flag.Bool("stdio", false, "stdio JSON-RPC 2.0 IPC 模式 (M2, 与 --mode 互斥)")
	)
	flag.Parse()

	if *showVersion {
		fmt.Printf("uvp-daemon v%s\n", Version)
		return
	}

	initLogger(*logLevel)

	ctx, cancel := signalContext()
	defer cancel()

	// --stdio 优先,与 --mode=once 互斥。
	// M2 Tauri 场景下 daemon 以子进程方式启动,stdin=命令帧 / stdout=响应+事件 / stderr=slog。
	if *stdio {
		slog.Info("uvp-daemon start", "version", Version, "mode", "stdio")
		if err := runStdioServer(ctx); err != nil && !errors.Is(err, io.EOF) {
			slog.Error("stdio server failed", "error", err)
			os.Exit(1)
		}
		return
	}

	slog.Info("uvp-daemon start", "version", Version, "mode", *mode)

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
//
// stdio 模式下用 JSONHandler 便于 Tauri 侧 stderr 收到后转发到桌面窗口 devtools。
// once 模式仍用 TextHandler,人类可读。
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

// runStdioServer 是 M2 完整 stdio 主循环。
//
// 组装:
//   1. ipc.Server + Router (T1)
//   2. RegisterBuiltins (ping)
//   3. DeviceManager 挂 3 个业务 handler (T3),factory 里真建 sip.Transport / Client / Session
//   4. TracePublisher 桥接 sip → ipc.Server.Publish("sip_trace", ..., false) (T2)
//
// stdin EOF / ctx cancel → 优雅退出,drainAndFlush 剩余帧。
func runStdioServer(ctx context.Context) error {
	router := ipc.NewRouter()
	ipc.RegisterBuiltins(router)
	server := ipc.NewServer(router)

	dm := ipc.NewDeviceManager()
	dm.SetFactory(newProductionSessionFactory(server))
	dm.RegisterHandlers(router, server)

	return server.Run(ctx, os.Stdin, os.Stdout)
}

// tracerToIPC 把 sip.TracePublisher 事件转发到 ipc.Server.Publish。
type tracerToIPC struct {
	server *ipc.Server
}

// Emit 实现 sip.TracePublisher。sip_trace 走 bulk(允许丢弃, plan R3)。
func (t *tracerToIPC) Emit(e sip.TraceEvent) {
	t.server.Publish("sip_trace", map[string]any{
		"direction":   e.Direction,
		"method":      e.Method,
		"status_code": e.StatusCode,
		"cseq":        e.CSeq,
		"call_id":     e.CallID,
		"peer":        e.Peer,
		"summary":     e.Summary,
		"raw":         e.RawText,
	}, false)
}

// sessionAdapter 让 *device.RegistrationSession (返回 device.State) 满足 ipc.Session (返回 string)。
type sessionAdapter struct {
	inner *device.RegistrationSession
}

func (a *sessionAdapter) Start(ctx context.Context) error { return a.inner.Start(ctx) }
func (a *sessionAdapter) Stop() error                     { return a.inner.Stop() }
func (a *sessionAdapter) State() string                   { return a.inner.State().String() }
func (a *sessionAdapter) RegisteredExpires() int          { return a.inner.RegisteredExpires() }

// newProductionSessionFactory 返回生产用的 SessionFactory。
// 每次 start_device 建全新 transport / client / session,不复用(spec Q10 全期 Call-ID 已经在 session 里保证)。
func newProductionSessionFactory(server *ipc.Server) ipc.SessionFactory {
	tracer := &tracerToIPC{server: server}
	return func(params ipc.StartDeviceParams) (ipc.Session, error) {
		cfg, err := params.ToSipConfig()
		if err != nil {
			return nil, fmt.Errorf("factory: %w", err)
		}

		// 探测本机对外 IP (与 runOnce 一致的做法)
		tmpTransport, err := sip.NewTransport(sip.TransportConfig{Protocol: cfg.Transport})
		if err != nil {
			return nil, fmt.Errorf("factory: new tmp transport: %w", err)
		}
		localHost := tmpTransport.DiscoverLocalIP(fmt.Sprintf("%s:%d", cfg.ServerHost, cfg.ServerPort))
		tmpTransport.Close()

		transport, err := sip.NewTransport(sip.TransportConfig{
			Protocol:  cfg.Transport,
			LocalAddr: "0.0.0.0:0",
			LocalHost: localHost,
		})
		if err != nil {
			return nil, fmt.Errorf("factory: new transport: %w", err)
		}

		client := sip.NewClient(transport, sip.ClientConfig{
			Username: cfg.DeviceID,
			Password: cfg.Password,
			Tracer:   tracer,
		})
		session := device.NewRegistrationSession(client, cfg)
		// M3 T5: 把 heartbeat_result / renewal_result / device_state 推给前端
		session.SetPublisher(server)
		return &sessionAdapter{inner: session}, nil
	}
}

// runStdioLoop 是 T0 遗留的最小骨架,保留供 stdio_test.go 单元测试用(不含 ipc.Server)。
// 生产路径走 runStdioServer。
//
// 帧格式(plan §2.2, JSON-RPC 2.0):
//   请求: {"jsonrpc":"2.0","method":"ping","id":1}
//   响应: {"jsonrpc":"2.0","id":1,"result":{"pong":true}}
//   错误: {"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}
//
// stdin EOF → 返回 io.EOF (调用方视为正常退出)。
func runStdioLoop(ctx context.Context, stdin io.Reader, stdout io.Writer) error {
	scanner := bufio.NewScanner(stdin)
	// 单帧上限 1 MiB (Tauri 侧命令 payload 一般 <10 KiB,SIP Trace 长报文极端也 <100 KiB)。
	// bufio.Scanner 默认 64 KiB 不够。
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)

	writer := bufio.NewWriter(stdout)
	// 每写完一帧立即 Flush,避免 Tauri 侧 pipe buffer 卡帧。
	// 性能不是瓶颈(每秒最多几十帧),Flush 开销可忽略。

	for scanner.Scan() {
		if err := ctx.Err(); err != nil {
			return err
		}
		line := scanner.Bytes()
		if len(bytesTrim(line)) == 0 {
			continue // 空行忽略,前端可能加换行分隔
		}
		respBytes := handleStdioFrame(line)
		if respBytes == nil {
			continue // Notification 不回复
		}
		if _, err := writer.Write(respBytes); err != nil {
			return fmt.Errorf("write stdout: %w", err)
		}
		if err := writer.WriteByte('\n'); err != nil {
			return fmt.Errorf("write newline: %w", err)
		}
		if err := writer.Flush(); err != nil {
			return fmt.Errorf("flush stdout: %w", err)
		}
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("scan stdin: %w", err)
	}
	// stdin 关闭 (EOF) → 正常退出
	return io.EOF
}

// bytesTrim 去掉行首尾 ASCII 空白 (\r \n \t 空格) 用于空行判断。
// 内联实现避免拉 strings 包(main.go 已经很杂了)。
func bytesTrim(b []byte) []byte {
	i, j := 0, len(b)
	for i < j && isASCIISpace(b[i]) {
		i++
	}
	for j > i && isASCIISpace(b[j-1]) {
		j--
	}
	return b[i:j]
}

func isASCIISpace(c byte) bool {
	return c == ' ' || c == '\t' || c == '\r' || c == '\n'
}

// handleStdioFrame 解一帧 JSON-RPC 请求并返回响应字节 (不含换行)。
// T0 只识别 ping;其他 method 返回 -32601 method not found。
// Notification (无 id) 返回 nil,调用方跳过写 stdout。
func handleStdioFrame(line []byte) []byte {
	var req struct {
		JSONRPC string          `json:"jsonrpc"`
		Method  string          `json:"method"`
		Params  json.RawMessage `json:"params,omitempty"`
		ID      json.RawMessage `json:"id,omitempty"`
	}
	if err := json.Unmarshal(line, &req); err != nil {
		// 解析失败:按 JSON-RPC 2.0 §5.1,parse error 用 null id。
		return encodeErrorResponse(nil, -32700, "parse error: "+err.Error())
	}

	// Notification (id 缺失或为 null) — 不回响应。
	isNotification := len(req.ID) == 0 || string(req.ID) == "null"

	switch req.Method {
	case "ping":
		if isNotification {
			return nil
		}
		return encodeResultResponse(req.ID, map[string]any{"pong": true})
	default:
		if isNotification {
			return nil
		}
		return encodeErrorResponse(req.ID, -32601, "method not found: "+req.Method)
	}
}

func encodeResultResponse(id json.RawMessage, result any) []byte {
	resp := struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Result  any             `json:"result"`
	}{"2.0", id, result}
	b, err := json.Marshal(resp)
	if err != nil {
		// 极罕见:result 含 unmarshalable 类型。降级返 error。
		return encodeErrorResponse(id, -32603, "internal error: marshal result: "+err.Error())
	}
	return b
}

func encodeErrorResponse(id json.RawMessage, code int, msg string) []byte {
	if id == nil {
		id = json.RawMessage("null")
	}
	resp := struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Error   any             `json:"error"`
	}{
		JSONRPC: "2.0",
		ID:      id,
		Error: struct {
			Code    int    `json:"code"`
			Message string `json:"message"`
		}{code, msg},
	}
	b, _ := json.Marshal(resp) // struct 全为原生类型,不会失败
	return b
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
