//! eWeLink v2 HTTP API 客户端
//!
//! 基于官方《接口中心_v2》《开发文档_v2》《OAuth2.0》实现：
//! - 登录：付费 APPID 用 POST /v2/user/login（Sign 签名，10004 自动区域重定向）；
//!   免费 APPID（407 = appid 无操作权限）自动切换 OAuth2.0 授权码流程
//! - OAuth2.0：POST apia.coolkit.cn/v2/user/oauth/code（账号密码 -> code，30s 有效）
//!   -> POST {region}/v2/user/oauth/token（code -> accessToken/refreshToken）
//! - 刷新 POST /v2/user/refresh
//! - 设备列表 GET /v2/device/thing、状态查询/控制 /v2/device/thing/status

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::models::{
    ApiResponse, Device, FamilyData, LoginData, OAuthCodeData, OAuthTokenData, RefreshData,
    StatusData, ThingListData,
};
use crate::sign::{make_sign, nonce, now_secs, sequence};

type HmacSha256 = Hmac<Sha256>;

/// 账号与凭证配置（来自命令行参数/环境变量）
pub struct Config {
    pub appid: String,
    pub secret: String,
    /// 邮箱 或 手机号（不带区号）
    pub account: String,
    pub password: String,
    /// 电话区号，必须以 "+" 开头，如 "+86"
    pub country_code: String,
    /// 初始区域 cn/as/us/eu；登录返回 10004 时会自动切换到正确区域
    pub region: String,
    /// OAuth2.0 回调地址（免费 APPID 必需），须与 dev.ewelink.cc 应用管理的跳转地址一致
    pub redirect_url: String,
}

pub struct Client {
    pub config: Config,
    pub at: Option<String>,
    pub rt: Option<String>,
    /// 用户 apikey（= 用户 ID），WS 握手与控制指令需要
    pub apikey: Option<String>,
    /// at / rt 过期时刻（秒，unix），用于常驻进程免重复登录
    at_exp: Option<i64>,
    rt_exp: Option<i64>,
    http: reqwest::Client,
}

impl Client {
    pub fn new(config: Config) -> Result<Self> {
        let mut c = Self {
            config,
            at: None,
            rt: None,
            apikey: None,
            at_exp: None,
            rt_exp: None,
            http: reqwest::Client::builder()
                .user_agent("ewelink-rs/0.1")
                .build()?,
        };
        // 复用已落盘的 token（daemon/CLI/ws 共享，避免互相顶掉）
        let _ = c.load_tokens(&token_path());
        Ok(c)
    }

    /// 从环境变量构造配置（daemon 进程复用：S90ewelink 已 source /etc/ewelink.env）
    pub fn from_env() -> Result<Self> {
        let c = Config {
            appid: env("EWELINK_APPID")?,
            secret: env("EWELINK_APPSECRET")?,
            account: env("EWELINK_ACCOUNT")?,
            password: env("EWELINK_PASSWORD")?,
            country_code: std::env::var("EWELINK_COUNTRY_CODE").unwrap_or_else(|_| "+86".into()),
            region: std::env::var("EWELINK_REGION").unwrap_or_else(|_| "as".into()),
            redirect_url: std::env::var("EWELINK_REDIRECT_URL")
                .unwrap_or_else(|_| "https://web.ewelink.cc".into()),
        };
        Self::new(c)
    }

    /// token 落盘文件（HOME/.ewelink-tokens.json）
    fn save_tokens(&self) {
        let path = token_path();
        let v = json!({
            "at": self.at, "rt": self.rt, "apikey": self.apikey,
            "at_exp": self.at_exp, "rt_exp": self.rt_exp,
            "region": self.config.region,
        });
        if let Ok(s) = serde_json::to_string(&v) {
            let _ = std::fs::write(path, s);
        }
    }

