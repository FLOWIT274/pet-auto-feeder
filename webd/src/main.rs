//! webd — LicheeRV Nano 后台管理系统服务入口。
//!
//! 路由一览：
//!   GET  /                       单页管理前端
//!   GET  /api/health             {ok, version}
//!   GET  /api/system             系统概览（CPU/内存/swap/磁盘/温度/负载）
//!   GET  /api/network            网卡 IP/MAC、默认网关、WiFi 模式
//!   GET  /api/logs/{name}        日志 tail（白名单：ewelink/syslog/push）
//!   GET  /api/ewelink/health     daemon 状态 + 缓存设备数
//!   GET  /api/ewelink/devices    缓存设备列表 + 逐个状态
//!   POST /api/ewelink/{on,off,status,pulse,daemon_start}
//!   GET  /api/push/health        推送凭证/bin 健康
//!   POST /api/push/{wxpush,mailpush}
//!   GET  /api/push/history       推送历史
//!   GET  /api/vision/meta        视觉可用性（调试模式标记）
//!   GET  /api/vision/latest      最新检测 JSON
//!   GET  /api/vision/frame       最新 JPEG 帧
//!   WS   /api/vision/ws          每帧检测推送
//!   GET  /api/control/status     自动控制状态（狗连续/配额/上次动作）
//!   POST /api/control/test       手动测试触发（不消耗配额）
//!   POST /api/control/reset      配额清零（测试用）

mod config;
mod control;
mod ewelink;
mod logs;
mod network;
mod push;
mod system;
mod vision;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<config::Config>,
    pub cpu: Arc<Mutex<system::CpuState>>,
    pub debug: Option<Arc<vision::DebugWriter>>,
    pub ctrl: Arc<Mutex<control::CtrlState>>,
    /// 调试模式：设备操作转日志输出（dry-run），不实际控制插座
    pub dry_run: Arc<AtomicBool>,
}

fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(api_health))
        .route("/system", get(api_system))
        .route("/network", get(api_network))
        .route(
            "/logs/{name}",
            get(|State(st): State<AppState>, AxPath(name): AxPath<String>, Query(q): Query<LogQuery>| async move {
                match logs::handler(&st.cfg, &name, q.lines.unwrap_or(100)).await {
                    Ok(v) => (StatusCode::OK, JsonWrap(v)).into_response(),
                    Err((code, msg)) => (code, JsonWrap(json!({"error": msg}))).into_response(),
                }
            }),
        )
        .route("/ewelink/health", get(api_ewelink_health))
        .route("/ewelink/devices", get(api_ewelink_devices))
        .route("/ewelink/on", post(api_ewelink_on))
        .route("/ewelink/off", post(api_ewelink_off))
        .route("/ewelink/status", post(api_ewelink_status))
        .route("/ewelink/pulse", post(api_ewelink_pulse))
        .route("/ewelink/daemon_start", post(api_ewelink_daemon_start))
        .route("/push/health", get(api_push_health))
        .route("/key/status", get(api_key_status))
        .route("/push/wxpush", post(api_push_wxpush))
        .route("/push/mailpush", post(api_push_mailpush))
        .route("/push/history", get(api_push_history))
        .route("/vision/meta", get(api_vision_meta))
        .route("/vision/latest", get(api_vision_latest))
        .route("/vision/frame", get(api_vision_frame))
        .route("/vision/stream.mjpg", get(api_vision_stream_mjpg))
        .route("/vision/ws", get(api_vision_ws))
        .route("/control/status", get(api_control_status))
        .route("/control/test", post(api_control_test))
        .route("/control/reset", post(api_control_reset))
        .route("/control/config", post(api_control_config))
        .route("/control/debug", get(api_control_debug_get).post(api_control_debug_set))
        .route("/control/debug/off", post(api_control_debug_off));

    Router::new()
        .route("/", get(index))
        .nest("/api", api)
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn api_health(State(st): State<AppState>) -> JsonWrap {
    let s = st.cfg;
    JsonWrap(json!({
        "ok": true,
        "service": "webd",
        "version": config::VERSION,
        "vision_debug": s.vision_debug,
    }))
}

