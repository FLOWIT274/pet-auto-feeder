//! 签名与随机工具：HMAC-SHA256 签名、nonce、时间戳
//!
//! 官方签名规则（《开发文档 - 签名规则》）：
//! 将 App Secret 作为 key，对请求内容做 HMAC-SHA256，结果 Base64 编码后放到
//! Authorization 头中，格式为 `Sign {值}`。
//! - GET 请求：对所有 query 参数按参数名排序后用 `&` 连接作为待签串
//! - POST 请求：以整个 request body 的 JSON 原文作为待签串

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256(secret, payload) 后 Base64 编码
pub fn make_sign(secret: &str, payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac 接受任意长度 key");
    mac.update(payload.as_bytes());
    let out = mac.finalize().into_bytes();
    BASE64.encode(out)
}

/// 8 位字母数字随机串，用于 `X-CK-Nonce` 请求头
pub fn nonce() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// 当前时间戳（秒），用于 WebSocket 握手 `ts` 字段
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时钟早于 1970")
        .as_secs()
}

/// 当前时间戳（毫秒）字符串，用于 `sequence` 字段
pub fn sequence() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时钟早于 1970")
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_matches_official_demo() {
        // 官方开发文档 Demo①：
        // appsecret=OdPuCZ4PkPPi0rVKRVcGmll2NM6vVk0c
        // body={"email":"1234@gmail.com","password":"12345678","countryCode":"+1"}
        // https://www.jokecamp.com/blog/examples-of-creating-base64-hashes-using-hmac-sha256-in-different-languages/
        let secret = "OdPuCZ4PkPPi0rVKRVcGmll2NM6vVk0c";
        let body = r#"{"email":"1234@gmail.com","password":"12345678","countryCode":"+1"}"#;
        let sign = make_sign(secret, body);
        // 期望值与官方文档「签名算法 Demo①」输出一致：ttZ/gluzqrafvGonjMD20p4//arW6KoZKbo1SOMEzCA=
        // 注意：字段顺序不同的 body 会产生不同签名，服务端按收到的原始字节计算，
        // 因此客户端必须用实际发送的 body 原文做签名。
        assert_eq!(sign, "ttZ/gluzqrafvGonjMD20p4//arW6KoZKbo1SOMEzCA=");
    }

    #[test]
    fn nonce_is_8_alnum() {
        let n = nonce();
        assert_eq!(n.len(), 8);
        assert!(n.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn sequence_is_millis() {
        assert!(sequence().parse::<u128>().unwrap() > 1_000_000_000_000);
    }
}