    fn load_tokens(&mut self, path: &str) -> Result<()> {
        let raw = std::fs::read_to_string(path)?;
        let v: Value = serde_json::from_str(&raw)?;
        self.at = v.get("at").and_then(|x| x.as_str()).map(String::from);
        self.rt = v.get("rt").and_then(|x| x.as_str()).map(String::from);
        self.apikey = v.get("apikey").and_then(|x| x.as_str()).map(String::from);
        self.at_exp = v.get("at_exp").and_then(|x| x.as_i64());
        self.rt_exp = v.get("rt_exp").and_then(|x| x.as_i64());
        if let Some(r) = v.get("region").and_then(|x| x.as_str()) {
            self.config.region = r.to_string();
        }
        Ok(())
    }

    /// 确保 at 有效：at 未过期直接复用（daemon 常驻省 OAuth 配额）；
    /// at 过期且 rt 有效 → refresh；否则重新登录。
    pub async fn ensure_auth(&mut self) -> Result<()> {
        let now = now_secs() as i64;
        let at_ok = self.at.is_some()
            && self
                .at_exp
                .map(|exp| exp > now + 60)
                .unwrap_or(true);
        if at_ok {
            return Ok(());
        }
        let rt_ok = self.rt.is_some()
            && self
                .rt_exp
                .map(|exp| exp > now + 60)
                .unwrap_or(true);
        if rt_ok {
            return self.refresh().await;
        }
        self.login().await
    }