async fn api_system(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(system::collect("/proc", &st.cpu).await)
}

async fn api_network(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(network::collect(&st.cfg).await)
}

async fn api_ewelink_health(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(ewelink::health(&st.cfg).await)
}

async fn api_ewelink_devices(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(ewelink::devices(&st.cfg).await)
}

async fn api_ewelink_on(State(st): State<AppState>, axum::Json(req): axum::Json<ewelink::PowerReq>) -> JsonWrap {
    if st.dry_run.load(Ordering::Relaxed) {
        return JsonWrap(json!({"ok": true, "dry_run": true, "deviceid": req.deviceid, "msg": "调试模式：模拟开插座，未操作设备"}));
    }
    JsonWrap(ewelink::power(&st.cfg, "on", req).await)
}

async fn api_ewelink_off(State(st): State<AppState>, axum::Json(req): axum::Json<ewelink::PowerReq>) -> JsonWrap {
    if st.dry_run.load(Ordering::Relaxed) {
        return JsonWrap(json!({"ok": true, "dry_run": true, "deviceid": req.deviceid, "msg": "调试模式：模拟关插座，未操作设备"}));
    }
    JsonWrap(ewelink::power(&st.cfg, "off", req).await)
}

async fn api_ewelink_status(State(st): State<AppState>, axum::Json(req): axum::Json<ewelink::PowerReq>) -> JsonWrap {
    JsonWrap(ewelink::status(&st.cfg, req).await)
}

async fn api_ewelink_pulse(State(st): State<AppState>, axum::Json(req): axum::Json<ewelink::PulseReq>) -> JsonWrap {
    if st.dry_run.load(Ordering::Relaxed) {
        return JsonWrap(json!({
            "ok": true,
            "dry_run": true,
            "deviceid": req.deviceid,
            "target_ms": req.ms,
            "outlet": req.outlet.map(|o| o.to_string()).unwrap_or_else(|| "all".into()),
            "msg": format!("调试模式：模拟插座 pulse（{}ms），未操作设备", req.ms),
        }));
    }
    JsonWrap(ewelink::pulse(&st.cfg, req).await)
}

async fn api_ewelink_daemon_start(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(ewelink::daemon_start(&st.cfg).await)
}

async fn api_push_health(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(push::health(&st.cfg).await)
}

