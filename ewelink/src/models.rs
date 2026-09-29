//! 与官方 v2 接口 JSON 对应的 serde 模型
//!
//! 字段名刻意保持官方 JSON 的 camelCase 原样（配合 serde 自动映射），
//! 部分字段当前示例未读取，但为完整描述协议而保留。

#![allow(non_snake_case)]
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

/// 所有接口的统一返回包装：`{ error, msg, data }`
#[derive(Debug, Deserialize)]
pub struct ApiResponse<T> {
    pub error: i64,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub data: Option<T>,
}

/// POST /v2/user/login 成功时的 data
#[derive(Debug, Deserialize)]
pub struct LoginData {
    pub user: User,
    pub at: String,
    pub rt: String,
    /// 用户所属区域 cn=中国区 as=亚洲区 us=美洲区 eu=欧洲区
    pub region: String,
    #[serde(default)]
    pub atExpiredTime: Option<i64>,
    #[serde(default)]
    pub rtExpiredTime: Option<i64>,
}

/// 登录/注册返回的 user 对象（user.apikey 即用户 ID，WS 握手与 update 会用到）
#[derive(Debug, Deserialize)]
pub struct User {
    pub apikey: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phoneNumber: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub countryCode: Option<String>,
}

/// GET /v2/device/thing 的 data
#[derive(Debug, Deserialize)]
pub struct ThingListData {
    #[serde(default)]
    pub thingList: Vec<ThingItem>,
    #[serde(default)]
    pub total: i64,
}

/// thingList 中的每一项
#[derive(Debug, Deserialize)]
pub struct ThingItem {
    /// 1=自己的设备 2=别人分享的设备 3=自己的群组
    pub itemType: i64,
    pub itemData: Device,
}

/// 设备（deviceList item 说明）
#[derive(Debug, Deserialize)]
pub struct Device {
    pub name: String,
    pub deviceid: String,
    /// 设备所属用户的 apikey
    pub apikey: String,
    /// 局域网 zeroconf 加密密钥（仅 thing 接口返回，devicelist 无此字段）
    #[serde(default)]
    pub devicekey: Option<String>,
    #[serde(default)]
    pub online: Option<bool>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub extra: Option<Extra>,
}

/// 设备 extra（含 uiid）
#[derive(Debug, Deserialize)]
pub struct Extra {
    #[serde(default)]
    pub uiid: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub brandName: Option<String>,
}

/// POST /v2/user/refresh 的 data
#[derive(Debug, Deserialize)]
pub struct RefreshData {
    pub at: String,
    pub rt: String,
    #[serde(default)]
    pub atExpiredTime: Option<i64>,
    #[serde(default)]
    pub rtExpiredTime: Option<i64>,
}

/// GET /v2/device/thing/status 的 data
#[derive(Debug, Deserialize)]
pub struct StatusData {
    pub params: Value,
}

// ---- OAuth2.0（免费 APPID 授权码流程）----

/// POST /v2/user/oauth/code 响应 data
#[derive(Debug, Deserialize)]
pub struct OAuthCodeData {
    pub code: String,
    pub region: String,
}

/// POST /v2/user/oauth/token 响应 data
#[derive(Debug, Deserialize)]
pub struct OAuthTokenData {
    pub accessToken: String,
    pub refreshToken: String,
    #[serde(default)]
    pub atExpiredTime: Option<i64>,
    #[serde(default)]
    pub rtExpiredTime: Option<i64>,
}

/// GET /v2/family 响应 data（OAuth 登录后从这里拿用户 apikey）
#[derive(Debug, Deserialize)]
pub struct FamilyData {
    #[serde(default)]
    pub familyList: Vec<Family>,
}

#[derive(Debug, Deserialize)]
pub struct Family {
    pub id: String,
    pub apikey: String,
    #[serde(default)]
    pub name: String,
}