    /// 区域 -> 接口域名（《接口中心_v2》）
    pub fn region_host(region: &str) -> &'static str {
        match region {
            "cn" => "https://cn-apia.coolkit.cn",
            "as" => "https://as-apia.coolkit.cc",
            "us" => "https://us-apia.coolkit.cc",
            "eu" => "https://eu-apia.coolkit.cc",
            _ => "https://as-apia.coolkit.cc",
        }
    }

    /// 未登录接口（用户分类）的请求头：Sign 签名（标准 v2 签名 = HMAC(secret, body 原文)）
    fn signed_headers(&self, payload: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "X-CK-Appid",
            HeaderValue::from_str(&self.config.appid).expect("appid"),
        );
        h.insert("X-CK-Nonce", HeaderValue::from_str(&nonce()).expect("nonce"));
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Sign {}",
                make_sign(&self.config.secret, payload)
            ))
            .expect("sign"),
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h
    }

    /// OAuth2.0 签名：HMAC(secret, "{clientId}_{seq}") 的 base64（OAuth2.0 文档「授权页说明」）
    fn oauth_signature(&self, seq: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(self.config.secret.as_bytes()).expect("hmac key");
        mac.update(format!("{}_{}", self.config.appid, seq).as_bytes());
        B64.encode(mac.finalize().into_bytes())
    }

    /// oauth/code 请求头：额外带 X-CK-Seq，Authorization 用 OAuth 签名
    fn oauth_headers(&self, seq: &str, sign: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "X-CK-Appid",
            HeaderValue::from_str(&self.config.appid).expect("appid"),
        );
        h.insert("X-CK-Nonce", HeaderValue::from_str(&nonce()).expect("nonce"));
        h.insert("X-CK-Seq", HeaderValue::from_str(seq).expect("seq"));
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Sign {sign}")).expect("sign"),
        );
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        h
    }

    /// 登录后接口的请求头：Bearer at
    fn authed_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "X-CK-Appid",
            HeaderValue::from_str(&self.config.appid).expect("appid"),
        );
        h.insert("X-CK-Nonce", HeaderValue::from_str(&nonce()).expect("nonce"));
        if let Some(at) = &self.at {
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {at}")).expect("at"),
            );
        }
        h
    }

    /// POST + Sign（用户类接口）；body 原文既作请求体也作签名输入
    async fn post_signed(&self, region: &str, path: &str, body: &Value) -> Result<ApiResponse<Value>> {
        let payload = body.to_string();
        let url = format!("{}{}", Self::region_host(region), path);
        let resp = self
            .http
            .post(&url)
            .headers(self.signed_headers(&payload))
            .body(payload)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| format!("解析响应失败: {text}"))
    }

    /// POST + Bearer（设备类接口）
    async fn post_authed(&self, path: &str, body: &Value) -> Result<ApiResponse<Value>> {
        let url = format!("{}{}", Self::region_host(&self.config.region), path);
        let resp = self
            .http
            .post(&url)
            .headers(self.authed_headers())
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| format!("解析响应失败: {text}"))
    }

    /// GET + Bearer（设备类接口）
    async fn get_authed(&self, path: &str, query: &str) -> Result<ApiResponse<Value>> {
        let url = format!("{}{}?{}", Self::region_host(&self.config.region), path, query);
        let resp = self
            .http
            .get(&url)
            .headers(self.authed_headers())
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| format!("解析响应失败: {text}"))
    }

    fn parse<T: DeserializeOwned>(&self, resp: ApiResponse<Value>, what: &str) -> Result<T> {
        if resp.error != 0 {
            bail!("{what}失败: error={} msg={}", resp.error, resp.msg);
        }
        let data = resp.data.context("响应缺少 data")?;
        serde_json::from_value(data).with_context(|| format!("{what}响应格式异常"))
    }

    /// 登录：优先账号密码登录（付费 APPID），407（appid 无操作权限）时自动切换 OAuth2.0。
    /// 幂等：本地已有未过期 at 时直接复用（daemon/CLI 共享 token，避免并发登录互相顶掉）。
    pub async fn login(&mut self) -> Result<()> {
        let now = now_secs() as i64;
        if self.at.is_some()
            && self
                .at_exp
                .map(|exp| exp > now + 60)
                .unwrap_or(true)
        {
            return Ok(());
        }
        let body = if self.config.account.contains('@') {
            json!({
                "email": self.config.account,
                "password": self.config.password,
                "countryCode": self.config.country_code,
            })
        } else {
            json!({
                "phoneNumber": self.config.account,
                "password": self.config.password,
                "countryCode": self.config.country_code,
            })
        };
        match self.login_at(self.config.region.clone(), &body).await {
            Err(e) if format!("{e}").contains("error=407") => {
                eprintln!("该 APPID 无 /v2/user/login 权限（407），改用 OAuth2.0 授权码登录");
                self.login_oauth().await
            }
            r => r,
        }
    }

    async fn login_at(&mut self, mut region: String, body: &Value) -> Result<()> {
        loop {
            let resp = self.post_signed(&region, "/v2/user/login", body).await?;
            if resp.error == 10004 {
                // 账号不在本区域：服务端返回 data.region，换区域重试
                let new_region = resp
                    .data
                    .as_ref()
                    .and_then(|d| d["region"].as_str())
                    .unwrap_or("as")
                    .to_string();
                eprintln!("账号不在 {region} 区域，重定向到 {new_region}");
                region = new_region;
                continue;
            }
            if resp.error != 0 {
                bail!("登录失败: error={} msg={}", resp.error, resp.msg);
            }
            let data: LoginData = serde_json::from_value(resp.data.context("登录响应缺少 data")?)
                .context("登录响应格式异常")?;
            self.at = Some(data.at);
            self.rt = Some(data.rt);
            self.at_exp = data.atExpiredTime;
            self.rt_exp = data.rtExpiredTime;
            self.apikey = Some(data.user.apikey);
            self.config.region = data.region;
            self.save_tokens();
            println!(
                "登录成功: region={} apikey={}",
                self.config.region,
                self.apikey.as_deref().unwrap_or("")
            );
            return Ok(());
        }
    }

    /// OAuth2.0 授权码登录（免费 APPID 必用）
    ///
    /// 1. POST apia.coolkit.cn/v2/user/oauth/code：账号密码 -> 授权码 code（30s 有效）
    /// 2. POST {region}/v2/user/oauth/token：code -> accessToken/refreshToken
    /// 3. GET /v2/family：用户 apikey（OAuth 响应不含 apikey）
    pub async fn login_oauth(&mut self) -> Result<()> {
        // 1) 账号密码 -> 授权码
        let seq = sequence();
        let sign = self.oauth_signature(&seq);
        let headers = self.oauth_headers(&seq, &sign);

        let mut body = serde_json::Map::new();
        body.insert("clientId".into(), Value::String(self.config.appid.clone()));
        body.insert("password".into(), Value::String(self.config.password.clone()));
        body.insert(
            "redirectUrl".into(),
            Value::String(self.config.redirect_url.clone()),
        );
        body.insert("grantType".into(), Value::String("authorization_code".into()));
        body.insert("state".into(), Value::String("rust-cli".into()));
        body.insert("nonce".into(), Value::String(nonce()));
        body.insert("seq".into(), Value::String(seq));
        body.insert("authorization".into(), Value::String(format!("Sign {sign}")));
        if self.config.account.contains('@') {
            body.insert("email".into(), Value::String(self.config.account.clone()));
        } else {
            body.insert(
                "phoneNumber".into(),
                Value::String(format!(
                    "{}{}",
                    self.config.country_code, self.config.account
                )),
            );
        }
        let body = Value::Object(body);

        let resp = self
            .http
            .post("https://apia.coolkit.cn/v2/user/oauth/code")
            .headers(headers)
            .body(body.to_string())
            .send()
            .await
            .with_context(|| "POST /v2/user/oauth/code")?;
        let text = resp.text().await?;
        let resp: ApiResponse<Value> =
            serde_json::from_str(&text).context("解析 oauth/code 响应")?;
        if resp.error != 0 {
            bail!("OAuth 授权失败: error={} msg={}", resp.error, resp.msg);
        }
        let code_data: OAuthCodeData = self.parse(resp, "oauth/code")?;
        eprintln!(
            "授权码已获取 (region={})，正在换取 access token...",
            code_data.region
        );

        // 2) code -> accessToken / refreshToken（标准 v2 签名）
        let token_body = json!({
            "code": code_data.code,
            "redirectUrl": self.config.redirect_url,
            "grantType": "authorization_code",
        });
        let token_resp = self
            .post_signed(&code_data.region, "/v2/user/oauth/token", &token_body)
            .await?;
        let token_data: OAuthTokenData = self.parse(token_resp, "oauth/token")?;
        self.at = Some(token_data.accessToken);
        self.rt = Some(token_data.refreshToken);
        self.at_exp = token_data.atExpiredTime;
        self.rt_exp = token_data.rtExpiredTime;
        self.config.region = code_data.region;

        // 3) 用户 apikey（OAuth 响应不含，从家庭列表取）
        let fam_resp = self.get_authed("/v2/family", "lang=en").await?;
        let fam: FamilyData = self.parse(fam_resp, "获取家庭列表")?;
        self.apikey = Some(
            fam.familyList
                .first()
                .map(|f| f.apikey.clone())
                .context("family 列表为空，无法获取用户 apikey")?,
        );
        self.save_tokens();
        println!(
            "OAuth2.0 登录成功: region={} apikey={}",
            self.config.region,
            self.apikey.as_deref().unwrap_or("")
        );
        Ok(())
    }

    /// 用 Refresh Token 刷新 Access Token（AT 30 天 / RT 60 天）
    pub async fn refresh(&mut self) -> Result<()> {
        let rt = self.rt.clone().context("还没有 rt，请先 login")?;
        let body = json!({ "rt": rt });
        let resp = self.post_authed("/v2/user/refresh", &body).await?;
        let data: RefreshData = self.parse(resp, "刷新 token")?;
        self.at = Some(data.at);
        self.rt = Some(data.rt);
        self.at_exp = data.atExpiredTime;
        self.rt_exp = data.rtExpiredTime;
        self.save_tokens();
        Ok(())
    }

    /// 获取账号下所有设备（itemType 1=自己的设备 2=分享的设备）
    pub async fn list_devices(&self) -> Result<Vec<Device>> {
        let resp = self.get_authed("/v2/device/thing", "num=0").await?;
        let data: ThingListData = self.parse(resp, "获取设备列表")?;
        Ok(data
            .thingList
            .into_iter()
            .filter(|t| t.itemType == 1 || t.itemType == 2)
            .map(|t| t.itemData)
            .collect())
    }

    /// 开/关设备：自动适配单通道（{"switch":"on"}）与多通道排插（{"switches":[...]}）
    pub async fn set_switch(&self, deviceid: &str, on: bool) -> Result<Value> {
        // 探测设备参数结构：含 switches 数组视为多通道排插，全部通道一起控制
        let params = self.get_status(deviceid).await?;
        let params = if params.get("switches").is_some() {
            let n = params["switches"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(1);
            let switches: Vec<Value> = (0..n)
                .map(|i| json!({ "outlet": i, "switch": if on { "on" } else { "off" } }))
                .collect();
            json!({ "switches": switches })
        } else {
            json!({ "switch": if on { "on" } else { "off" } })
        };
        self.post_control(deviceid, params).await
    }

    /// 控制多通道排插的单个插位（outlet 从 0 开始）
    pub async fn set_outlet(&self, deviceid: &str, outlet: u8, on: bool) -> Result<Value> {
        let params = json!({
            "switches": [{ "outlet": outlet, "switch": if on { "on" } else { "off" } }]
        });
        self.post_control(deviceid, params).await
    }

    async fn post_control(&self, deviceid: &str, params: Value) -> Result<Value> {
        let body = json!({ "type": 1, "id": deviceid, "params": params });
        let resp = self.post_authed("/v2/device/thing/status", &body).await?;
        if resp.error != 0 {
            bail!(
                "控制失败: error={} msg={}（请确认设备在线、deviceid 正确）",
                resp.error,
                resp.msg
            );
        }
        Ok(resp.data.unwrap_or(Value::Null))
    }

    /// 设备原生 pulse（一体化：通电 width 毫秒后设备自动断电，无需轮询/宿主计时）。
    /// ms=Some(w) → 启用该插位 pulse（结束后回到 off）；ms=None → 关闭该插位 pulse。
    /// 多通道排插需整体提交 pulses 数组，其余插位原样保留。
    /// 注意：/v2/device/thing/update 对免费 APP 常无权限(403)，pulses 走控制端点 thing/status 下推。
    pub async fn set_pulse(&self, deviceid: &str, outlet: u8, ms: Option<u64>) -> Result<Value> {
        let params = self.get_status(deviceid).await?;
        let mut pulses: Vec<Value> = params
            .get("pulses")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        while pulses.len() <= outlet as usize {
            pulses.push(json!({ "outlet": pulses.len(), "pulse": "off", "switch": "off", "width": 0 }));
        }
        pulses[outlet as usize] = match ms {
            Some(w) => json!({ "outlet": outlet, "pulse": "on", "switch": "off", "width": w }),
            None => json!({ "outlet": outlet, "pulse": "off", "switch": "off", "width": 0 }),
        };
        self.post_control(deviceid, json!({ "pulses": pulses })).await
    }

    /// 全部插位配置原生 pulse：所有通道 width 毫秒后自动断电（无需查询现有配置，全覆盖写）。
    /// 通道数 n 由调用方缓存传入（首次查询后复用，避免每次控制多一次 status API）。
    pub async fn set_pulse_all(&self, deviceid: &str, ms: u64, n: u8) -> Result<Value> {
        let pulses: Vec<Value> = (0..n)
            .map(|i| json!({ "outlet": i, "pulse": "on", "switch": "off", "width": ms }))
            .collect();
        self.post_control(deviceid, json!({ "pulses": pulses })).await
    }

    /// 一次性触发（中间态优化）：pulses + switches 同包下发，一次 post_control 完成
    /// （若固件不支持组合包则需回退两步法；调用方通过返回值/实测判定）
    pub async fn pulse_all_combined(&self, deviceid: &str, ms: u64, n: u8) -> Result<Value> {
        let pulses: Vec<Value> = (0..n)
            .map(|i| json!({ "outlet": i, "pulse": "on", "switch": "off", "width": ms }))
            .collect();
        let switches: Vec<Value> = (0..n).map(|i| json!({ "outlet": i, "switch": "on" })).collect();
        self.post_control(deviceid, json!({ "pulses": pulses, "switches": switches }))
            .await
    }

    /// 查询设备全部状态（params 如 {"switch":"on","power":..}）
    pub async fn get_status(&self, deviceid: &str) -> Result<Value> {
        let resp = self
            .get_authed("/v2/device/thing/status", &format!("type=1&id={deviceid}"))
            .await?;
        let data: StatusData = self.parse(resp, "查询状态")?;
        Ok(data.params)
    }
}

/// 读取必填环境变量
fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("缺少环境变量 {name}"))
}

/// token 落盘路径（HOME/.ewelink-tokens.json）
pub fn token_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    format!("{home}/.ewelink-tokens.json")
}