/// 按键监视：读 key-wipe 守护的状态文件（/tmp/keywipe_state.json）
async fn api_key_status() -> JsonWrap {
    let mut v: Value = std::fs::read("/tmp/keywipe_state.json")
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_else(|| {
            json!({"pressed": false, "press_start_ms": 0, "pressed_for_ms": 0,
                   "last_wipe_ms": 0, "history": []})
        });
    // 守护进程是否在跑（pidof 按进程名匹配不到 python3 脚本，改用 pidfile + kill -0）
    let daemon = std::fs::read_to_string("/var/run/keywipe.pid")
        .ok()
        .map(|p| {
            std::process::Command::new("kill")
                .args(["-0", p.trim()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .unwrap_or(false);
    v["daemon_running"] = json!(daemon);
    // 60s 冷却倒计时（管理页显示）
    let last_wipe = v["last_wipe_ms"].as_i64().unwrap_or(0);
    if last_wipe > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        v["cooldown_left_ms"] = json!((60_000 - (now_ms - last_wipe)).max(0));
    } else {
        v["cooldown_left_ms"] = json!(0);
    }
    JsonWrap(v)
}

async fn api_push_wxpush(State(st): State<AppState>, axum::Json(req): axum::Json<push::WxPushReq>) -> JsonWrap {
    JsonWrap(push::wxpush(&st.cfg, req).await)
}

async fn api_push_mailpush(State(st): State<AppState>, axum::Json(req): axum::Json<push::MailPushReq>) -> JsonWrap {
    JsonWrap(push::mailpush(&st.cfg, req).await)
}

#[derive(Deserialize)]
struct LogQuery {
    lines: Option<usize>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    lines: Option<usize>,
}

async fn api_push_history(State(st): State<AppState>, Query(q): Query<HistoryQuery>) -> JsonWrap {
    JsonWrap(push::history(&st.cfg, q.lines.unwrap_or(20)).await)
}

async fn api_vision_meta(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(vision::meta(st.debug.clone()).await)
}

async fn api_vision_latest() -> axum::response::Response {
    vision::latest().await.into_response()
}

async fn api_vision_frame() -> axum::response::Response {
    vision::frame().await
}

async fn api_vision_stream_mjpg() -> axum::response::Response {
    vision::stream_mjpg().await
}

async fn api_vision_ws(ws: axum::extract::ws::WebSocketUpgrade) -> axum::response::Response {
    vision::ws(ws).await
}

async fn api_control_status(State(st): State<AppState>) -> JsonWrap {
    let c = st.ctrl.lock().unwrap();
    JsonWrap(c.status_json(vision::try_read().is_some()))
}

async fn api_control_test(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(control::manual_test(&st.cfg, st.ctrl.clone(), st.dry_run.load(Ordering::Relaxed)).await)
}

async fn api_control_reset(State(st): State<AppState>) -> JsonWrap {
    st.ctrl.lock().unwrap().reset_quota();
    JsonWrap(json!({"ok": true, "used_today": 0}))
}

async fn api_control_config(
    State(st): State<AppState>,
    axum::Json(req): axum::Json<control::ConfigReq>,
) -> JsonWrap {
    match st.ctrl.lock().unwrap().update_config(&req) {
        Ok(v) => JsonWrap(v),
        Err(e) => JsonWrap(json!({"ok": false, "error": e})),
    }
}

#[derive(serde::Deserialize)]
struct DebugReq {
    enabled: bool,
}

async fn api_control_debug_get(State(st): State<AppState>) -> JsonWrap {
    JsonWrap(json!({"dry_run": st.dry_run.load(Ordering::Relaxed)}))
}

async fn api_control_debug_set(State(st): State<AppState>, axum::Json(req): axum::Json<DebugReq>) -> JsonWrap {
    st.dry_run.store(req.enabled, Ordering::Relaxed);
    // 调试模式强制开启推理+视频流，方便调试（忽略配额用尽暂停）
    st.ctrl.lock().unwrap().set_force_video(req.enabled);
    let msg = if req.enabled {
        "调试模式已开启：设备操作改为日志输出，并强制开启推理+视频流"
    } else {
        "调试模式已关闭：设备操作恢复实际执行，暂停状态按配额恢复"
    };
    JsonWrap(json!({"ok": true, "dry_run": req.enabled, "msg": msg}))
}

/// 页面退出时 sendBeacon 调用（无 body）：强制关闭调试模式，防止忘记关
async fn api_control_debug_off(State(st): State<AppState>) -> JsonWrap {
    st.dry_run.store(false, Ordering::Relaxed);
    st.ctrl.lock().unwrap().set_force_video(false);
    JsonWrap(json!({"ok": true, "dry_run": false}))
}

struct JsonWrap(Value);
impl IntoResponse for JsonWrap {
    fn into_response(self) -> axum::response::Response {
        axum::Json(self.0).into_response()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cfg = Arc::new(config::Config::from_env());

    // 视觉调试模式：自建 SHM 模拟视觉数据（仅开发机；设备部署默认关闭）
    let debug = if cfg.vision_debug {
        vision::DebugWriter::start().map(Arc::new)
    } else {
        None
    };
    if cfg.vision_debug {
        println!("webd: 视觉调试模式 {}", if debug.is_some() { "已启用（自造 SHM 数据）" } else { "跳过（SHM 已存在，使用真实 visiond 数据）" });
    }

    let state = AppState {
        cfg: cfg.clone(),
        cpu: Arc::new(Mutex::new(system::CpuState::default())),
        debug,
        ctrl: Arc::new(Mutex::new(control::CtrlState::from_cfg(&cfg))),
        dry_run: Arc::new(AtomicBool::new(false)),
    };

    // 自动控制后台任务（狗连续 → 配额 → 插座 + 邮件）
    if cfg.ctrl_enabled {
        tokio::spawn(control::run(
            cfg.clone(),
            state.ctrl.clone(),
            state.dry_run.clone(),
        ));
        println!(
            "webd: 自动控制已启用 (狗≥{}s 无猫 → 配额 {}/日 {}点刷新 → 插座{}s + 邮件)",
            control::DOG_WINDOW_S, cfg.ctrl_quota, cfg.ctrl_refresh_hour, cfg.ctrl_socket_seconds
        );
    } else {
        println!("webd: 自动控制已禁用 (WEB_CTRL_ENABLED=0)");
    }

    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .unwrap_or_else(|e| panic!("webd: 监听 {} 失败: {e}", cfg.listen));
    println!("webd: {} 启动，监听 http://{} (version {})", config::VERSION, cfg.listen, config::VERSION);

    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| panic!("webd: serve 失败: {e}"));
}

/// Ctrl-C / SIGTERM 优雅退出（调试模式负责清理 SHM）
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sig = signal(SignalKind::terminate()).expect("webd: 注册 SIGTERM 失败");
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    println!("webd: 收到退出信号");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config::Config::from_env();
        cfg.home = dir.path().to_path_buf();
        cfg.boot_dir = dir.path().join("boot");
        std::fs::create_dir_all(&cfg.boot_dir).unwrap();
        cfg.wxpush_bin = dir.path().join("wxpush");
        cfg.mailpush_bin = dir.path().join("mailpush");
        cfg.wxpusher_conf = dir.path().join("wxpusher.env");
        cfg.mail_conf = dir.path().join("mail.env");
        cfg.syslog = dir.path().join("messages");
        cfg.push_log = dir.path().join("push.log");
        cfg.ctrl_state_file = dir.path().join("control.json");
        cfg.ewelink_bin = dir.path().join("ewelink-rs");
        cfg.ewelink_env = dir.path().join("ewelink.env");
        let cfg = Arc::new(cfg);
        let ctrl = Arc::new(Mutex::new(control::CtrlState::from_cfg(&cfg)));
        (
            AppState {
                cfg,
                cpu: Arc::new(Mutex::new(system::CpuState::default())),
                debug: None,
                ctrl,
                dry_run: Arc::new(AtomicBool::new(false)),
            },
            dir,
        )
    }

    async fn get_json(state: AppState, uri: &str) -> (StatusCode, Value) {
        let resp = build_router(state)
            .oneshot(axum::http::Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn root_serves_html() {
        let (state, _d) = test_state();
        let resp = build_router(state)
            .oneshot(axum::http::Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap().to_str().unwrap();
        assert!(ct.contains("text/html"));
    }

    #[tokio::test]
    async fn health_ok() {
        let (state, _d) = test_state();
        let (status, v) = get_json(state, "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn ewelink_daemon_down_reported() {
        let (state, _d) = test_state();
        let (status, v) = get_json(state, "/api/ewelink/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["daemon_running"], false);
    }

    #[tokio::test]
    async fn logs_whitelist_via_router() {
        let (state, _d) = test_state();
        let (status, v) = get_json(state.clone(), "/api/logs/ewelink?lines=5").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["exists"], false);
        let (status2, _) = get_json(state, "/api/logs/etc-passwd").await;
        assert_eq!(status2, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn push_health_without_configs() {
        let (state, _d) = test_state();
        let (status, v) = get_json(state, "/api/push/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["wxpush"]["config"]["exists"], false);
        assert_eq!(v["wxpush"]["bin"], false);
    }
}